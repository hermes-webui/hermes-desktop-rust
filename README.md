# Hermes WebUI Desktop

The cross-platform desktop app for [hermes-webui](https://github.com/nesquena/hermes-webui) — run your Hermes agent's web UI as a real desktop app on **Windows, Linux, and macOS**, with one-time SSH tunnel setup for remote servers, native tabs, theme-matched window chrome, clipboard image paste, and background-response notifications.

<img width="1466" height="920" alt="image" src="https://github.com/user-attachments/assets/a7b81abc-f053-4e11-b2a4-ca3d4d8d72ea" />

Built with [Tauri 2](https://tauri.app) (system webview + Rust — ~11 MB, no bundled browser). It is the cross-platform sibling of
[hermes-swift-mac](https://github.com/hermes-webui/hermes-swift-mac), rebuilt 1:1 from its architecture (see [issue #555](https://github.com/nesquena/hermes-webui/issues/555)).

On macOS, hermes-swift-mac remains the flagship app; this build exists for Windows and Linux users and for anyone who wants identical behavior across all three platforms.

## Install

Download the latest build from **[Releases](../../releases)**:

| Platform | File | First-run note |
|---|---|---|
| **Windows 10/11** (x64) | `…_win_x64-setup.exe` | Unsigned for now → SmartScreen will warn: click **More info → Run anyway**. WebView2 installs automatically on Win10 if missing. (`…_win_x64_en-US.msi` also available for IT installs) |
| **Windows portable** (x64) | `…_win_x64_portable.zip` | Unzip anywhere and run — no installer, no admin rights. Keep `portable.txt` next to the exe (it keeps the app in portable mode). Needs the WebView2 runtime (preinstalled on Win11); updates are manual by design |
| **Linux** (x86_64) | `…_lin_x86_64.AppImage` | `chmod +x` and run — no install needed. Requires WebKitGTK 4.1 (preinstalled on Ubuntu 22.04+/Debian 12+/Fedora 38+ desktops). A `…_lin_x86_64.deb` is also available |
| **macOS 12+** (universal) | `…_macos_universal.dmg` | Signed and notarized — opens clean, no Gatekeeper warnings |

## Connect it to your Hermes

The app is a shell — it needs a running
[hermes-webui](https://github.com/nesquena/hermes-webui) server. Three setups:

**1. Local server (same machine, including WSL2 on Windows)**
Start hermes-webui (`./start.sh`), launch the app — it opens
`http://localhost:8787` by default. Nothing to configure. WSL2's localhost
forwarding works out of the box in most setups; if it doesn't, set the Target URL (Preferences, `Cmd/Ctrl+,`) to the WSL IP (`hostname -I` inside WSL), or enable [mirrored networking](https://learn.microsoft.com/en-us/windows/wsl/networking#mirrored-mode-networking).

**2. Remote server over SSH (the headline feature)**
Preferences → Mode: **SSH Tunnel** → enter username, host, and ports → Save &
Reconnect. The app maintains `ssh -N -L 8787:127.0.0.1:8787 user@host` for you, shows live tunnel status in a footer, reconnects on demand, and verifies the forward with a real HTTP round-trip. **Key-based SSH auth is required** (there's no terminal for password prompts) — make sure `ssh user@host` works without typing a password first (`ssh-add` your key; on Windows enable the `ssh-agent` service).

**3. Anything else that serves hermes-webui over http(s)**
Tailscale IP, reverse proxy, custom port — set it as the Target URL in Direct mode.

## Features

- **Two connection modes** — Direct (with `/health` monitoring and a ●/○ health indicator) and SSH Tunnel (status footer, reconnect button, actionable error hints parsed from ssh itself).
- **Tabs & windows** — native macOS tab groups (`Cmd+T`, drag to reorder/detach); a browser-style tab bar on Windows/Linux (`Ctrl+T`, `Ctrl+Tab` cycling, middle-click close, ＋ and ⋯ controls); multi-window everywhere (`Cmd/Ctrl+N`). Tab titles follow your conversation names; every tab is a live session view, so switching tabs never interrupts a streaming response.
- **Theme-matched chrome** — the window borders/titlebar/tab bar follow the web UI's theme (all 11+ hermes-webui skins), with a cached color so every launch and new tab paints correctly from the first frame. No white flashes.
- **Paste images** — `Cmd/Ctrl+V` a screenshot straight into the chat composer.
- **Background notifications** — get pinged when a long response finishes while you're in another app.
- **Find in page** — `Cmd/Ctrl+F`, `Enter`/`Shift+Enter`, `Esc`.
- **Summon hotkey** — `Cmd/Ctrl+Shift+H` brings Hermes to front from anywhere.
- **Safe navigation** — only your Hermes origin loads inside the app; external links open in your browser; `file://` is blocked.
- **Auto-update** — the app checks GitHub Releases shortly after launch and on "Check for Updates…" (app menu on macOS, ⋯ menu on Windows/Linux), verifies the download against a pinned signing key, installs, and relaunches. Covers the Windows installer builds, the Linux AppImage, and macOS; `.deb` installs are notified to grab the new package manually.
- **Zoom** (`Cmd/Ctrl` `+`/`−`/`0`, persisted), window frame/fullscreen restore, single-instance.

## Configuration

Preferences (`Cmd/Ctrl+,`) — same semantics as the Swift app:

| Setting | Default | Notes |
|---|---|---|
| Mode | Direct (Local) | Direct ↔ SSH Tunnel |
| Target URL | `http://localhost:8787` | Any http(s) URL; the page the app loads |
| SSH Username / Host | `hermes` / — | SSH mode only |
| Local / Remote port | `8787` / `8787` | SSH mode only; remote side always forwards to `127.0.0.1` |
| Notifications | on | "response ready" notification when backgrounded |

Settings file: `~/Library/Application Support/ai.get-hermes.HermesWebUIDesktop/prefs.json`
(macOS) · `%APPDATA%\ai.get-hermes.HermesWebUIDesktop\prefs.json` (Windows) ·
`~/.config/ai.get-hermes.HermesWebUIDesktop/prefs.json` (Linux).

Logs (attach to bug reports): `~/Library/Logs/ai.get-hermes.HermesWebUIDesktop/` (macOS)
· `%LOCALAPPDATA%\ai.get-hermes.HermesWebUIDesktop\logs\` (Windows) ·
`~/.local/share/ai.get-hermes.HermesWebUIDesktop/logs/` (Linux).

## Troubleshooting

- **"Can't reach Hermes" on launch** — is hermes-webui running? `curl
  http://localhost:8787/health` should answer. The app retries automatically every 5 s while the error screen is up.
- **SSH tunnel won't connect** — the error window shows the actual ssh failure (auth, host key, DNS…). Test the exact same thing in a terminal first: `ssh -N -L 8787:127.0.0.1:8787 user@host`, then `curl http://127.0.0.1:8787/health`.
- **WSL2: localhost doesn't reach the server** — see setup note 1 above.
- **Linux: blank/black window** — usually WebKitGTK + NVIDIA proprietary drivers; launch with `WEBKIT_DISABLE_DMABUF_RENDERER=1 ./Hermes*.AppImage`.
- **Linux: global hotkey does nothing on Wayland** — Wayland doesn't allow global key grabs; bind a compositor shortcut to launching the app instead (the single-instance guard focuses the running window).

## Build from source

Requirements: Rust 1.88+, Node 18+ (for the Tauri CLI), and on Linux
`libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev patchelf`.

```bash
git clone git@github.com:hermes-webui/hermes-desktop-rust.git
cd hermes-desktop-rust
npm install
npx tauri dev      # run against a local hermes-webui
npx tauri build    # platform installer in src-tauri/target/release/bundle/
cd src-tauri && cargo test
```

## Architecture (one paragraph)

A small Rust core owns a connection state machine (splash → preflight/tunnel → window | error), an ssh child process with HTTP-probe readiness checking and a 10s liveness monitor, and a JSON prefs store. The web UI loads in the system webview with a set of injected scripts that bridge the two worlds: a theme reporter (drives native chrome appearance), a response-settled detector (drives notifications), a synthetic paste/drop injector (clipboard images), a find bar, and a navigation/title pipeline. Remote page content is capability-restricted to event emission only — it can never call into app commands. Releases are built by GitHub Actions on tags (`scripts/release.sh`).

## License

[MIT](LICENSE)
