#!/usr/bin/env python3
"""A version's section of CHANGELOG.md.

  scripts/changelog.py 0.1.6     prints that section's body (the release
                                 notes); fails if there is none, or if it's
                                 empty, so a release can't go out without
                                 saying what changed.

Sections are `## <version> — <date>` (or `## Unreleased`), newest first.
Stdlib only.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path


def section(text: str, version: str) -> str | None:
    lines = text.splitlines()
    head = re.compile(rf"^## {re.escape(version)}(\s|$)")
    for i, line in enumerate(lines):
        if head.match(line):
            body = []
            for rest in lines[i + 1 :]:
                if rest.startswith("## "):
                    break
                body.append(rest)
            return "\n".join(body).strip()
    return None


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    version = sys.argv[1].removeprefix("v")
    path = Path(__file__).resolve().parent.parent / "CHANGELOG.md"
    body = section(path.read_text(), version)
    if body is None:
        print(
            f"CHANGELOG.md has no section for {version}: rename `## Unreleased` to `## {version} — <date>`",
            file=sys.stderr,
        )
        return 1
    if not body:
        print(f"CHANGELOG.md's section for {version} is empty", file=sys.stderr)
        return 1
    print(body)
    return 0


if __name__ == "__main__":
    sys.exit(main())
