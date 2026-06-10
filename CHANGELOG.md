# Changelog

## [v0.1.2] — 2026-06-10

### Fixed

- **Windows: the Preferences window opened blank and never rendered** (report:
  "prefs window didn't render" — the window showed only a blurred glimpse of
  whatever sat behind it). The `open_preferences` command created the webview
  window synchronously on the main thread; on Windows, WebView2 initialization
  needs the message loop to keep pumping, so a webview created from inside a
  blocking IPC command stalls forever and its window never paints — it just
  exposes the DWM backdrop. Every working window (splash, error, browser) was
  already being created from a background thread; the Preferences window (and the
  Ctrl+T / Ctrl+N new-tab/new-window paths, which had the same latent bug) now do
  the same. macOS never reproduced this because WKWebView doesn't depend on the
  Win32 message pump.

## [v0.1.1] — 2026-06-10

### Fixed

- **Windows/Linux: app exited immediately after the splash screen — nothing ever
  appeared** (first-run report: "showed a spinning loading thing and then
  disappeared"). The connection flow destroys the splash window an instant before
  it creates the browser or error window; during that window-less instant Tauri's
  default "all windows closed → exit" behavior killed the process, so neither the
  error screen (no server running) nor the main window (server running) ever
  appeared. macOS was unaffected only because it already suppressed that exit for
  its keep-running-in-Dock semantics. The exit request is now suppressed on every
  platform, and Windows/Linux "closing the last window quits the app" is
  implemented explicitly instead: the app exits only when a window is closed while
  the connection flow is idle and no browser, error, preferences, or splash window
  remains — user-initiated closes quit exactly as before, internal window churn
  never does.

## [v0.1.0] — 2026-06-10

First public build. Cross-platform (macOS / Windows / Linux) Tauri 2 desktop shell
for [hermes-webui](https://github.com/nesquena/hermes-webui), modeled 1:1 on
[hermes-swift-mac](https://github.com/hermes-webui/hermes-swift-mac)'s architecture:
a thin native shell that hosts the web UI, with SSH tunnel support and the same
settings surface.

### Added
- **Direct mode** — connects to a running hermes-webui (default
  `http://localhost:8787`): splash screen → HTTP preflight (any HTTP response counts
  as reachable; GET, never HEAD) → browser window. `/health` is polled every 30 s and
  surfaced as a `●`/`○` health dot in the window title and a dock badge on macOS.
- **SSH tunnel mode** — spawns the system `ssh` with the same arguments and lifecycle
  as the Swift app (`-N -o StrictHostKeyChecking=accept-new -o
  ExitOnForwardFailure=yes -L <local>:127.0.0.1:<remote> user@host`), verifies
  readiness with an end-to-end HTTP probe (a TCP connect is not proof the forward
  works), monitors liveness every 10 s, and tears down SIGTERM→SIGKILL on quit. A
  28 px status footer shows the gray/green/red state with a Reconnect button, and
  ssh stderr is parsed into actionable error hints (bad key, unknown host, refused…).
- **Preferences** (Cmd/Ctrl+,) — Direct/SSH mode toggle, SSH username/host,
  local/remote ports (validated 1–65535), target URL, notifications toggle, Test
  Connection. Save & Reconnect reuses live webviews on same-mode reconnects so
  in-flight chats survive; switching modes rebuilds windows.
- **Native macOS tabs** — Cmd+T opens a tab in the key window's native tab group
  (explicit `addTabbedWindow`, since AppKit's auto-tab heuristic is unreliable);
  Cmd+N opens a standalone window that can still later Merge All Windows. Tab-bar-
  aware layout pins the webview below the tab bar when it's visible and hides the
  web UI's redundant in-page titlebar; titles mirror the active conversation name
  (with the "— Hermes" suffix stripped, truncated at 40 chars).
- **Theme-matched chrome** — the page's effective background color drives the window
  appearance (titlebar/tab bar render dark for dark themes, light for light) via a
  bridge that prefers hermes-webui's `theme-color` meta tag and falls back to pixel
  sampling, with a 2.5 s stability gate against transient flashes and a 7-day cached
  color so every window opens in the right theme from the first frame. Theme/skin
  changes propagate across all open tabs.
- **Clipboard image paste** — Cmd+V with an image on the clipboard injects it into
  the chat composer using the 3-strategy synthetic paste/drop approach (WebKit's DOM
  image paste is unreliable); Windows uses Chromium's native paste.
- **Find in page** — Cmd/Ctrl+F bar with Enter/Shift+Enter next/previous, Esc to
  close (`window.find`-based, works on all three platforms).
- **Native notifications** — "Your response is ready" when a streamed response
  settles while the app is in the background (web Notification prompts are
  suppressed in favor of native ones; Web Speech is suppressed so voice input uses
  the webui's server-side transcription path).
- **Navigation guard** — only localhost/loopback and the configured target host load
  inside the app; `file://` is blocked; everything else (including `target=_blank`
  and `window.open`) opens in the system browser.
- **App behavior** — global summon hotkey (Cmd/Ctrl+Shift+H), zoom (Cmd/Ctrl +/−/0,
  persisted), single-instance guard, first-window frame + fullscreen persistence,
  error window with Retry + automatic recovery probing while the server is down,
  macOS Cmd+W hides the last window (app stays in the Dock), Windows/Linux closing
  the last window quits.
- **CI/release** — GitHub Actions test workflow (fmt, clippy, unit tests) and a
  tag-driven release workflow building macOS universal DMG, Windows NSIS installer
  (with WebView2 bootstrapper) + MSI, and Linux AppImage + .deb.

### Known gaps (tracked for upcoming releases)
- Windows/Linux ship multi-window only; the custom tab strip lands in a later
  milestone (macOS has native tabs today).
- No auto-updater yet; releases are manual downloads.
- Download links rely on the engine's default handling (test session export per
  platform); launch-at-login and a hotkey recorder UI are not wired yet.
- Builds are unsigned: macOS Gatekeeper and Windows SmartScreen each need a one-time
  bypass (see README install notes).
