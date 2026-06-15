# Changelog

## [v0.3.6]

### Added

- **Windows/Linux: tabs now badge when a session is waiting on you.** When a
  background tab raises a tool-approval or clarify/question popup, you used to
  see only a momentary flash and had to click through every tab to find the
  blocked one. Such tabs now show an amber attention dot (and a subtle tint) in
  the tab strip, so the session waiting for input is obvious at a glance. The
  signal is the WebUI's existing pending-prompt title marker, read per-tab
  through the native title hook — no extra polling. (#14, reported by b3nw.)

## [v0.3.5] — 2026-06-14

### Fixed

- **Windows/Linux: tabs were stuck on "New Tab" and never showed the active
  session (and titles could "reset" to `Hermes WebUI ● <host>`).** The custom
  tab strip derived each tab's title from an injected script that posted the
  page's `document.title` back over the JS event IPC (`window.__TAURI__`). That
  IPC isn't reliably available in remote-origin content webviews, so the title
  report silently no-op'd and the tab kept its placeholder. Titles are now read
  from the webview engine directly via a native title-changed hook
  (WebView2/WebKitGTK/WKWebView), independent of page JS, the IPC global, and
  the page's CSP — the same "don't depend on the remote webview's IPC" approach
  the link-opening fix used. (#15, reported by Deor and b3nw.)
- **A transient blank title no longer wipes a good tab title.** While a tab is
  mid-load (or briefly reports a separator-only title), the strip used to fall
  back to the `Hermes WebUI ● <host>` placeholder. A known-good title (or the
  initial "New Tab" seed) is now kept until a real title arrives.
- **Tab titles now strip the trailing name suffix for custom bot names and
  non-default profiles.** The WebUI appends a " — name" suffix where the name is
  the configured bot name or the profile name — not always "Hermes". The suffix
  is now removed generically (only the last separator-delimited segment, so an
  em-dash inside the session title itself is preserved) instead of matching the
  literal "Hermes".
- **Direct-mode connections to a non-localhost HTTP server now get the bridge
  again.** The content-webview capability listed `https://*` but no plain
  `http://*`, so for a Direct connection to e.g. `http://192.168.x.x:8787`
  every bridge emit (theme sync, the "response is ready" notification) was
  silently dropped. Plain-HTTP remote origins are now covered (still
  event-emit-only — no command access).

## [v0.3.4] — 2026-06-12

### Fixed

- **Clicking a link in chat did nothing instead of opening the browser.**
  The WebUI renders chat links with `target="_blank"`, and our injected script
  forwarded those clicks to native by emitting an `open-external` IPC event.
  That emit posts to `ipc.localhost`, which the remote page's own CSP
  `connect-src` blocks (the WebUI server governs the page's CSP, not the shell),
  so the event never reached Rust and the click was a no-op. External links
  (and `window.open`) now navigate the top frame instead: the native
  `on_navigation` hook — which is not subject to the page CSP — opens external
  hosts in the system browser and cancels the navigation so the page stays put,
  exactly as plain links already did. No server-side change required.
  (#12, reported by Deor; cross-ref hermes-webui#4040.)
- **White flash when opening a new tab (Windows/Linux).** A new tab is a child
  webview added to an already-visible window, so it painted white until its
  first paint — the window-level anti-flash (build hidden, reveal on load)
  couldn't cover it. New tab webviews now get an opaque native background in the
  cached theme color, so they come up theme-colored instead of white. (#4,
  reported by Rod.)

## [v0.3.3] — 2026-06-10

### Fixed

- **macOS: app froze ("crashed") every time a new tab was opened.** Cmd+T
  built the window fine, but the native tab attach (`addTabbedWindow`) ran
  inside the event loop's dispatch, where AppKit may force the other tab's
  window to redraw *synchronously* — and that redraw re-enters the windowing
  layer's non-reentrant lock, deadlocking the main thread on itself. The app
  froze instantly and forever (no crash report — it never crashes, it hangs;
  confirmed by macOS hang diagnostics on v0.3.1 and v0.3.2). The freeze only
  triggers when the two windows' sizes differ, which is why default-size dev
  windows masked it and real resized windows hit it 100% of the time. Tab
  attach (and the Cmd+N tabbing-mode dance) now runs via the GCD main queue —
  outside the event dispatch — and Cmd+T/Cmd+N window creation runs inline on
  the main thread on macOS, like the original prototype. Verified live: tab
  attach plus continued main-thread heartbeats on the previously-freezing
  setup.

## [v0.3.2] — 2026-06-10

### Fixed

- **Linux: intermittent crash at launch.** WebKitGTK's internal threads talk
  X11 directly; without Xlib's thread-safe mode they race the GTK main loop
  and the app could abort during startup (`[xcb] Unknown sequence number
  while awaiting reply`) or die silently with no window — roughly two out of
  three launches in the CI smoke harness, timing-dependent on real desktops.
  The app now calls `XInitThreads` before anything else on Linux. Wayland-only
  systems without libX11 are unaffected (the call is skipped). Found by the
  Linux smoke harness flaking on identical builds.

## [v0.3.1] — 2026-06-10

The first release that arrives **as an in-app update** for v0.3.0 users — and the
first **signed and notarized macOS build** (Developer ID + hardened runtime +
stapled notarization: no more right-click-to-open ritual; microphone and network
entitlements mirror hermes-swift-mac so voice input keeps working under the
hardened runtime).

### Added

- **Platform-labeled release artifacts** (tester request: "label the binaries to
  be clear about platform"): every asset now states its platform —
  `…_win_x64-setup.exe`, `…_lin_x86_64.AppImage`, `…_macos_universal.dmg`,
  and the formerly ambiguous `universal.app.tar.gz` is now
  `…_macos_universal.app.tar.gz`. The update manifest's URLs are rewritten to
  match automatically, while the release is still a draft.
- **Portable Windows build** (tester request: "be nice to have a non-installer
  .exe"): `…_win_x64_portable.zip` — unzip anywhere and run, no installer, no
  admin rights. The bundled `portable.txt` marker keeps the app in portable mode:
  self-update is disabled there (it would silently convert the portable copy into
  an installed app) and points at Releases instead. Requires the WebView2 runtime
  (preinstalled on Windows 11).
- **SSH tunnel auto-recovery** (Swift app NWPathMonitor parity): while the tunnel
  is down — laptop slept, Wi-Fi dropped, VPN flapped — the app probes the SSH
  host's port every 10 s and reconnects the moment it answers, with a blind retry
  every 60 s for `ssh_config`-mapped ports. No more manually clicking Reconnect
  after every sleep/wake.
- **Downloads on macOS and Linux**: session exports and other in-app downloads
  (which WKWebView/WebKitGTK silently drop) are intercepted, saved into
  ~/Downloads with collision-safe names, and announced with a notification.
  Windows keeps WebView2's native Save As dialog.
- **Reveal Log File** in the macOS app menu and the tab bar's ⋯ menu — opens the
  live log in Finder/Explorer/Files for bug reports.

### Fixed

- The update-check failure dialog now gives actionable guidance instead of a raw
  plugin error string.
- Linux stability hardening from the new CI smoke harness (which launches the
  real app on Ubuntu under Xvfb and screenshots it): window centering no longer
  relies on GTK's no-op `center()` for hidden windows, the tab strip's buttons
  use font-safe glyphs, and tab operations avoid re-fitting GTK child webview
  geometry — which crashes natively in Tauri's multi-webview on Linux. Known
  cosmetic limits on Linux for now: extra strip padding, and window resizes
  don't re-fit webview bounds (upstream wry/GTK work, tracked).

## [v0.3.0] — 2026-06-10

### Added

- **Auto-update** (the Sparkle-parity feature). The app now checks GitHub Releases
  about ten seconds after launch — silently unless an update actually exists — and
  on demand via **Check for Updates…** (app menu on macOS, the tab bar's ⋯ menu on
  Windows/Linux). When a new version is found you get a native "Install and
  Relaunch / Later" prompt; the download is verified against a signing key pinned
  in the app before anything is installed. Coverage: Windows installer builds
  (the installer runs and relaunches), Linux **AppImage** (replaced in place),
  and macOS (.app replaced in place). `.deb` installs can't self-update by design —
  the interactive check tells those users to grab the new package from Releases.
  Release builds now ship signed updater artifacts and a `latest.json` manifest;
  because releases are published from drafts, installed apps only ever see
  smoke-tested builds. **This release is the first carrying the manifest, so
  v0.3.0 is the last manual download — every release after this arrives
  in-app.**

## [v0.2.0] — 2026-06-10

### Added

- **Tabs on Windows and Linux** (tester report: "there's no indicator that there is
  a functionality for new tabs or windows"). Every window now has a browser-style
  tab bar: **＋** or `Ctrl+T` opens a tab, click to switch, hover **×** /
  middle-click / `Ctrl+W` closes, `Ctrl+Tab` / `Ctrl+Shift+Tab` cycle, and tab
  titles follow the active conversation (same "— Hermes"-stripping pipeline as the
  macOS native tabs). Each tab is its own live webview — switching tabs never
  reloads the page or interrupts a streaming response, matching what macOS gets
  from one-WKWebView-per-native-tab. Built on Tauri's multi-webview support: the
  window hosts a 38px shell strip plus one content webview per tab; the strip is a
  bundled page with full IPC while tab content keeps the event-emit-only
  capability. The connection status (tunnel state with Reconnect, or the direct
  health dot) moved from the injected page footer into the strip's status area.
- **A native ⋯ menu in the tab bar** (Windows/Linux) — the discoverability surface
  the first build lacked: New Tab, New Window, Reload, Find in Page, Zoom,
  Preferences, Open in Browser and Quit, each listed with its keyboard shortcut, as
  a real OS context menu.
- prefs.json is seeded with the full default settings schema on first launch
  (it previously showed `{}` until something was saved — Swift
  `seedDefaultsIfNeeded` parity).

### Fixed

- Windows/Linux: `Ctrl+T` previously opened the new window at the OS default
  position instead of cascading from the current one (superseded by real tabs, but
  the cascade also applies to `Ctrl+N` windows).

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
