#!/usr/bin/env python3
"""Advisories whose fix is still inside the cooldown: a warning, not a failure.

The cooldown (scripts/check-cooldown.py) keeps any crate published less than
COOLDOWN_HOURS ago (default 48) out of Cargo.lock; only our own crates
(open-chess-*) are exempt. So when an advisory lands with a fix released
today, we can't take the fix yet, and failing CI over it would leave every
branch red until it can. This runs `cargo deny check advisories`, and for
each advisory against a locked crate looks for a release that satisfies the
advisory's fix and is past the cooldown (and not yanked):

  * one exists: nothing changes; cargo deny and cargo audit fail on it as
    usual, and the fix is a `cargo update -p <crate>` away.
  * none yet: the advisory is ignored for this run, with a warning saying
    when its fix comes out of the cooldown. An advisory with no fix at all
    still fails.

Writes deny.ci.toml (deny.toml plus those ignores) for cargo deny, and one
advisory id per line to advisory-ignores.txt for cargo audit's --ignore.

Usage:
  scripts/advisory-cooldown.py [--hours 48]
"""

from __future__ import annotations

import argparse
import datetime as dt
import importlib.util
import json
import os
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("check_cooldown", HERE / "check-cooldown.py")
cc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cc)


def _cmp(have: tuple, op: str, want: tuple) -> bool:
    n = max(len(have), len(want))
    a, b = have + (0,) * (n - len(have)), want + (0,) * (n - len(want))
    return {
        ">=": a >= b,
        ">": a > b,
        "<=": a <= b,
        "<": a < b,
        "=": a == b,
        "==": a == b,
    }[op]


def satisfies(num: str, req: str) -> bool:
    """`>=0.23.46, <0.24 OR >=0.22.9, <0.23` style (RustSec's patched list)."""
    have = cc.semver_key(num)
    for clause in re.split(r"\s+OR\s+", req.strip(), flags=re.I):
        parts = [p.strip() for p in clause.split(",") if p.strip()]
        ok = True
        for p in parts:
            m = re.match(r"^(>=|<=|==|>|<|=|\^)?\s*([0-9][0-9A-Za-z.+-]*)$", p)
            if not m:
                return False
            op, ver = m.group(1) or "^", cc.semver_key(m.group(2))
            if op == "^":
                # Caret: same leftmost non-zero part, and at least ver.
                lead = next((i for i, x in enumerate(ver) if x != 0), len(ver) - 1)
                upper = ver[: lead] + (ver[lead] + 1,)
                ok = ok and _cmp(have, ">=", ver) and _cmp(have, "<", upper)
            else:
                ok = ok and _cmp(have, op, ver)
        if ok:
            return True
    return False


def fix_requirement(notes: list[str]) -> str | None:
    for n in notes:
        m = re.match(r"^Solution: Upgrade to (.+?)(?: \(try `cargo update.*)?$", n.strip())
        if m:
            return m.group(1)
    return None


def advisories() -> list[dict]:
    out = subprocess.run(
        ["cargo", "deny", "--format", "json", "check", "advisories"],
        capture_output=True,
        text=True,
    )
    found = []
    for line in (out.stdout + out.stderr).splitlines():
        try:
            d = json.loads(line)
        except ValueError:
            continue
        f = d.get("fields") or {}
        adv = f.get("advisory")
        if d.get("type") != "diagnostic" or not adv or f.get("severity") != "error":
            continue
        krate = f["graphs"][0]["Krate"]
        found.append(
            {
                "id": adv["id"],
                "crate": krate["name"],
                "version": krate["version"],
                "fix": fix_requirement(f.get("notes") or []),
            }
        )
    return found


def with_ignores(deny: str, ignores: list[tuple[str, str]]) -> str:
    entries = "".join(f'    {{ id = "{i}", reason = "{why}" }},\n' for i, why in ignores)
    m = re.search(r"(?m)^ignore\s*=\s*\[\s*\n", deny)
    if m:
        return deny[: m.end()] + entries + deny[m.end() :]
    m = re.search(r"(?m)^\[advisories\]\s*\n", deny)
    if not m:
        return deny + "\n[advisories]\nignore = [\n" + entries + "]\n"
    return deny[: m.end()] + "ignore = [\n" + entries + "]\n" + deny[m.end() :]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--hours", type=float, default=float(os.environ.get("COOLDOWN_HOURS", 48)))
    ap.add_argument("--deny", type=Path, default=Path("deny.toml"))
    ap.add_argument("--out", type=Path, default=Path("deny.ci.toml"))
    ap.add_argument("--ids", type=Path, default=Path("advisory-ignores.txt"))
    args = ap.parse_args()

    now = dt.datetime.now(dt.timezone.utc)
    cutoff = now - dt.timedelta(hours=args.hours)
    ignores: list[tuple[str, str]] = []
    seen: set[str] = set()
    for a in advisories():
        if a["id"] in seen:
            continue
        seen.add(a["id"])
        name = a["crate"]
        if not a["fix"]:
            print(f"✗ {a['id']} ({name} {a['version']}): no fixed release; fails as usual")
            continue
        fixes = [
            v
            for v in cc.versions(name)
            if not v["yanked"] and "-" not in v["num"].split("+")[0] and satisfies(v["num"], a["fix"])
        ]
        times = [dt.datetime.fromisoformat(v["created_at"].replace("Z", "+00:00")) for v in fixes]
        if any(t <= cutoff for t in times):
            print(f"✗ {a['id']} ({name} {a['version']}): a fix ({a['fix']}) is past the cooldown; take it")
            continue
        if not times:
            print(f"✗ {a['id']} ({name} {a['version']}): no release on crates.io matches {a['fix']}; fails as usual")
            continue
        ready = min(times) + dt.timedelta(hours=args.hours)
        msg = (
            f"{a['id']} ({name} {a['version']}): its fix ({a['fix']}) is inside the {args.hours:g}h cooldown, "
            f"ignored until {ready:%Y-%m-%d %H:%M} UTC; then cargo update -p {name}"
        )
        print(f"::warning::{msg}" if os.environ.get("GITHUB_ACTIONS") else f"! {msg}")
        ignores.append((a["id"], f"fix inside the cooldown until {ready:%Y-%m-%d %H:%M} UTC"))

    args.out.write_text(with_ignores(args.deny.read_text(), ignores))
    args.ids.write_text("".join(f"{i}\n" for i, _ in ignores))
    print(f"{len(ignores)} advisor{'y' if len(ignores) == 1 else 'ies'} waiting on the cooldown; wrote {args.out}, {args.ids}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
