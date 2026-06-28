#!/usr/bin/env python3
"""Re-sync and verify the updater signatures in a Tauri ``latest.json``.

For each platform entry, the signature is taken from the matching uploaded
``<artifact>.sig`` asset — the authoritative signature for the *actually
uploaded* binary — and then verified against that binary with ``minisign``.

This closes the class of bug where a re-fired or partially-failed release
leaves ``latest.json`` carrying a stale signature from an earlier build. Seen
on v0.6.7: the macOS binary was rebuilt on the tag re-fire (non-reproducible —
codesign + notarization embed timestamps) but ``latest.json`` kept the failed
run's signature, so every macOS client got "signature verification failed".
Windows/Linux were fine because those builds are reproducible (identical bytes
across runs → the old signature still matched).

Run by the release workflow's ``finalize`` job as a hard gate, and usable by
hand to repair a botched release (no signing key required — it only copies a
signature the build already produced).

Usage:
    sync_latest_json.py <latest.json> <assets_dir> <minisign_pubkey> [--verify-only]

``<assets_dir>`` must contain, for every platform URL's basename ``B`` found in
``latest.json``, both ``B`` (the artifact) and ``B.sig`` (its base64-wrapped
minisign signature, exactly as tauri-action uploads it).

``<minisign_pubkey>`` is the bare key line (``RW...``) — i.e. the second line of
the decoded ``plugins.updater.pubkey`` from ``src-tauri/tauri.conf.json``.

Rewrites ``latest.json`` in place with re-synced signatures (unless
``--verify-only``) and exits non-zero if any artifact or ``.sig`` is missing or
any signature fails to verify.
"""

import argparse
import base64
import json
import subprocess
import sys
import tempfile
from pathlib import Path


def verify_one(artifact: Path, signature_b64: str, pubkey: str) -> tuple[bool, str]:
    """Verify a base64-wrapped minisign signature against ``artifact``.

    ``signature_b64`` is the value stored in latest.json / the ``.sig`` asset:
    base64 of the raw minisign signature file. minisign handles both the legacy
    (``Ed``) and prehashed (``ED``) algorithms transparently.
    """
    try:
        raw = base64.b64decode(signature_b64)
    except Exception as exc:  # noqa: BLE001 - report and fail, never raise
        return False, f"signature is not valid base64: {exc}"
    with tempfile.NamedTemporaryFile(suffix=".minisig", delete=False) as tf:
        tf.write(raw)
        sig_path = tf.name
    try:
        proc = subprocess.run(
            ["minisign", "-V", "-m", str(artifact), "-P", pubkey, "-x", sig_path],
            capture_output=True,
            text=True,
        )
    except FileNotFoundError:
        return False, "minisign not found on PATH (install it before verifying)"
    finally:
        Path(sig_path).unlink(missing_ok=True)
    if proc.returncode == 0:
        return True, "ok"
    return False, (proc.stdout + proc.stderr).strip() or "minisign reported failure"


def main() -> int:
    ap = argparse.ArgumentParser(description="Re-sync + verify latest.json updater signatures.")
    ap.add_argument("latest_json", type=Path, help="path to latest.json")
    ap.add_argument("assets_dir", type=Path, help="dir holding the artifacts + their .sig files")
    ap.add_argument("pubkey", help="minisign public key line (RW...)")
    ap.add_argument("--verify-only", action="store_true", help="do not rewrite; only verify")
    args = ap.parse_args()

    data = json.loads(args.latest_json.read_text(encoding="utf-8"))
    platforms = data.get("platforms") or {}
    if not platforms:
        print("error: latest.json has no platform entries", file=sys.stderr)
        return 1

    resynced = 0
    verified = 0
    problems: list[str] = []

    for key, entry in platforms.items():
        base = (entry.get("url") or "").rsplit("/", 1)[-1]
        if not base:
            problems.append(f"{key}: entry has no url")
            continue
        artifact = args.assets_dir / base
        sigfile = args.assets_dir / (base + ".sig")
        if not artifact.is_file():
            problems.append(f"{key}: missing artifact {base}")
            continue
        if not sigfile.is_file():
            problems.append(f"{key}: missing signature asset {base}.sig")
            continue

        # The .sig asset is the authoritative signature for this exact binary.
        authoritative = sigfile.read_text(encoding="utf-8").strip()
        if not args.verify_only and entry.get("signature") != authoritative:
            entry["signature"] = authoritative
            resynced += 1
            print(f"resynced: {key} <- {base}.sig")

        check_sig = authoritative if not args.verify_only else (entry.get("signature") or "")
        ok, detail = verify_one(artifact, check_sig, args.pubkey)
        if ok:
            verified += 1
            print(f"verified: {key} ({base})")
        else:
            problems.append(f"{key}: signature INVALID for {base} — {detail}")

    if not args.verify_only:
        args.latest_json.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")

    print(f"\nsummary: {resynced} resynced, {verified}/{len(platforms)} verified")
    if problems:
        print("\nPROBLEMS:", file=sys.stderr)
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        return 1
    print("all updater signatures present and valid ✓")
    return 0


if __name__ == "__main__":
    sys.exit(main())
