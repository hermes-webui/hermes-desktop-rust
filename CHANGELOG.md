# Changelog

## [v0.6.3] — 2026-06-19

A bug-squash release: the Windows session-restore failures fixed at the real
root cause, plus universal per-tab profile color dots. (Rolls up the v0.6.2 tag,
which was never published — these changes ship together here.)

### Fixed

- **Windows: a restored tab now reopens on the right profile and its real
  session, instead of bouncing to the home screen with "Session not available in
  web UI."** (issues #30 and #37 — one root cause; #30 was marked fixed in v0.6.0
  and #37 in v0.6.1, but both kept happening on Windows). The profile selector
  (`hermes_profile`) is a *session* cookie, and WebView2 discards session cookies
  from a tab's on-disk data folder when the app restarts. So a restored tab —
  even though it reuses its data folder to keep you logged in — came back on the
  *default* profile, and its saved `/session/<id>` link is profile-scoped, so the
  server returned it as not-found and the tab bounced to the home screen. The app
  now re-establishes the saved profile on restore by re-seeding that one cookie
  before the tab loads, pinned to the server's host so WebView2 actually sends it
  — a cookie set without an explicit host is silently dropped there, which is why
  the earlier attempt looked right in testing yet did nothing on Windows. Your
  login and drafts still ride along in the reused data folder; only the profile
  selector is restored. (macOS was unaffected — its webview keeps the in-memory
  cookie for the life of the app.)
- **Windows/Linux: renaming a tab no longer has the edit box vanish as you type**
  (issue #38). Double-clicking a tab opens an inline rename field, but the active
  tab's title changes constantly while a chat streams, and each change rebuilt the
  tab strip — destroying the open field mid-keystroke. The strip now rebuilds only
  when something it actually shows has changed, and never while a rename is in
  progress, so the field stays put until you press Enter or Escape.
- **Windows: hiding the tab bar now tells you how to bring it back** (issue #10).
  Hiding the bar (the "⋯" menu or Ctrl+Shift+B) also hides the "⋯" button — the
  only on-screen control — leaving first-time users with no way to discover how to
  restore it. Hiding it now shows an OS notification with the shortcut, and keeps
  reminding you on each hide until you've successfully brought the bar back at
  least once (so the hint isn't wasted if the notification was suppressed).

### Changed

- **Every tab shows a profile color dot — the default profile included**
  (issues #8 / #31). The dot used to be driven by the profile *cookie*, which the
  WebUI sets only on an explicit switch — so a tab that simply *started* on a
  named profile showed no dot, and the default profile never showed one at all.
  Now every profile maps to its own stable color, default and named alike: each
  tab carries its current profile's dot, all tabs on the same profile share one
  color, and switching a tab's profile recolors it. Reopened tabs come back on
  the profile (and session) you left them on, with the colors reloaded. Driven by
  the page's reported active profile (`/api/profile/active`), authoritative for
  the default profile too, so it's reliable rather than guessed from a cookie.

## [v0.6.1] — 2026-06-19

A crash-and-bug-fix release.

### Fixed

- **The "⋯" overflow menu no longer hangs the app on Windows** (issue #33,
  follow-up to v0.6.0). v0.6.0 moved the popup onto the event loop, but the hang
  persisted: testers still saw the app freeze ~2-3 seconds after opening the
  menu — the window stuck on top of everything, Preferences/Quit dead, only a
  Task Manager kill recovered it. The popup runs a native modal loop that owns
  the main thread until dismissed; meanwhile background work that reads a tab's
  cookie or URL marshals back onto the main thread, and that re-entry while the
  modal loop is up deadlocked the UI — independent of which item you hovered or
  selected, matching the "it's a time thing" report. Every such read — the
  periodic 4-second autosave + profile-dot refresh, **and** the
  navigation-driven profile re-read and page-load capture — now pauses while a
  menu is open and resumes the moment it closes, so the menu stays responsive.
- **A restored tab no longer shows "Session not available in web UI." on
  launch** (issue #37, new in v0.6.0's deep-session restore). Behind a reverse
  proxy the restored session's first load could 404 before the server was ready
  to resolve it, leaving the tab on the empty-state until you switched away and
  back. A restored tab that gets bounced to the home screen now retries its
  saved session once, a couple seconds later — mirroring that switch-away-and-back
  recovery — so the session you left off in comes back on its own. (If a tab
  still lands on the empty-state behind a slow proxy, a manual reload or session
  switch recovers it.)

## [v0.6.0] — 2026-06-18

A bug-squash release: a Windows hang blocker, plus restore/notification/profile-dot fixes.

### Fixed

- **Windows: clicking the "⋯" overflow menu no longer hangs the app** (issue #33).
  The menu was popped synchronously inside the IPC command; on Windows that
  modal `TrackPopupMenu` loop wedged the WebView2 message pump (AppHangB1) — the
  menu stuck, the window got stuck always-on-top, and Preferences/Quit stopped
  working, forcing a kill from Task Manager. The popup now runs on the main
  event loop so its modal loop pumps normally and the command returns at once.
- **Reopened tabs come back on the session they were on, not a blank page**
  (issue #30). Restore replayed the root URL, so each tab returned fresh
  (session, title, and profile dot all gone). The page now reports its live URL
  — including in-app (SPA) navigation that `WebView2`/`WKWebView` don't expose
  through the wrapper — so restore reopens the exact deep-linked session.
- **The per-tab profile dot updates right after you switch profile** (issue #31,
  follow-on to #26). The dot is now re-read the moment the page navigates (a
  profile/session switch), so it paints correctly on a freshly created tab
  without needing to open another tab, and no longer adopts a sibling tab's
  color after a session switch.

### Changed

- **Browser notifications now fire as native OS notifications** (issue #32).
  The embedded webview doesn't implement the Web Notifications API, so the
  WebUI's notifications (response complete, approval required, clarification
  needed) silently did nothing. The app now bridges `window.Notification` to
  native notifications, so they appear in Notification Center / the Windows
  action center / the Linux notification tray. Honors the app's notifications
  preference. (Replaces the old DOM-heuristic "response is ready" guess with the
  WebUI's real, specific notifications.)


## [v0.5.0] — 2026-06-17

A bug-squash release focused on things that worked worse than the web, plus tab
and session fixes and a few tab conveniences.

### Fixed

- **You can drag files from the workspace tree onto the chat composer again**
  (issue #27, macOS/Windows/Linux). The desktop wrapper left wry's native OS
  drag-drop handler enabled, which intercepted drag events over the page so the
  WebUI's own drag-drop never fired — you got the "no-drop" cursor. The handler
  is now disabled on the content webviews, so in-page drag works **and**
  dragging a file in from Finder/Explorer drops onto the composer to upload.
- **Restored tabs no longer come back as `502 Bad Gateway` behind a reverse
  proxy** (issue #28). On launch the app treated *any* HTTP response — including
  a proxy's `502`/`503`/`504` while its upstream was still booting — as "ready,"
  so it reopened your tabs onto gateway-error pages that never recovered.
  Readiness now rejects those gateway statuses (a real login page / `404` / `405`
  still counts as up), waits for the server to actually serve, and reloads any
  tab if the server recovers while the app is open.
- **Login now survives a restart on Windows/Linux** (issue #28). Each tab's
  cookie jar is kept on disk and reused when its tab is restored, so you stay
  logged in across quit/reopen and updates — matching a browser. (Only the jar
  is reused; nothing sensitive is written to the prefs file. macOS tabs use an
  isolation model that can't persist, so they still re-prompt for login.)
- **The per-tab profile dot updates when you switch profile inside a tab**
  (issue #26, Windows/Linux). It previously only refreshed when a tab was opened
  or fully reloaded, so an in-tab profile switch left the dot showing the old
  color; it now re-reads on tab activation and a light periodic sweep, and the
  first tabs after a re-login get their dots too.

### Added

- **Rename a tab** by double-clicking its title (issue #7, Windows/Linux strip).
  The name sticks regardless of the page title and is restored across restart;
  clear it to fall back to the page title.
- **Hide the tab bar** to reclaim its space (issue #10, Windows) — from the ⋯
  menu or **Ctrl+Shift+B** (which also brings it back). macOS uses native tabs;
  Linux is pending an upstream webview-geometry fix.
- **What's New** in the app menu / ⋯ menu (issue #6) shows the current version
  and this release's changelog in-app, with a link to the full changelog.

## [v0.4.1] — 2026-06-17

### Fixed

- **macOS: dragging the window by the title bar with a single tab open now
  actually works** (issue #22). The v0.4.0 attempt used the CSS
  `-webkit-app-region: drag`, which is a Chromium/Electron feature that macOS's
  WebKit webview ignores — so it was a no-op and the window still couldn't be
  moved. The real cause: the window-drag was driven by Tauri's
  `data-tauri-drag-region`, but the command it invokes (`start_dragging`) was
  never permitted for the remote page, so it silently did nothing. The
  permission is now granted (narrowly — only window dragging, nothing else), and
  the whole title bar is a drag region (its buttons/links still click). With two
  or more tabs the native tab bar already provided dragging, which is why the
  bug only appeared with a single tab.

## [v0.4.0] — 2026-06-17

A tabs & sessions release across macOS, Windows, and Linux.

### Added

- **Your windows and tabs come back after you quit or update the app**
  (issue #18). The shell now remembers each window's tabs — their order, which
  one was active, each tab's URL, and each tab's profile — and restores them on
  the next launch (including after an in-app update relaunches). Previously
  every restart reopened a single empty tab and the rest were lost. Works on all
  three platforms: macOS rebuilds the native window tab groups; Windows/Linux
  rebuild the tab strip. Each restored tab reopens on the profile it was on (the
  profile selector is re-seeded). Notes: a server that requires login will ask
  you to sign in again after a restart (only the profile selector is persisted,
  never login/auth cookies); the session is saved for the server you're
  connected to and isn't restored if you switch to a different server.
- **Per-tab profile dot** (issue #8, Windows/Linux tab strip). Each tab shows a
  small colored dot keyed to its active profile, so several profiles open at
  once are easy to tell apart at a glance; the default profile shows no dot.
  Hovering a tab shows its profile name. Pairs with the per-tab profile
  isolation added in v0.3.7/v0.3.8.
- **Drag tabs left/right to reorder them** in the Windows/Linux tab strip
  (issue #19). macOS already reorders via native window tabs; the custom strip
  had no way to. The new order is what gets saved and restored.

### Fixed

- **macOS: you couldn't drag the window by the title bar when only one tab was
  open** (issue #22). Window dragging relied on Tauri's JS-based drag region,
  which doesn't work reliably in the remote-content webview — so with a single
  tab (no native tab bar to grab) the window wouldn't move. Dragging now uses
  WebKit's built-in native drag region (`-webkit-app-region`), which is
  independent of that fragile IPC and works regardless of how many tabs are
  open. (Same remote-webview limitation behind the earlier title (#15) and
  external-link (#12) fixes.)

### Known issues

- The app icon still shows a faint light edge on its rounded corners (issue #5)
  — the icon artwork has a light border baked in, so a clean fix needs the
  source logo and a proper icon pass rather than editing the rendered images.
  Tracked for a follow-up.

## [v0.3.8] — 2026-06-16

### Fixed

- **macOS: multiple tabs on different profiles bled into each other** (the
  v0.3.7 fix only covered the Windows/Linux tab strip). macOS uses native
  window tabs, which shared one cookie store, so switching profile in one tab
  switched the others too. Each tab opened from an existing one now gets its own
  isolated (ephemeral) cookie store, seeded with the opener tab's profile +
  login so it still opens on your current profile and only diverges when you
  switch it — the same behavior the Windows/Linux fix gives. The first window
  keeps the persistent store, so a single-window setup still remembers your
  profile and login across restarts. (#3, reported by the maintainer on macOS.)
- **A new tab now reliably inherits the current tab's profile.** The seed copied
  the opener's cookies via a URL-filtered lookup, but that lookup drops
  host-only cookies on macOS — and the WebUI sets the profile cookie host-only —
  so a new tab fell back to the default profile instead of the one you were on.
  Seeding now copies the opener's whole cookie store (a tab only ever loads one
  origin), so the profile cookie transfers on every platform.

## [v0.3.7] — 2026-06-16

### Fixed

- **Windows/Linux: multiple tabs on different profiles no longer bleed into
  each other.** The WebUI scopes the active profile to a per-client HttpOnly
  `hermes_profile` cookie, but every tab shared one cookie jar — so switching
  profile in one tab flipped it for all of them: the sidebar stuck to the
  last-loaded profile, other profiles' active chats went missing, and the
  default workspace intermittently failed to apply. Each tab now gets its own
  isolated data partition (its own cookie jar), so profile selection is
  genuinely per-tab — the same isolation you get from separate browser windows.
  A new tab is seeded with the opener tab's profile + login cookies, so it still
  opens on your current profile (and stays logged in) and only diverges when you
  switch it. Partitions are session-scoped and cleared on tab close and at
  startup. (#3, reported by b3nw and Lemz.)

## [v0.3.6] — 2026-06-14

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
