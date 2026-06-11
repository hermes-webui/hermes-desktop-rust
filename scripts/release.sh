#!/bin/bash
# Release a new version: push main, then push the tag as a SEPARATE operation
# so the Build and Release workflow reliably fires.
#
# Background (inherited from hermes-swift-mac): when main and a new tag are
# pushed in a single `git push` invocation, GitHub sometimes delivers only one
# of the two push events and the release workflow never fires (their v1.0.5).
# Pushing the tag in its own operation avoids that.
#
# Usage: scripts/release.sh v0.2.0

set -euo pipefail

VERSION="${1:-}"
if [[ -z "$VERSION" ]]; then
    echo "usage: $0 vX.Y.Z" >&2
    exit 1
fi
if [[ ! "$VERSION" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "error: version must look like vX.Y.Z (got '$VERSION')" >&2
    exit 1
fi
BARE="${VERSION#v}"

BRANCH=$(git rev-parse --abbrev-ref HEAD)
if [[ "$BRANCH" != "main" ]]; then
    echo "error: release must be cut from main (currently on '$BRANCH')" >&2
    exit 1
fi

if [[ -n "$(git status --porcelain)" ]]; then
    echo "error: working tree not clean" >&2
    exit 1
fi

# Version parity: tag must match tauri.conf.json and Cargo.toml.
CONF_VERSION=$(python3 -c "import json; print(json.load(open('src-tauri/tauri.conf.json'))['version'])")
CARGO_VERSION=$(grep -m1 '^version' src-tauri/Cargo.toml | sed 's/.*"\(.*\)"/\1/')
if [[ "$CONF_VERSION" != "$BARE" || "$CARGO_VERSION" != "$BARE" ]]; then
    echo "error: version mismatch — tag $VERSION vs tauri.conf.json $CONF_VERSION vs Cargo.toml $CARGO_VERSION" >&2
    exit 1
fi

# CHANGELOG entry required.
python3 scripts/extract_changelog.py "$VERSION" > /dev/null \
    || { echo "error: CHANGELOG.md has no section for $VERSION" >&2; exit 1; }

echo "Running tests…"
(cd src-tauri && cargo test --quiet)

echo "Pushing main…"
git push origin main

echo "Tagging and pushing $VERSION (separate operation)…"
git tag "$VERSION"
git push origin "$VERSION"

# Pre-create the draft release with the maintainer's token. The org's Actions
# policy blocks the workflow token from CREATING releases (even with
# Contents:write — see RELEASING.md troubleshooting); tauri-action finds this
# draft by tag and only uploads assets into it, which works fine.
echo "Creating draft release…"
NOTES_FILE=$(mktemp)
python3 scripts/extract_changelog.py "$VERSION" > "$NOTES_FILE"
gh release create "$VERSION" --repo hermes-webui/hermes-desktop-rust --draft \
    --title "Hermes WebUI Desktop $VERSION" --notes-file "$NOTES_FILE" \
    || echo "note: draft may already exist — CI will reuse it"
rm -f "$NOTES_FILE"

echo "Done. Watch the run: gh run watch --repo hermes-webui/hermes-desktop-rust"
echo "The release is a DRAFT — smoke-test the artifacts, then publish."
