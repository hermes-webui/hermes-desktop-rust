# Hermes WebUI Desktop — Windows test build notes

Thanks for testing! This is an early cross-compiled prototype of the Windows desktop
shell for [hermes-webui](https://github.com/nesquena/hermes-webui). It wraps the web
UI in a native window (Microsoft Edge WebView2) and can connect either to a local
server or to a remote one over an SSH tunnel.

## Install

1. Run `Hermes WebUI Desktop_0.1.0_x64-setup.exe`.
2. **SmartScreen warning is expected** (the build is unsigned for now): click
   **More info → Run anyway**.
3. If you're on Windows 10 and don't have WebView2 yet, the installer fetches it
   automatically (~2 min, one time). Windows 11 already has it.

## Point it at a server

The app needs a running hermes-webui. Three setups:

| Your setup | What to do |
|---|---|
| hermes-webui running on this PC (incl. **WSL2**) | Nothing — the app opens `http://localhost:8787` by default. WSL2's localhost forwarding usually just works; if not, press `Ctrl+,` and set Target URL to the WSL IP (`hostname -I` inside WSL) |
| hermes-webui on a remote server | Press `Ctrl+,` → switch Mode to **SSH Tunnel** → enter username + host + ports → Save & Reconnect. **Key-based SSH auth is required** (no password prompts): make sure `ssh user@host` works from PowerShell without typing a password first (`ssh-agent` + `ssh-add`) |
| No server anywhere | The app will show the "Can't reach Hermes" window — that's working as intended; it retries automatically once a server appears on the target URL |

## Shortcuts

`Ctrl+,` preferences · `Ctrl+N` new window · `Ctrl+R` reload · `Ctrl+F` find in page
(`Enter`/`Shift+Enter` next/prev, `Esc` closes) · `Ctrl+=` / `Ctrl+-` / `Ctrl+0` zoom ·
`Ctrl+Shift+H` summon the window from anywhere · paste screenshots straight into the
chat composer with `Ctrl+V`.

## Known gaps in this build (don't file these)

- No tab strip yet on Windows — `Ctrl+T`/`Ctrl+N` both open a separate window (the
  tabbed UI is a later milestone; macOS uses native tabs already).
- Unsigned binary (SmartScreen, some AVs may grumble).
- No auto-update; new builds are shared manually.
- Closing the last window quits the app (by design on Windows).
- Toast notifications may not appear on a portable/unsigned install.

## What feedback helps most

1. Does it connect — localhost, WSL2, and especially **SSH tunnel to a real server**?
2. Any console windows flashing when the tunnel starts/reconnects? (There shouldn't be.)
3. Streaming chat, file upload, voice input, image paste — anything broken vs. your browser?
4. Rough edges in scaling/DPI, dark/light theme matching, window behavior.

Logs (attach to any bug report):
`%LOCALAPPDATA%\ai.get-hermes.HermesWebUIDesktop\logs\hermes-webui-desktop.log`
