//! Window management: content (browser) windows, splash, error, preferences.
//! Mirrors AppDelegate/BrowserWindowController behaviors (docs/03).

use crate::state::{AppState, TunnelStatus};
use crate::{bridge, prefs, strip, theme};
use std::sync::atomic::Ordering;
use std::sync::LazyLock;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

/// The WebUI prepends this glyph (U+25CF + space) to `document.title` when the
/// view's active session has a pending approval/clarify popup waiting on the
/// user (hermes-webui `ui.js` `syncTopbar`, #4121). Each tab is its own webview
/// reporting its own title, so this is a per-tab "needs attention" signal — the
/// input for the strip's attention badge (issue #14).
const ATTENTION_MARKER: char = '●';

/// Split a reported `document.title` into (pending-attention, marker-free
/// title). The marker is stripped so it never leaks into the displayed title
/// text (the strip renders a proper badge instead, and the macOS window title
/// stays clean).
pub fn split_attention(raw: &str) -> (bool, &str) {
    match raw.trim_start().strip_prefix(ATTENTION_MARKER) {
        Some(rest) => (true, rest.trim_start()),
        None => (false, raw),
    }
}

/// Title-segment separators the WebUI may place before its trailing name
/// suffix. Shared by the suffix regex and the separator-only collapse check in
/// `clean_title` so the two can't drift. The hyphen is last so it reads as a
/// literal inside the regex character classes built below.
const TITLE_SEPARATORS: &str = "—–|·-";

/// Matches a trailing ` <sep> <segment>` suffix (segment is separator-free, so
/// only the LAST separator group is removed). Leading `\s*` (not `\s+`) so a
/// session-less title like " — Hermes" (trims to "— Hermes", separator at
/// index 0) still strips to empty. Compiled once.
static TITLE_SUFFIX_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(&format!(r"\s*[{s}]\s+[^{s}]+\s*$", s = TITLE_SEPARATORS)).unwrap()
});

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
    state.window_profiles.lock().unwrap().remove(label);
    state.window_profile_names.lock().unwrap().remove(label);
    state.window_indicators.lock().unwrap().remove(label);
    crate::session::forget_navigated(app, label);
    crate::session::forget_url(app, label);
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

    // macOS per-tab cookie isolation (issue #3): native-tab webviews share one
    // WKWebsiteDataStore by default, so the WebUI's HttpOnly `hermes_profile`
    // cookie bleeds across tabs — switching profile in one tab flips it in all.
    // Each window now gets its own ephemeral store via `incognito` (set in the
    // macOS builder block below). When there's an opener, seed its cookies so
    // the new tab still inherits the current profile + login, loading
    // about:blank first so the seed lands before the real navigation — the same
    // approach the Windows/Linux strip uses (#3 / v0.3.7).
    let seed_macos = cfg!(target_os = "macos") && host_window.is_some();
    #[cfg(target_os = "macos")]
    let seed_target = target.clone();
    let initial_url = if seed_macos {
        url::Url::parse("about:blank").unwrap()
    } else {
        target
    };

    let nav_app = app.clone();
    let load_label = label.clone();
    let load_mode = p.connection_mode.clone();
    let load_host = p.ssh_host.clone();
    let load_port = p.local_port.clone();

    #[allow(unused_mut)]
    let mut builder = WebviewWindowBuilder::new(app, &label, WebviewUrl::External(initial_url))
        .title("Hermes WebUI")
        .inner_size(1280.0, 830.0)
        // Let the WebUI's HTML5 drag-drop work (tree→composer drag, file-drop
        // upload) — wry's native drag-drop handler would otherwise swallow it
        // (issue #27). The shell intercepts no native drops.
        .disable_drag_drop_handler()
        // Chrome (titlebar/tab bar) opens in the cached page theme — never
        // the OS appearance (Swift: window.appearance = currentAppearance).
        .theme(Some(cached_theme(app)))
        // Anti-flash (Swift fix #52): stay hidden until the first paint-ready
        // moment (page load finished), then show — no white/blank flash.
        .visible(false)
        .initialization_script(&init)
        .on_navigation(move |url| navigation_allowed(&nav_app, url, allowed_host.as_deref()))
        // Native title source (see apply_reported_title) — replaces the JS
        // EMIT('title') watcher so titles work regardless of remote-webview IPC.
        .on_document_title_changed(|win, title| {
            apply_reported_title(win.app_handle(), win.label(), &title);
        })
        .on_page_load(move |webview, payload| {
            if !matches!(payload.event(), tauri::webview::PageLoadEvent::Finished) {
                return;
            }
            // Skip the pre-seed about:blank load (macOS seeded tabs): reveal,
            // zoom and chrome replay only apply to the real target page.
            if payload.url().scheme() == "about" {
                return;
            }
            let app = webview.app_handle().clone();
            let Some(win) = app.get_webview_window(&load_label) else {
                return;
            };
            // Non-nil URL now — capture may read it (#18 crash guard).
            crate::session::mark_navigated(&app, &load_label);
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
            // Capture this window's active profile (#8 carrier for #18 restore)
            // + persist the session. macOS-only; the cookie read is GCD-deferred
            // inside the helper (invariant #12).
            #[cfg(target_os = "macos")]
            capture_window_profile(&app, win.label());
        });

    #[cfg(target_os = "macos")]
    {
        use tauri::TitleBarStyle;
        builder = builder
            .title_bar_style(TitleBarStyle::Overlay)
            .hidden_title(true)
            .tabbing_identifier(TABBING_ID)
            // Per-tab cookie isolation (issue #3): give each tab opened from an
            // existing one its own ephemeral WKWebsiteDataStore so the profile
            // cookie can't bleed across native tabs. The first window (no
            // opener) keeps the default persistent store — it's the only window
            // using it, so nothing shares a jar, and login/profile still persist
            // across restarts for the common single-window case.
            .incognito(seed_macos);
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

    // macOS: seed the new (incognito) window's jar from the opener, then load
    // the real target. Deferred to the GCD main queue (run_on_main_async) so
    // the cookie reads/writes — which pump the main run loop — run BETWEEN tao
    // callouts, never inside one (invariant #12: pumping while tao's handler
    // mutex is held re-enters draw_rect and self-deadlocks). Queued last so it
    // runs after the addTabbedWindow block.
    #[cfg(target_os = "macos")]
    if seed_macos {
        if let Some(opener) = host_window {
            let new_win = win.clone();
            crate::macos::run_on_main_async(move || {
                // Use cookies() (whole store), NOT cookies_for_url(): on
                // WKWebView the latter filters by an exact
                // `cookie.domain() == url.domain()` match, which drops the
                // host-only `hermes_profile`/`hermes_session` cookies (the
                // WebUI sets them with no Domain attribute) and returns nothing
                // — the macOS "new tab doesn't inherit the profile" bug (#3).
                // The opener only ever loads the one target origin, so its whole
                // store IS the target's cookies.
                match opener.cookies() {
                    Ok(seed) => {
                        let names: Vec<&str> = seed.iter().map(|c| c.name()).collect();
                        log::info!(
                            "open_browser: seeding {} from {} — {} cookie(s): {:?}",
                            new_win.label(),
                            opener.label(),
                            seed.len(),
                            names
                        );
                        for cookie in seed {
                            if let Err(e) = new_win.set_cookie(cookie) {
                                log::warn!("open_browser: set_cookie failed: {e}");
                            }
                        }
                    }
                    Err(e) => log::warn!(
                        "open_browser: seed read failed for {} (opens on default profile): {e}",
                        new_win.label()
                    ),
                }
                if let Err(e) = new_win.navigate(seed_target) {
                    log::error!("open_browser: seed navigate failed: {e}");
                }
            });
        }
    }

    set_offline_badge(app, false);
    refresh_macos_chrome(app);
    Some(win)
}

/// macOS: read this content window's `hermes_profile` cookie and record it in
/// `window_profiles` (the per-window profile that feeds session capture, #8/#18)
/// then persist. The cookie read pumps the run loop, so it's GCD-deferred to
/// run between tao callouts (invariant #12) — never inside a page-load callout.
#[cfg(target_os = "macos")]
pub fn capture_window_profile(app: &AppHandle, label: &str) {
    let Some(win) = app.get_webview_window(label) else {
        return;
    };
    let app = app.clone();
    let label = label.to_string();
    crate::macos::run_on_main_async(move || {
        let profile = win.cookies().ok().and_then(|cs| {
            cs.into_iter()
                .find(|c| c.name() == "hermes_profile")
                .map(|c| c.value().to_string())
        });
        {
            let state = app.state::<AppState>();
            let mut map = state.window_profiles.lock().unwrap();
            match profile {
                Some(v) => {
                    map.insert(label.clone(), v);
                }
                None => {
                    map.remove(&label);
                }
            }
        }
        crate::session::persist(&app);
    });
}

/// Recreate one saved macOS window-group as native tabs (issue #18). The first
/// tab is a standalone window; each subsequent tab joins its native tab group
/// via the freeze-safe `add_tabbed_window` path (invariant #12). Every restored
/// tab is incognito and re-seeded with its saved `hermes_profile` selector, so
/// distinct profiles stay isolated and each reopens on the right one (auth
/// logins are not persisted — an authed server re-prompts). The saved active
/// tab is focused last. Runs on the orchestrator worker thread.
#[cfg(target_os = "macos")]
pub fn restore_macos_window(app: &AppHandle, sw: &crate::session::SessionWindow) {
    let p = prefs::load(app);
    let mut group_host: Option<WebviewWindow> = None;
    let mut built: Vec<WebviewWindow> = Vec::new();
    for (i, tab) in sw.tabs.iter().enumerate() {
        let url = match url::Url::parse(&tab.url).or_else(|_| url::Url::parse(&p.target_url)) {
            Ok(u) => u,
            Err(_) => continue,
        };
        let frame = if i == 0 { sw.frame } else { None };
        if let Some(win) =
            build_restored_macos_tab(app, &p, url, tab.profile.clone(), group_host.clone(), frame)
        {
            if group_host.is_none() {
                group_host = Some(win.clone());
            }
            built.push(win);
        }
    }
    // Focus the saved active tab once the group has formed (GCD is FIFO, so this
    // runs after the queued addTabbedWindow blocks).
    if let Some(active) = built.get(sw.active).or_else(|| built.last()).cloned() {
        crate::macos::run_on_main_async(move || {
            let _ = active.set_focus();
        });
    }
}

#[cfg(target_os = "macos")]
fn build_restored_macos_tab(
    app: &AppHandle,
    p: &prefs::Prefs,
    target: url::Url,
    profile: Option<String>,
    host: Option<WebviewWindow>,
    frame: Option<[i64; 4]>,
) -> Option<WebviewWindow> {
    use tauri::TitleBarStyle;
    let state = app.state::<AppState>();
    let n = state.window_seq.fetch_add(1, Ordering::SeqCst) + 1;
    let label = format!("main-{n}");
    let allowed_host = target.host_str().map(|h| h.to_lowercase());
    let (r, g, b) = prefs::pre_paint_color(app);
    let hex = theme::hex_string(r, g, b);
    let init = bridge::init_script(&label, &hex, p.connection_mode == "ssh");

    let nav_app = app.clone();
    let load_label = label.clone();
    let load_mode = p.connection_mode.clone();
    let load_host = p.ssh_host.clone();
    let load_port = p.local_port.clone();
    let seed_target = target.clone();

    let blank = url::Url::parse("about:blank").unwrap();
    let win = match WebviewWindowBuilder::new(app, &label, WebviewUrl::External(blank))
        .title("Hermes WebUI")
        .inner_size(1280.0, 830.0)
        // HTML5 drag-drop (issue #27) — see open_browser.
        .disable_drag_drop_handler()
        .theme(Some(cached_theme(app)))
        .visible(false)
        .initialization_script(&init)
        .on_navigation(move |url| navigation_allowed(&nav_app, url, allowed_host.as_deref()))
        .on_document_title_changed(|win, title| {
            apply_reported_title(win.app_handle(), win.label(), &title);
        })
        .on_page_load(move |webview, payload| {
            if !matches!(payload.event(), tauri::webview::PageLoadEvent::Finished) {
                return;
            }
            if payload.url().scheme() == "about" {
                return;
            }
            let app = webview.app_handle().clone();
            let Some(win) = app.get_webview_window(&load_label) else {
                return;
            };
            // Non-nil URL now — capture may read it (#18 crash guard).
            crate::session::mark_navigated(&app, &load_label);
            let zoom = prefs::zoom_get(&app);
            if (zoom - 1.0).abs() > f64::EPSILON {
                let _ = win.set_zoom(zoom);
            }
            if !win.is_visible().unwrap_or(true) {
                let _ = win.show();
                let _ = win.set_focus();
            }
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
            capture_window_profile(&app, win.label());
        })
        .title_bar_style(TitleBarStyle::Overlay)
        .hidden_title(true)
        .tabbing_identifier(TABBING_ID)
        .incognito(true)
        .build()
    {
        Ok(w) => w,
        Err(e) => {
            log::error!("restore: window build failed: {e}");
            return None;
        }
    };

    state
        .window_modes
        .lock()
        .unwrap()
        .insert(label.clone(), p.connection_mode.clone());

    if let Some([x, y, w, h]) = frame.filter(|f| f[2] >= 200 && f[3] >= 200) {
        let _ = win.set_size(tauri::PhysicalSize::new(w as u32, h as u32));
        let _ = win.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
    }

    // Subsequent tabs join the group (GCD-deferred — invariant #12).
    if let Some(host) = &host {
        crate::macos::add_tabbed_window(host, &win);
    }

    // Show fallback if the page never finishes loading.
    {
        let w2 = win.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(4));
            if !w2.is_visible().unwrap_or(true) {
                let _ = w2.show();
            }
        });
    }

    // Seed the profile selector into the (incognito) jar, then navigate to the
    // saved URL — GCD-deferred so the cookie write's run-loop pump runs outside
    // tao callouts (invariant #12), after the addTabbedWindow block (FIFO).
    let new_win = win.clone();
    crate::macos::run_on_main_async(move || {
        if let Some(v) = profile {
            if let Err(e) = new_win.set_cookie(crate::session::profile_cookie(&v)) {
                log::warn!("restore: set_cookie failed for {}: {e}", new_win.label());
            }
        }
        if let Err(e) = new_win.navigate(seed_target) {
            log::error!("restore: navigate failed for {}: {e}", new_win.label());
        }
    });

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

/// Strip the WebUI's trailing " — <name>" suffix from a reported document
/// title. The suffix is NOT always "Hermes": it's the user's configured bot
/// name, or — on a non-default profile — the capitalized profile name (issue
/// #15). So we strip the trailing `<sep> <segment>` generically rather than
/// matching the literal "Hermes". `<segment>` is separator-free, so only the
/// LAST separator group is removed — an internal " — " in the session title
/// itself is preserved ("Plan A — Phase 1 — Hermes" → "Plan A — Phase 1").
/// Returns empty when the title is empty or separator/suffix-only.
pub fn clean_title(raw: &str) -> String {
    let stripped = TITLE_SUFFIX_RE.replace(raw.trim(), "").trim().to_string();
    // A remainder that is only separators/whitespace (e.g. a bare "—") is a
    // transient state, not a real session title — collapse it to empty so the
    // caller's guard treats it as "no title". Real titles always contain a
    // non-separator character.
    if stripped
        .chars()
        .all(|c| c.is_whitespace() || TITLE_SEPARATORS.contains(c))
    {
        return String::new();
    }
    stripped
}

/// Title pipeline — port of refreshTabTitle (docs/03 § Windows & tabs).
pub fn display_title(raw: &str, mode: &str, target: &str, healthy: bool) -> String {
    let stripped = clean_title(raw);
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
    let base = display_title(&raw, &mode, &target, healthy);
    // macOS native tabs have no strip dot, so prefix a non-default profile name
    // onto the tab title as the per-tab profile indicator (issue #44). The
    // prefix sits OUTSIDE display_title's truncation so it's never cut.
    let titled = match state.window_profile_names.lock().unwrap().get(label) {
        Some(name) if !name.is_empty() => format!("{name} · {base}"),
        _ => base,
    };
    // State adornment (issues #64/#65), outermost so it's always visible:
    // "●" = the session is waiting on you (approval/clarify — the page's own
    // title marker, previously stripped and dropped here); "⟳" = working.
    // Attention outranks busy: a paused-for-you run matters more than motion.
    let titled = match state
        .window_indicators
        .lock()
        .unwrap()
        .get(label)
        .copied()
        .unwrap_or((false, false))
    {
        (_, true) => format!("● {titled}"),
        (true, false) => format!("⟳ {titled}"),
        _ => titled,
    };
    let _ = w.set_title(&titled);
}

/// Update a content window's (busy, attention) native-title adornment (issues
/// #64/#65) — `None` leaves that half unchanged. Repaints only on change.
pub fn set_window_indicator(
    app: &AppHandle,
    label: &str,
    busy: Option<bool>,
    attention: Option<bool>,
) {
    let changed = {
        let state = app.state::<AppState>();
        let mut map = state.window_indicators.lock().unwrap();
        let cur = map.get(label).copied().unwrap_or((false, false));
        let next = (busy.unwrap_or(cur.0), attention.unwrap_or(cur.1));
        if next == cur {
            false
        } else {
            map.insert(label.to_string(), next);
            true
        }
    };
    if changed {
        refresh_title(app, label);
    }
}

/// Set (or clear, on empty) a content window's active profile NAME, used to
/// prefix the native macOS tab title with a profile indicator (issue #44).
/// Repaints the title immediately. No-op effect on Win/Linux (those use the
/// strip dot and never emit `mac-profile`).
pub fn set_window_profile_name(app: &AppHandle, label: &str, name: &str) {
    {
        let state = app.state::<AppState>();
        let mut names = state.window_profile_names.lock().unwrap();
        if name.is_empty() {
            names.remove(label);
        } else {
            names.insert(label.to_string(), name.to_string());
        }
    }
    refresh_title(app, label);
}

/// A content webview reported a new `document.title` via wry's native
/// title-changed hook (`on_document_title_changed`). This is the sole title
/// source — it replaces the old injected `EMIT('title')` watcher, which
/// silently no-ops whenever `window.__TAURI__`/the event IPC isn't available in
/// a remote-origin webview (issue #15, failure mode #1: tabs stuck on "New
/// Tab"). Being a native callback, it is immune to that and to the page CSP.
///
/// Transient empty/separator-only reports are ignored so a known-good title is
/// never downgraded to the host fallback (`display_title`'s empty-strip branch)
/// — issue #15 failure mode #2 ("title instantly resets to Hermes WebUI ●
/// host"). The 38px strip seed ("New Tab") likewise survives until a real title
/// arrives.
pub fn apply_reported_title(app: &AppHandle, label: &str, raw: &str) {
    let (attention, title) = split_attention(raw);
    if clean_title(title).is_empty() {
        return;
    }
    if label.starts_with("tab-") {
        strip::set_tab_title(app, label, title, attention);
    } else {
        // macOS / regular content windows: store the marker-free title (the
        // adornment is re-applied in a controlled position by refresh_title)
        // and keep the attention flag — it used to be dropped here, which is
        // why macOS never showed needs-attention state (issue #64).
        app.state::<AppState>()
            .raw_titles
            .lock()
            .unwrap()
            .insert(label.to_string(), title.to_string());
        set_window_indicator(app, label, None, Some(attention));
        refresh_title(app, label);
    }
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

/// "What's New" window — version + this release's changelog (issue #6). Built
/// off the calling thread like the other shell windows (invariant #9).
pub fn open_whats_new(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("whatsnew") {
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        if app.get_webview_window("whatsnew").is_some() {
            return;
        }
        let _ =
            WebviewWindowBuilder::new(&app, "whatsnew", WebviewUrl::App("whatsnew.html".into()))
                .title("What's New")
                .inner_size(540.0, 620.0)
                .resizable(true)
                .theme(Some(cached_theme(&app)))
                .center()
                .build();
    });
}

#[cfg(test)]
mod tests {
    use super::{clean_title, display_title, split_attention};

    #[test]
    fn splits_pending_attention_marker() {
        // WebUI prepends "● " for a session with a pending approval/clarify.
        assert_eq!(
            split_attention("● My session — Hermes"),
            (true, "My session — Hermes")
        );
        // No marker → not flagged, title untouched.
        assert_eq!(
            split_attention("My session — Hermes"),
            (false, "My session — Hermes")
        );
        // Marker survives the suffix strip downstream and yields a real title.
        let (attn, body) = split_attention("● Untitled — Hermes");
        assert!(attn);
        assert_eq!(
            display_title(body, "ssh", "http://localhost:8787", true),
            "Untitled"
        );
    }

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
    fn strips_non_hermes_suffix() {
        // The WebUI appends the configured bot name or the profile name, not
        // always "Hermes" (issue #15). The strip must be generic.
        assert_eq!(
            display_title(
                "Daily standup — Claude",
                "ssh",
                "http://localhost:8787",
                true
            ),
            "Daily standup"
        );
        assert_eq!(
            display_title("Budget review — Work", "ssh", "http://localhost:8787", true),
            "Budget review"
        );
        // Only the LAST separator group is removed — an internal em-dash in the
        // session title itself survives.
        assert_eq!(
            display_title(
                "Plan A — Phase 1 — Hermes",
                "ssh",
                "http://localhost:8787",
                true
            ),
            "Plan A — Phase 1"
        );
    }

    #[test]
    fn clean_title_empty_for_transient_states() {
        // These are the reports the title hook must NOT promote to a tab title;
        // apply_reported_title drops them so a good title isn't downgraded to
        // the host fallback (issue #15 failure mode #2).
        assert_eq!(clean_title(""), "");
        assert_eq!(clean_title("   "), "");
        assert_eq!(clean_title(" — Hermes"), "");
        // A bare title with no separator suffix is meaningful, not transient.
        assert_eq!(clean_title("Hermes"), "Hermes");
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
