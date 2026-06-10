#!/bin/bash
# Bump the app version across every file that must agree, in one command,
# and seed a CHANGELOG section for the release.
#
# Usage: scripts/bump_version.sh 0.2.0     (no leading "v")
# Then:  edit CHANGELOG.md → commit → scripts/release.sh v0.2.0

set -euo pipefail

V="${1:-}"
if [[ ! "$V" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "usage: $0 X.Y.Z (no leading v)" >&2
    exit 1
fi
cd "$(dirname "$0")/.."

python3 - "$V" <<'PYEOF'
import datetime
import json
import re
import sys

v = sys.argv[1]

# tauri.conf.json
path = "src-tauri/tauri.conf.json"
conf = json.load(open(path))
conf["version"] = v
with open(path, "w") as f:
    json.dump(conf, f, indent=2, ensure_ascii=False)
    f.write("\n")

# package.json
path = "package.json"
pkg = json.load(open(path))
pkg["version"] = v
with open(path, "w") as f:
    json.dump(pkg, f, indent=2, ensure_ascii=False)
    f.write("\n")

# Cargo.toml — first `version = "..."` line is the package version
path = "src-tauri/Cargo.toml"
text = open(path).read()
text = re.sub(r'^version = ".*"$', f'version = "{v}"', text, count=1, flags=re.MULTILINE)
open(path, "w").write(text)

# CHANGELOG.md — seed a section if absent
path = "CHANGELOG.md"
text = open(path).read()
if f"## [v{v}]" not in text:
    date = datetime.date.today().isoformat()
    marker = "# Changelog\n"
    section = f"\n## [v{v}] — {date}\n\n### Added\n\n- \n\n### Fixed\n\n- \n"
    open(path, "w").write(text.replace(marker, marker + section, 1))
    print(f"Seeded CHANGELOG.md section for v{v} — fill it in before releasing.")
PYEOF

# Refresh Cargo.lock with the new package version
(cd src-tauri && cargo check --quiet)

echo "Bumped to $V: src-tauri/tauri.conf.json, src-tauri/Cargo.toml (+Cargo.lock), package.json"
echo "Next: edit CHANGELOG.md → commit → scripts/release.sh v$V"
