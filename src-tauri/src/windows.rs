//! Window management: content (browser) windows, splash, error, preferences.
//! Mirrors AppDelegate/BrowserWindowController behaviors (docs/03).

use crate::state::{AppState, TunnelStatus};
use crate::{bridge, prefs, strip, theme};
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub const TABBING_ID: &str = "ai.get-hermes.HermesWebUIDesktop.main";

/// The appearance every new window opens with — from the cached page
/// background (7-day staleness), defaulting to dark. Port of the Swift app's
/// `currentAppearance` seeded by loadCachedTheme(): windows must be born with
/// the right chrome theme because the theme bridge's match-suppression means
/// it stays silent when the page already matches the cache.
pub fn cached_theme(app: &AppHandle) -> tauri::Theme {
    let (r, g, b) = prefs::pre_paint_color(app);
    if theme::is_dark(r, g, b) {
        tauri::Theme::Dark
    } else {
        tauri::Theme::Light
    }
}

/// All live content (browser) windows, ordered by label sequence.
/// Raw window handles for every content window, ordered by label sequence.
/// Works in BOTH modes (macOS WebviewWindows and strip-mode multi-webview
/// windows both appear in app.windows()) — use this for window-level ops
/// (hide/show/destroy/count/position).
pub fn content_window_handles(app: &AppHandle) -> Vec<tauri::Window> {
    let mut wins: Vec<(u64, tauri::Window)> = app
        .windows()
        .into_iter()
        .filter(|(label, _)| label.starts_with("main-"))
        .map(|(label, w)| {
            let n: u64 = label.trim_start_matches("main-").parse().unwrap_or(0);
            (n, w)
        })
        .collect();
    wins.sort_by_key(|(n, _)| *n);
    wins.into_iter().map(|(_, w)| w).collect()
}

pub fn focused_or_recent_window_handle(app: &AppHandle) -> Option<tauri::Window> {
    let wins = content_window_handles(app);
    wins.iter()
        .find(|w| w.is_focused().unwrap_or(false))
        .cloned()
        .or_else(|| wins.last().cloned())
}

/// Evaluate JS in EVERY content webview (mac: one per window; strip: every tab).
pub fn eval_all_content(app: &AppHandle, js: &str) {
    if strip::enabled() {
        for wv in strip::all_tab_webviews(app) {
            let _ = wv.eval(js);
        }
    } else {
        for w in content_windows(app) {
            let _ = w.eval(js);
        }
    }
}

/// Evaluate JS in the ACTIVE content webview (focused window's visible tab).
pub fn active_content_eval(app: &AppHandle, js: &str) {
    if strip::enabled() {
        if let Some(wv) = strip::focused_active_webview(app) {
            let _ = wv.eval(js);
        }
    } else if let Some(w) = focused_or_recent_content(app) {
        let _ = w.eval(js);
    }
}

/// Set zoom on the active content webview.
pub fn active_content_zoom(app: &AppHandle, zoom: f64) {
    if strip::enabled() {
        if let Some(wv) = strip::focused_active_webview(app) {
            let _ = wv.set_zoom(zoom);
        }
    } else if let Some(w) = focused_or_recent_content(app) {
        let _ = w.set_zoom(zoom);
    }
}

pub fn content_windows(app: &AppHandle) -> Vec<WebviewWindow> {
    let mut wins: Vec<(u64, WebviewWindow)> = app
        .webview_windows()
        .into_iter()
        .filter(|(label, _)| label.starts_with("main-"))
        .map(|(label, w)| {
            let n: u64 = label.trim_start_matches("main-").parse().unwrap_or(0);
            (n, w)
        })
        .collect();
    wins.sort_by_key(|(n, _)| *n);
    wins.into_iter().map(|(_, w)| w).collect()
}

pub fn focused_or_recent_content(app: &AppHandle) -> Option<WebviewWindow> {
    let wins = content_windows(app);
    wins.iter()
        .find(|w| w.is_focused().unwrap_or(false))
        .cloned()
        .or_else(|| wins.last().cloned())
}

pub fn all_modes_match(app: &AppHandle, mode: &str) -> bool {
    let state = app.state::<AppState>();
    let modes = state.window_modes.lock().unwrap();
    content_windows(app)
        .iter()
        .all(|w| modes.get(w.label()).map(String::as_str) == Some(mode))
}

pub fn forget(app: &AppHandle, label: &str) {
    let state = app.state::<AppState>();
    state.window_modes.lock().unwrap().remove(label);
    state.raw_titles.lock().unwrap().remove(label);
}

/// Open a new browser window (a "tab" on macOS when as_tab and a window
/// exists to join). Port of AppDelegate.openBrowser.
pub fn open_browser(app: &AppHandle, p: &prefs::Prefs, as_tab: bool) -> Option<WebviewWindow> {
    // Windows/Linux: strip mode (custom tab bar + one webview per tab).
    if strip::enabled() {
        strip::open_browser_window(app, p);
        return None;
    }
    let state = app.state::<AppState>();
    let n = state.window_seq.fetch_add(1, Ordering::SeqCst) + 1;
    let label = format!("main-{n}");

    let target = match url::Url::parse(&p.target_url) {
        Ok(u) => u,
        Err(e) => {
            log::error!("open_browser: bad target URL {}: {e}", p.target_url);
            return None;
        }
    };
    let allowed_host = target.host_str().map(|h| h.to_lowercase());

    let (r, g, b) = prefs::pre_paint_color(app);
    let hex = theme::hex_string(r, g, b);
    let init = bridge::init_script(&label, &hex, p.connection_mode == "ssh");

    let host_window = focused_or_recent_content(app);
    let is_first = host_window.is_none();

    let nav_app = app.clone();
    let load_label = label.clone();
    let load_mode = p.connection_mode.clone();
    let load_host = p.ssh_host.clone();
    let load_port = p.local_port.clone();

    #[allow(unused_mut)]
    let mut builder = WebviewWindowBuilder::new(app, &label, WebviewUrl::External(target))
        .title("Hermes WebUI")
        .inner_size(1280.0, 830.0)
        // Chrome (titlebar/tab bar) opens in the cached page theme — never
        // the OS appearance (Swift: window.appearance = currentAppearance).
        .theme(Some(cached_theme(app)))
        // Anti-flash (Swift fix #52): stay hidden until the first paint-ready
        // moment (page load finished), then show — no white/blank flash.
        .visible(false)
        .initialization_script(&init)
        .on_navigation(move |url| navigation_allowed(&nav_app, url, allowed_host.as_deref()))
        .on_page_load(move |webview, payload| {
            if !matches!(payload.event(), tauri::webview::PageLoadEvent::Finished) {
                return;
            }
            let app = webview.app_handle().clone();
            let Some(win) = app.get_webview_window(&load_label) else {
                return;
            };
            // Persisted zoom re-applies on every load (Swift didFinish parity).
            let zoom = prefs::zoom_get(&app);
            if (zoom - 1.0).abs() > f64::EPSILON {
                let _ = win.set_zoom(zoom);
            }
            // First successful load: reveal the window.
            if !win.is_visible().unwrap_or(true) {
                let _ = win.show();
                let _ = win.set_focus();
            }
            // Reloads reset the injected footer + chrome classes — replay state.
            if load_mode == "ssh" {
                let status = *app.state::<AppState>().tunnel_status.lock().unwrap();
                push_tunnel_status(&app, &win, status, &load_host, &load_port);
            }
            app.state::<AppState>()
                .ui_state
                .lock()
                .unwrap()
                .remove(win.label());
            refresh_macos_chrome(&app);
        });

    #[cfg(target_os = "macos")]
    {
        use tauri::TitleBarStyle;
        builder = builder
            .title_bar_style(TitleBarStyle::Overlay)
            .hidden_title(true)
            .tabbing_identifier(TABBING_ID);
    }

    let win = match builder.build() {
        Ok(w) => w,
        Err(e) => {
            log::error!("open_browser: window build failed: {e}");
            return None;
        }
    };

    state
        .window_modes
        .lock()
        .unwrap()
        .insert(label.clone(), p.connection_mode.clone());

    if is_first {
        // First window: restore the persisted frame (≈ HermesMainWindow
        // autosave), else center — Swift first-launch behavior.
        if let Some((x, y, w, h)) = prefs::frame_load(app) {
            let _ = win.set_size(tauri::PhysicalSize::new(w, h));
            let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
        } else {
            let _ = win.center();
        }
        // Fullscreen restore (Swift fix #43), after the window shows.
        if prefs::fullscreen_get(app) {
            let w2 = win.clone();
            std::thread::spawn(move || {
                for _ in 0..50 {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    if w2.is_visible().unwrap_or(false) {
                        let _ = w2.set_fullscreen(true);
                        return;
                    }
                }
            });
        }
    } else if as_tab {
        // Cmd+T: explicit join of the key window's native tab group —
        // AppKit's auto-tab heuristic is flaky (Swift app lesson).
        #[cfg(target_os = "macos")]
        if let Some(host) = &host_window {
            crate::macos::add_tabbed_window(host, &win);
        }
        // Windows/Linux have no native tabbing yet (custom tab strip is the
        // v0.4.0 sprint) — until then Ctrl+T behaves like Ctrl+N: a separate
        // window, cascaded so it visibly lands somewhere new.
        #[cfg(not(target_os = "macos"))]
        if let Some(host) = &host_window {
            if let Ok(pos) = host.outer_position() {
                let _ = win.set_position(tauri::PhysicalPosition::new(pos.x + 28, pos.y + 28));
            }
        }
    } else {
        // Cmd+N: force standalone at show-time (tabbingMode = disallowed),
        // restore preferred after show so Merge All Windows still works.
        #[cfg(target_os = "macos")]
        {
            crate::macos::set_tabbing_mode(&win, true);
            let w2 = win.clone();
            std::thread::spawn(move || {
                for _ in 0..50 {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    if w2.is_visible().unwrap_or(false) {
                        std::thread::sleep(std::time::Duration::from_millis(150));
                        crate::macos::set_tabbing_mode(&w2, false);
                        return;
                    }
                }
            });
        }
        // Cascade from the front-most window instead of stacking.
        if let Some(host) = &host_window {
            if let Ok(pos) = host.outer_position() {
                let _ = win.set_position(tauri::PhysicalPosition::new(pos.x + 28, pos.y + 28));
            }
        }
    }

    // Show fallback: if the page never finishes loading (server vanished
    // between preflight and load), reveal the window anyway after 4s so the
    // user isn't left with an invisible app.
    {
        let w2 = win.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(4));
            if !w2.is_visible().unwrap_or(true) {
                let _ = w2.show();
            }
        });
    }

    set_offline_badge(app, false);
    refresh_macos_chrome(app);
    Some(win)
}

pub(crate) fn navigation_allowed(
    app: &AppHandle,
    url: &url::Url,
    allowed_host: Option<&str>,
) -> bool {
    let scheme = url.scheme();
    if scheme == "file" {
        return false;
    }
    if scheme != "http" && scheme != "https" {
        // about:, blob:, data:, etc. — the engine needs these internally.
        return true;
    }
    let host = url
        .host_str()
        .unwrap_or("")
        .trim_matches(|c| c == '[' || c == ']')
        .to_lowercase();
    if host == "localhost" || host == "127.0.0.1" || host == "::1" {
        return true;
    }
    if Some(host.as_str()) == allowed_host {
        return true;
    }
    log::info!("nav: opening externally: {url}");
    let _ = tauri_plugin_opener::open_url(url.as_str(), None::<&str>);
    let _ = app; // handle kept for future use (download routing etc.)
    false
}

/// Tunnel status fan-out: footer in every ssh window + dock badge.
pub fn on_tunnel_status_changed(app: &AppHandle, status: TunnelStatus) {
    let p = prefs::load(app);
    // Strip pages (Windows/Linux) render status from this event; the mac
    // injected footer is driven by the evals below.
    let state_str = match status {
        TunnelStatus::Connecting => "connecting",
        TunnelStatus::Connected => "connected",
        TunnelStatus::Disconnected => "disconnected",
    };
    let _ = app.emit(
        "tunnel-status",
        serde_json::json!({ "state": state_str, "host": p.ssh_host, "port": p.local_port }),
    );
    for w in content_windows(app) {
        push_tunnel_status(app, &w, status, &p.ssh_host, &p.local_port);
    }
    match status {
        TunnelStatus::Connected => set_offline_badge(app, false),
        TunnelStatus::Disconnected => set_offline_badge(app, true),
        TunnelStatus::Connecting => {}
    }
}

fn push_tunnel_status(
    _app: &AppHandle,
    w: &WebviewWindow,
    status: TunnelStatus,
    host: &str,
    port: &str,
) {
    let state_str = match status {
        TunnelStatus::Connecting => "connecting",
        TunnelStatus::Connected => "connected",
        TunnelStatus::Disconnected => "disconnected",
    };
    let host_js = host.replace(['\\', '\''], "");
    let port_js = port.replace(['\\', '\''], "");
    let _ = w.eval(format!(
        "if (window.__hermesSetTunnelStatus) window.__hermesSetTunnelStatus('{state_str}', '{host_js}', '{port_js}');"
    ));
}

/// Title pipeline — port of refreshTabTitle (docs/03 § Windows & tabs).
pub fn display_title(raw: &str, mode: &str, target: &str, healthy: bool) -> String {
    let re = regex::Regex::new(r"\s+[—\-|·]\s+Hermes(\s+Agent)?\s*$").unwrap();
    let stripped = re.replace(raw.trim(), "").trim().to_string();
    if !stripped.is_empty() {
        if stripped.chars().count() > 40 {
            let head: String = stripped.chars().take(38).collect();
            return format!("{head}…");
        }
        return stripped;
    }
    if mode == "direct" {
        let dot = if healthy { "●" } else { "○" };
        let host = url::Url::parse(target)
            .ok()
            .and_then(|u| {
                u.host_str().map(|h| match u.port() {
                    Some(p) => format!("{h}:{p}"),
                    None => h.to_string(),
                })
            })
            .unwrap_or_else(|| target.to_string());
        format!("Hermes WebUI  {dot} {host}")
    } else {
        "Hermes WebUI".to_string()
    }
}

pub fn refresh_title(app: &AppHandle, label: &str) {
    let Some(w) = app.get_webview_window(label) else {
        return;
    };
    let state = app.state::<AppState>();
    let raw = state
        .raw_titles
        .lock()
        .unwrap()
        .get(label)
        .cloned()
        .unwrap_or_default();
    let mode = state
        .window_modes
        .lock()
        .unwrap()
        .get(label)
        .cloned()
        .unwrap_or_else(|| "direct".into());
    let healthy = state.healthy.load(Ordering::SeqCst);
    let target = prefs::load(app).target_url;
    let _ = w.set_title(&display_title(&raw, &mode, &target, healthy));
}

pub fn refresh_all_titles(app: &AppHandle) {
    if strip::enabled() {
        strip::refresh_all_titles(app);
        return;
    }
    for w in content_windows(app) {
        refresh_title(app, w.label());
    }
}

/// Tab-bar-aware chrome refresh — the port of the Swift app's tabbedWindows
/// KVO + updateWebViewLayout + fullscreen handlers. For every content window:
/// resize the WKWebView below the tab bar when visible, toggle the
/// `hermes-mac-tabbed` class (hides the page's redundant titlebar), and keep
/// `--traffic-light-width` in sync with fullscreen. Driven by explicit calls
/// on tab open/close/load/resize plus a 1s poller (Tauri exposes no KVO).
pub fn refresh_macos_chrome(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let app = app.clone();
        let app2 = app.clone();
        let _ = app2.run_on_main_thread(move || {
            let wins = content_windows(&app);
            let first_label = wins.first().map(|w| w.label().to_string());
            for w in wins {
                let tabbed = crate::macos::update_webview_layout(&w);
                let fullscreen = w.is_fullscreen().unwrap_or(false);
                let state = app.state::<AppState>();
                let prev = state
                    .ui_state
                    .lock()
                    .unwrap()
                    .insert(w.label().to_string(), (tabbed, fullscreen));
                if prev != Some((tabbed, fullscreen)) {
                    let class_action = if tabbed { "add" } else { "remove" };
                    let traffic_px = if fullscreen { 0 } else { 80 };
                    let _ = w.eval(format!(
                        "if (document.body) document.body.classList.{class_action}('hermes-mac-tabbed'); \
                         document.documentElement.style.setProperty('--traffic-light-width', '{traffic_px}px');"
                    ));
                    // Persist fullscreen for the first window (Swift fix #43);
                    // only on observed transitions, never on first sighting.
                    if Some(w.label().to_string()) == first_label {
                        if let Some(p) = prev {
                            if p.1 != fullscreen {
                                prefs::fullscreen_set(&app, fullscreen);
                            }
                        }
                    }
                }
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

/// Persist the first content window's frame (≈ "NSWindow Frame
/// HermesMainWindow" autosave). Called from Moved/Resized window events.
pub fn persist_first_frame(app: &AppHandle, window: &tauri::Window) {
    if !window.label().starts_with("main-") {
        return;
    }
    let wins = content_window_handles(app);
    let Some(first) = wins.first() else { return };
    if first.label() != window.label() {
        return;
    }
    if window.is_fullscreen().unwrap_or(false) || window.is_minimized().unwrap_or(false) {
        return;
    }
    if let (Ok(pos), Ok(size)) = (window.outer_position(), window.inner_size()) {
        prefs::frame_save(app, pos.x, pos.y, size.width, size.height);
    }
}

pub fn set_offline_badge(app: &AppHandle, offline: bool) {
    #[cfg(target_os = "macos")]
    {
        if let Some(w) = app.webview_windows().values().next().cloned() {
            let _ = w.set_badge_label(offline.then(|| "!".to_string()));
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, offline);
}

pub fn show_most_recent(app: &AppHandle) {
    if let Some(w) = focused_or_recent_content(app) {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Windows/Linux last-window-quit (D11), called on every window Destroyed.
/// Quits only when no meaningful window remains AND the connection
/// orchestrator isn't mid-rebuild — its splash→error/main gaps must never
/// kill the app (the v0.1.0 "splash then nothing" Windows bug). macOS keeps
/// running with Dock semantics instead.
pub fn maybe_quit_after_close(app: &AppHandle) {
    #[cfg(not(target_os = "macos"))]
    {
        let state = app.state::<AppState>();
        if state.connecting.load(Ordering::SeqCst) {
            return;
        }
        let any_alive = !content_window_handles(app).is_empty()
            || app.get_webview_window("error").is_some()
            || app.get_webview_window("prefs").is_some()
            || app.get_webview_window("splash").is_some();
        if !any_alive {
            log::info!("windows: last window closed — exiting");
            app.exit(0);
        }
    }
    #[cfg(target_os = "macos")]
    let _ = app;
}

// ---- Shell windows ----

pub fn show_splash(app: &AppHandle, _p: &prefs::Prefs) {
    if let Some(w) = app.get_webview_window("splash") {
        let _ = w.destroy();
    }
    let _ = WebviewWindowBuilder::new(app, "splash", WebviewUrl::App("splash.html".into()))
        .title("Hermes WebUI Desktop")
        .inner_size(420.0, 200.0)
        .resizable(false)
        .decorations(false)
        .theme(Some(cached_theme(app)))
        .center()
        .build();
}

pub fn close_splash(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("splash") {
        let _ = w.destroy();
    }
}

pub fn show_error(app: &AppHandle, _p: &prefs::Prefs) {
    if let Some(w) = app.get_webview_window("error") {
        let _ = w.set_focus();
        return;
    }
    let _ = WebviewWindowBuilder::new(app, "error", WebviewUrl::App("error.html".into()))
        .title("Hermes WebUI Desktop")
        .inner_size(460.0, 320.0)
        .resizable(false)
        .theme(Some(cached_theme(app)))
        .center()
        .build();
}

pub fn open_prefs(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("prefs") {
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    // Build OFF the calling thread. On Windows, creating a WebView2 webview
    // synchronously inside an IPC command (which runs on the main thread)
    // stalls its initialization — the window appears but never paints
    // (v0.1.1 report: "prefs window didn't render", showing only a blurred
    // DWM backdrop). Every other window is already built from a worker
    // thread; this one must be too.
    let app = app.clone();
    std::thread::spawn(move || {
        if app.get_webview_window("prefs").is_some() {
            return;
        }
        let _ = WebviewWindowBuilder::new(&app, "prefs", WebviewUrl::App("prefs.html".into()))
            .title("Preferences")
            .inner_size(520.0, 640.0)
            .resizable(false)
            .theme(Some(cached_theme(&app)))
            .center()
            .build();
    });
}

#[cfg(test)]
mod tests {
    use super::display_title;

    #[test]
    fn strips_hermes_suffix_and_truncates() {
        assert_eq!(
            display_title(
                "Planning chat — Hermes",
                "ssh",
                "http://localhost:8787",
                true
            ),
            "Planning chat"
        );
        assert_eq!(
            display_title("Notes - Hermes Agent", "ssh", "http://localhost:8787", true),
            "Notes"
        );
        let long = format!("{} — Hermes", "x".repeat(60));
        let t = display_title(&long, "ssh", "http://localhost:8787", true);
        assert_eq!(t.chars().count(), 39); // 38 + ellipsis
    }

    #[test]
    fn fallback_titles() {
        assert_eq!(
            display_title("", "direct", "http://localhost:8787", true),
            "Hermes WebUI  ● localhost:8787"
        );
        assert_eq!(
            display_title("", "direct", "http://localhost:8787", false),
            "Hermes WebUI  ○ localhost:8787"
        );
        assert_eq!(
            display_title("", "ssh", "http://localhost:8787", true),
            "Hermes WebUI"
        );
    }
}
