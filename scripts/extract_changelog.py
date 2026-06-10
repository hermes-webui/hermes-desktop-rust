#!/usr/bin/env python3
"""Print the CHANGELOG.md section for a given version tag.

Usage: scripts/extract_changelog.py v0.1.0
Exits non-zero if the section is missing (callers fall back to a stub body).
"""

import re
import sys
from pathlib import Path


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: extract_changelog.py vX.Y.Z", file=sys.stderr)
        return 2
    version = sys.argv[1]
    changelog = Path(__file__).resolve().parent.parent / "CHANGELOG.md"
    text = changelog.read_text(encoding="utf-8")

    # Sections look like: ## [v0.1.0] — 2026-06-10
    pattern = re.compile(
        r"^## \[" + re.escape(version) + r"\][^\n]*\n(.*?)(?=^## \[|\Z)",
        re.DOTALL | re.MULTILINE,
    )
    match = pattern.search(text)
    if not match:
        print(f"error: no CHANGELOG section for {version}", file=sys.stderr)
        return 1
    print(match.group(1).strip())
    return 0


if __name__ == "__main__":
    sys.exit(main())
