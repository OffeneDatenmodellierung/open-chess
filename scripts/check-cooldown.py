#!/usr/bin/env python3
"""Supply-chain cooldown gate for Cargo.lock.

Fails if any crates.io package in Cargo.lock was published less than
COOLDOWN_HOURS ago (default 48), so brand-new releases get a cooling-off
period before we depend on them. Also reports direct dependencies that are
behind the newest *eligible* (non-yanked, non-prerelease, past cooldown)
release, and fails on those with --strict-latest. Our own crates
(open-chess-*) are exempt.

Usage:
  scripts/check-cooldown.py [--hours 48] [--strict-latest] [--lock Cargo.lock]

Stdlib only; talks to the crates.io API (sparse per-crate lookups, cached for
the run). Set CRATES_IO_UA to identify your CI to crates.io.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import sys
import tomllib
import urllib.request
from functools import lru_cache
from pathlib import Path

UA = os.environ.get("CRATES_IO_UA", "open-chess-cooldown-check (github.com/OffeneDatenmodellierung)")
CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
# Our own crates (open-chess, which we release ourselves): the cooldown
# guards against someone else's compromised release, so it doesn't apply.
OWN_PREFIXES = ("open-chess-",)


@lru_cache(maxsize=None)
def versions(name: str) -> list[dict]:
    req = urllib.request.Request(f"https://crates.io/api/v1/crates/{name}/versions", headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r)["versions"]


def published(name: str, version: str) -> dt.datetime | None:
    for v in versions(name):
        if v["num"] == version:
            return dt.datetime.fromisoformat(v["created_at"].replace("Z", "+00:00"))
    return None


def semver_key(num: str) -> tuple:
    core = num.split("+")[0].split("-")[0]
    return tuple(int(p) for p in core.split(".") if p.isdigit())


def latest_eligible(name: str, cutoff: dt.datetime) -> str | None:
    best = None
    for v in versions(name):
        num = v["num"]
        if v["yanked"] or "-" in num.split("+")[0]:
            continue
        if dt.datetime.fromisoformat(v["created_at"].replace("Z", "+00:00")) > cutoff:
            continue
        if best is None or semver_key(num) > semver_key(best):
            best = num
    return best


def direct_deps(manifest: Path) -> dict[str, str]:
    doc = tomllib.loads(manifest.read_text())
    out: dict[str, str] = {}
    for table in ("dependencies", "dev-dependencies", "build-dependencies"):
        for name, spec in doc.get(table, {}).items():
            req = spec if isinstance(spec, str) else spec.get("version", "")
            pkg = name if isinstance(spec, str) else spec.get("package", name)
            if req:
                out[pkg] = req
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--hours", type=float, default=float(os.environ.get("COOLDOWN_HOURS", 48)))
    ap.add_argument("--lock", type=Path, default=Path("Cargo.lock"))
    ap.add_argument("--manifest", type=Path, default=Path("Cargo.toml"))
    ap.add_argument("--strict-latest", action="store_true", help="fail when a direct dep is behind latest eligible")
    args = ap.parse_args()

    now = dt.datetime.now(dt.timezone.utc)
    cutoff = now - dt.timedelta(hours=args.hours)
    lock = tomllib.loads(args.lock.read_text())
    resolved: dict[str, list[str]] = {}
    too_new: list[str] = []

    for pkg in lock.get("package", []):
        if pkg.get("source") != CRATES_IO:
            continue
        name, ver = pkg["name"], pkg["version"]
        resolved.setdefault(name, []).append(ver)
        if name.startswith(OWN_PREFIXES):
            continue
        when = published(name, ver)
        if when is None:
            too_new.append(f"{name} {ver}: not found on crates.io")
        elif when > cutoff:
            age = (now - when).total_seconds() / 3600
            too_new.append(f"{name} {ver}: published {age:.1f}h ago")

    behind: list[str] = []
    for name, req in sorted(direct_deps(args.manifest).items()):
        if name.startswith(OWN_PREFIXES):
            continue
        latest = latest_eligible(name, cutoff)
        have = max(resolved.get(name, ["0"]), key=semver_key)
        if latest and semver_key(have) < semver_key(latest):
            behind.append(f"{name}: requires {req}, locked {have}, latest eligible {latest}")

    print(f"cooldown: {args.hours:g}h (cut-off {cutoff:%Y-%m-%d %H:%M} UTC), "
          f"{sum(len(v) for v in resolved.values())} crates.io packages checked")
    if too_new:
        print("\n✗ inside cooldown — pin to an older release (cargo update -p <name> --precise <ver>):")
        for line in too_new:
            print(f"  {line}")
    if behind:
        print("\n! direct dependencies behind latest eligible release:")
        for line in behind:
            print(f"  {line}")
    if not too_new and not behind:
        print("✓ all packages outside cooldown; direct dependencies current")

    return 1 if too_new or (args.strict_latest and behind) else 0


if __name__ == "__main__":
    sys.exit(main())
