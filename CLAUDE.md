# CLAUDE.md — hermes-desktop-rust

> Read this before touching any code. Cross-platform (macOS / Windows / Linux)
> Tauri 2 desktop shell for hermes-webui — the Rust sibling of
> [hermes-swift-mac](https://github.com/hermes-webui/hermes-swift-mac).

---

## What this project is

A thin native shell that hosts hermes-webui (default `http://localhost:8787`) in the
system webview — WKWebView on macOS, WebView2 on Windows, WebKitGTK on Linux — with
Direct and SSH-tunnel connection modes, native macOS tabs, theme-matched chrome, and
preferences mirroring the Swift app's UserDefaults keys 1:1. The server owns all
state; the shell owns the window, the ssh process, and a small prefs JSON.

**Language:** Rust (Tauri 2) + vanilla HTML/CSS/JS shell pages (no framework, no bundler)
**Build:** `npm install && npx tauri build` (local) or tag `v*` to trigger CI
**Dev:** `npx tauri dev` against a running hermes-webui
**Tests:** `cd src-tauri && cargo test`
**CI:** `.github/workflows/test.yml` (PR tests), `release.yml` (tag-driven builds)
**Latest release:** `git tag --sort=-v:refname | head -1`

---

## Repo structure

```
src/                      # shell pages (vanilla): splash.html, error.html, prefs.html
src-tauri/src/
  main.rs                 # tauri Builder, plugins, commands, window/menu events
  conn.rs                 # connection orchestrator (splash → connect → windows | error)
  tunnel.rs               # ssh child process: spawn args, HTTP-probe readiness,
                          #   10s liveness monitor, TERM→KILL teardown (argv frozen by test)
  health.rs               # HTTP probe primitives (GET, any-HTTP-response = reachable)
  windows.rs              # browser/splash/error/prefs windows, titles, frame persistence
  bridge.rs               # ALL injected scripts (theme bridge, paste, find bar, footer,
                          #   notifier, shortcut forwarder) + the `bridge` event handler
  macos.rs                # objc2 shims: addTabbedWindow, tabbingMode, tab-bar-aware
                          #   webview layout (port of the Swift updateWebViewLayout)
  prefs.rs                # tauri-plugin-store accessors; keys mirror Swift UserDefaults
  theme.rs                # CSS color parse / luminance / hex (pure fns, unit-tested)
  paste.rs                # clipboard image → PNG → base64 → 3-strategy injection
  menu.rs                 # macOS menu bar (Win/Linux use the injected shortcut forwarder)
src-tauri/capabilities/   # IPC scoping: full for shell pages; remote content may ONLY
                          #   emit events (content.json)
```

---

## The rules

### The Swift app is the spec
When behavior is in question, match
[hermes-swift-mac](https://github.com/hermes-webui/hermes-swift-mac) and translate
the idiom. Deviations must be deliberate and documented in the changelog entry.

### Never push directly to main
All changes through a named branch + PR. Tests must pass. CHANGELOG entry required
for anything user-visible.

### SSH push required
```bash
eval $(ssh-agent -s) && ssh-add ~/.ssh/id_ed25519
git push origin <branch>     # or: git push origin vX.Y.Z
```
HTTPS token push fails for this org. Always use ssh-agent.

### Releases: follow RELEASING.md exactly
The whole flow is `scripts/bump_version.sh X.Y.Z` → fill CHANGELOG → commit →
`scripts/release.sh vX.Y.Z` → CI builds all three platforms into a draft release →
publish. **[RELEASING.md](RELEASING.md) is the canonical doc** — including why main
and the tag are pushed as separate operations (single-push drops events; learned in
hermes-swift-mac v1.0.5) and how to fix a botched tag.

### Version parity — three files must agree
`src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`, and the git tag (plus
`package.json`/`Cargo.lock` kept in step). `scripts/bump_version.sh` updates them
all; `scripts/release.sh` refuses to ship on mismatch. Don't bypass either.

### Hard-won invariants (do not "simplify" these away)
1. SSH forwards to `127.0.0.1` on the remote side, never `localhost` (IPv6-first
   `/etc/hosts` breaks IPv4-only servers).
2. Probes use **GET** and treat **any** HTTP status as reachable — only transport
   errors mean down. Servers 405/501 on HEAD.
3. Tunnel readiness needs an end-to-end **HTTP round-trip**; ssh accepts the local
   socket even when the remote end of the forward is dead.
4. The `ssh` argv is frozen by `tunnel::tests::ssh_args_frozen` — changing it is a
   behavior change, not a refactor.
5. Theme bridge: match-suppression + the 2.5 s stability gate exist to stop
   mount-time flicker; window themes must ALSO be seeded from the cache at creation
   (`windows::cached_theme`) because the bridge stays silent when page == cache.
6. Same-mode reconnects must REUSE webviews (`conn.rs` reuse rule) or users lose
   in-flight chats.
7. Injected scripts (`bridge.rs`) only get `core:event:default` capability in remote
   content — never grant the remote origin command access.
8. Windows ssh spawns need `CREATE_NO_WINDOW` or a console flashes per reconnect.
9. **Windows/Linux: never create a webview window synchronously inside an IPC
   command or event handler.** WebView2 init needs the main message loop
   pumping — a webview built from a blocking main-thread context stalls forever
   and the window never paints (v0.1.2 bug). Build windows from a worker thread
   (the orchestrator pattern). macOS is the deliberate exception: menu-driven
   window creation runs INLINE on the main thread (`conn::open_new_session`) —
   plain AppKit usage, no WebView2 constraint.
10. A transient zero-window moment must never exit the app — exit requests
    without a code are suppressed everywhere and Win/Linux quit-on-last-close
    is explicit in `windows::maybe_quit_after_close` (v0.1.1 bug).
11. Linux `main()` calls `XInitThreads` (via x11-dl) as its FIRST statement,
    before GTK/GDK touch X11. WebKitGTK's internal threads make direct X
    calls; without thread-safe Xlib they intermittently corrupt the reply
    stream at startup — `xcb_xlib_threads_sequence_lost` abort or a silent
    exit(1) (v0.3.2 fix; ~2-in-3 startup crash under the CI smoke harness).
12. **macOS NSWindow mutations that can force a synchronous display
    (`addTabbedWindow`, `setTabbingMode`) go through the GCD main queue
    (`macos::dispatch_main_async`) — NEVER directly inside a tao callout**
    (menu/IPC handlers, `run_on_main_thread` closures). AppKit's forced
    redraw re-enters tao's `draw_rect`, which takes tao's non-reentrant
    handler mutex → main-thread self-deadlock, app frozen with no crash
    report (the v0.2.0–v0.3.2 every-Cmd+T freeze; size-dependent, so
    default-size dev windows masked it). Repro/guard: run with
    `HERMES_TEST_TAB_AFTER=<s>` — drives the Cmd+T path and heartbeats the
    main thread.

### Cross-compiling Windows locally (optional — CI does this natively)
```bash
brew install nsis llvm lld && cargo install cargo-xwin
rustup target add x86_64-pc-windows-msvc
export PATH="/opt/homebrew/opt/llvm/bin:/opt/homebrew/opt/lld/bin:$PATH"
npx tauri build --runner cargo-xwin --target x86_64-pc-windows-msvc --bundles nsis
```
Note: makensis can crash (`std::bad_alloc`) under sandboxed/memory-capped shells —
prefer the CI artifact.
