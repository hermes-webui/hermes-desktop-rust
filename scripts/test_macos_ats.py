#!/usr/bin/env python3
"""Regression check: macOS WKWebView must be allowed to load HTTP WebUI targets."""
from pathlib import Path
import plistlib

plist_path = Path(__file__).resolve().parents[1] / "src-tauri" / "Info.plist"
if not plist_path.exists():
    raise SystemExit(f"FAIL: missing {plist_path}")
with plist_path.open("rb") as fh:
    data = plistlib.load(fh)
ats = data.get("NSAppTransportSecurity", {})
if ats.get("NSAllowsArbitraryLoadsInWebContent") is not True:
    raise SystemExit("FAIL: NSAllowsArbitraryLoadsInWebContent must be true")
print("PASS: macOS WKWebView HTTP loads are enabled")
