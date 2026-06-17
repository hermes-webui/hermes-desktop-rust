//! Strip mode — the Windows/Linux tab bar (internal docs 04 § Tab architecture,
//! roadmap v0.4.0): one OS window hosts a 38px shell webview (shell.html, full
//! IPC) plus one live content webview per tab (remote hermes-webui,
//! event-emit-only IPC). Exactly one tab webview is visible; hidden tabs stay
//! alive so SSE streams, scroll and drafts survive switching — the property
//! macOS gets from one-WKWebView-per-native-tab.
//!
//! macOS does NOT use this module (native tabs); set HERMES_FORCE_STRIP=1 to
//! exercise the strip on a Mac for development.

use crate::state::{AppState, TabEntry, WindowTabs};
use crate::{bridge, prefs, tunnel, windows};
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use tauri::webview::{Color, Cookie, WebviewBuilder};
use tauri::window::WindowBuilder;
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Webview, WebviewUrl, Window, Wry,
};

pub const STRIP_HEIGHT: f64 = 38.0;

/// Strip mode is the tab implementation everywhere except macOS.
pub fn enabled() -> bool {
    cfg!(not(target_os = "macos")) || std::env::var("HERMES_FORCE_STRIP").is_ok()
}

/// tab-{n}-{m} -> main-{n}
pub fn window_of_tab(tab_label: &str) -> Option<String> {
    let rest = tab_label.strip_prefix("tab-")?;
    let n = rest.split('-').next()?;
    Some(format!("main-{n}"))
}

fn window_seq(window_label: &str) -> &str {
    window_label.strip_prefix("main-").unwrap_or("0")
}

fn find_webview(win: &Window<Wry>, label: &str) -> Option<Webview<Wry>> {
    win.webviews().into_iter().find(|w| w.label() == label)
}

fn content_bounds(win: &Window<Wry>) -> (LogicalPosition<f64>, LogicalSize<f64>) {
    let scale = win.scale_factor().unwrap_or(1.0);
    let size = win
        .inner_size()
        .map(|s| s.to_logical::<f64>(scale))
        .unwrap_or(LogicalSize::new(1280.0, 830.0));
    (
        LogicalPosition::new(0.0, STRIP_HEIGHT),
        LogicalSize::new(size.width, (size.height - STRIP_HEIGHT).max(0.0)),
    )
}

/// Per-tab webview data partition path. Each tab gets its own directory → its
/// own cookie jar, so the WebUI's HttpOnly `hermes_profile` cookie (and the
/// login session) is scoped to that tab instead of shared across every tab
/// through one store — the profile-bleed root cause in issue #3.
fn tab_partition_dir(app: &AppHandle, tab_label: &str) -> Option<PathBuf> {
    app.path()
        .app_local_data_dir()
        .ok()
        .map(|d| d.join("tab-partitions").join(tab_label))
}

/// Best-effort removal of a closed tab's data partition. The startup wipe
/// (`clear_partitions`) is the safety net if this fails because the OS still
/// holds the folder open immediately after the webview is destroyed.
fn remove_tab_partition(app: &AppHandle, tab_label: &str) {
    if let Some(dir) = tab_partition_dir(app, tab_label) {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Wipe all tab data partitions. Called once at startup before any tab opens:
/// partitions are session-scoped (chats live server-side), so orphans from a
/// prior run or a crash would otherwise accumulate. No-op when none exist.
pub fn clear_partitions(app: &AppHandle) {
    if let Ok(base) = app.path().app_local_data_dir() {
        let dir = base.join("tab-partitions");
        if dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&dir) {
                log::warn!("strip: could not clear tab partitions: {e}");
            }
        }
    }
}

/// Open a new strip window with its first tab. Runs on the caller's thread —
/// callers must already be off the main thread (CLAUDE.md invariant #9).
pub fn open_browser_window(app: &AppHandle, p: &prefs::Prefs) {
    if let Some(label) = build_strip_window(app, p, None) {
        add_tab(app, &label);
        log::info!("strip: window {label} ready");
    }
}

/// Build a strip window (OS window + 38px shell webview + frame) WITHOUT any
/// content tab, returning its label. Shared by `open_browser_window` (then adds
/// one tab) and session restore (then adds the saved tabs). `frame_override`,
/// when set, positions/sizes the window to a saved frame instead of the
/// first-window-restore / cascade default. Caller must be off the main thread.
fn build_strip_window(
    app: &AppHandle,
    p: &prefs::Prefs,
    frame_override: Option<[i64; 4]>,
) -> Option<String> {
    let state = app.state::<AppState>();
    let n = state.window_seq.fetch_add(1, Ordering::SeqCst) + 1;
    let label = format!("main-{n}");
    let is_first = windows::content_window_handles(app).is_empty();
    let host = windows::focused_or_recent_window_handle(app);

    let win = match WindowBuilder::new(app, &label)
        .title("Hermes WebUI")
        .inner_size(1280.0, 830.0)
        .theme(Some(windows::cached_theme(app)))
        .visible(false)
        .build()
    {
        Ok(w) => w,
        Err(e) => {
            log::error!("strip: window build failed: {e}");
            return None;
        }
    };

    state
        .window_modes
        .lock()
        .unwrap()
        .insert(label.clone(), p.connection_mode.clone());
    state
        .strip
        .lock()
        .unwrap()
        .insert(label.clone(), WindowTabs::default());

    // The strip webview — a bundled shell page with full IPC.
    let strip_label = format!("strip-{n}");
    let init = format!("window.__HERMES_WIN = '{label}';");
    let swb = WebviewBuilder::new(&strip_label, WebviewUrl::App("shell.html".into()))
        .initialization_script(&init);
    let scale = win.scale_factor().unwrap_or(1.0);
    let logical = win
        .inner_size()
        .map(|s| s.to_logical::<f64>(scale))
        .unwrap_or(LogicalSize::new(1280.0, 830.0));
    if let Err(e) = win.add_child(
        swb,
        LogicalPosition::new(0.0, 0.0),
        LogicalSize::new(logical.width, STRIP_HEIGHT),
    ) {
        log::error!("strip: strip webview failed: {e}");
    }

    log::info!("strip: window {label} built, strip webview added");

    // Frame: a saved frame (restore) wins; else first window restores the
    // persisted frame / centers, and others cascade off the host.
    if let Some([x, y, w, h]) = frame_override.filter(|f| f[2] >= 200 && f[3] >= 200) {
        let _ = win.set_size(tauri::PhysicalSize::new(w as u32, h as u32));
        let _ = win.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
    } else if is_first {
        if let Some((x, y, w, h)) = prefs::frame_load(app) {
            let _ = win.set_size(tauri::PhysicalSize::new(w, h));
            let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
        } else {
            // GTK no-ops center() on a not-yet-shown window (Linux smoke
            // finding: the window landed half off-screen). Compute the
            // centered position from the APP-level primary monitor — never
            // query the hidden window itself (monitor/size calls on an
            // unrealized GTK window are crash-prone).
            let centered = app.primary_monitor().ok().flatten().map(|mon| {
                let ms = mon.size();
                let mp = mon.position();
                let sf = mon.scale_factor();
                let ww = (1280.0 * sf) as u32;
                let wh = (830.0 * sf) as u32;
                tauri::PhysicalPosition::new(
                    mp.x + (ms.width.saturating_sub(ww) as i32) / 2,
                    mp.y + (ms.height.saturating_sub(wh) as i32) / 2,
                )
            });
            match centered {
                Some(pos) => {
                    let _ = win.set_position(pos);
                }
                None => {
                    let _ = win.center();
                }
            }
        }
        log::info!("strip: window {label} positioned");
    } else if let Some(host) = host {
        if let Ok(pos) = host.outer_position() {
            let _ = win.set_position(tauri::PhysicalPosition::new(pos.x + 28, pos.y + 28));
        }
    }

    // Show fallback if the first page load never completes.
    {
        let w2 = win.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(4));
            if !w2.is_visible().unwrap_or(true) {
                let _ = w2.show();
            }
        });
    }
    Some(label)
}

/// Recreate one saved strip window and its tabs (issue #18). Each tab reloads
/// its saved URL and is re-seeded with its `hermes_profile` cookie so it
/// reopens on the same profile. Caller must be off the main thread.
pub fn restore_window(app: &AppHandle, sw: &crate::session::SessionWindow) {
    if sw.tabs.is_empty() {
        return;
    }
    let p = prefs::load(app);
    let Some(label) = build_strip_window(app, &p, sw.frame) else {
        return;
    };
    for tab in &sw.tabs {
        let url = match url::Url::parse(&tab.url) {
            Ok(u) => u,
            Err(_) => match url::Url::parse(&p.target_url) {
                Ok(u) => u,
                Err(_) => continue,
            },
        };
        // Re-seed only the (non-sensitive) profile selector; fresh jar otherwise.
        let seed: Vec<Cookie<'static>> = if cfg!(not(target_os = "macos")) {
            tab.profile
                .as_deref()
                .map(|v| vec![crate::session::profile_cookie(v)])
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        add_tab_with(app, &label, url, seed);
    }
    // Select the saved active tab.
    let active_label = {
        let state = app.state::<AppState>();
        let strip = state.strip.lock().unwrap();
        strip
            .get(&label)
            .and_then(|e| e.tabs.get(sw.active.min(e.tabs.len().saturating_sub(1))))
            .map(|t| t.label.clone())
    };
    if let Some(al) = active_label {
        select_tab(app, &label, &al);
    }
    log::info!("strip: restored window {label} with {} tab(s)", sw.tabs.len());
}

/// Add a tab to an existing strip window, seeded from the currently-active
/// tab's cookie jar so it inherits the active profile + login (issue #3), then
/// diverges. Caller must be off the main thread (CLAUDE.md invariant #9).
pub fn add_tab(app: &AppHandle, window_label: &str) {
    let Some(win) = app.windows().get(window_label).cloned() else {
        return;
    };
    let p = prefs::load(app);
    if p.connection_mode == "ssh"
        && tunnel::current_status(app) != crate::state::TunnelStatus::Connected
    {
        return;
    }
    let target = match url::Url::parse(&p.target_url) {
        Ok(u) => u,
        Err(e) => {
            log::error!("strip: bad target URL: {e}");
            return;
        }
    };

    // Read the active tab's cookies BEFORE the new webview exists — the seed for
    // the fresh jar. Safe on this worker thread: the cookie dispatcher marshals
    // to the UI thread without the main-thread deadlock the docs warn about.
    //
    // cookies() (whole store), NOT cookies_for_url(): WKWebView's URL filter
    // drops host-only cookies (the WebUI sets `hermes_profile` host-only) — the
    // macOS inheritance bug (#3). A content tab only ever loads the one target
    // origin, so its whole store IS that origin's cookies — uniform + robust.
    let seed: Vec<Cookie<'static>> = if cfg!(not(target_os = "macos")) {
        let opener = {
            let state = app.state::<AppState>();
            let strip = state.strip.lock().unwrap();
            strip
                .get(window_label)
                .and_then(|e| e.tabs.get(e.active))
                .map(|t| t.label.clone())
        };
        match opener
            .and_then(|lbl| find_webview(&win, &lbl))
            .or_else(|| focused_active_webview(app))
        {
            Some(opener) => match opener.cookies() {
                Ok(cookies) => {
                    let names: Vec<&str> = cookies.iter().map(|c| c.name()).collect();
                    log::info!(
                        "strip: seeding new tab in {window_label} from opener — {} cookie(s): {:?}",
                        cookies.len(),
                        names
                    );
                    cookies
                }
                Err(e) => {
                    log::warn!("strip: seed read failed in {window_label} (opens on default profile): {e}");
                    Vec::new()
                }
            },
            None => {
                log::info!("strip: no opener in {window_label} (opens on default profile)");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    add_tab_with(app, window_label, target, seed);
}

/// Shared tab-creation body: build an isolated content webview for `target`,
/// seeded with `seed` cookies, append it to `window_label`'s strip and make it
/// active. Used by `add_tab` (seed = the opener's jar) and session restore
/// (seed = the saved profile selector). Caller must be off the main thread.
pub(crate) fn add_tab_with(
    app: &AppHandle,
    window_label: &str,
    target: url::Url,
    seed: Vec<Cookie<'static>>,
) {
    let Some(win) = app.windows().get(window_label).cloned() else {
        return;
    };
    let allowed_host = target.host_str().map(|h| h.to_lowercase());
    let state = app.state::<AppState>();
    let tab_label = {
        let mut strip = state.strip.lock().unwrap();
        let Some(entry) = strip.get_mut(window_label) else {
            return;
        };
        entry.tab_seq += 1;
        format!("tab-{}-{}", window_seq(window_label), entry.tab_seq)
    };

    let (r, g, b) = prefs::pre_paint_color(app);
    let hex = crate::theme::hex_string(r, g, b);
    // Paint the native webview surface in the cached theme color so a freshly
    // added tab never flashes white before its first paint (issue #4). On
    // Windows wry clamps any non-zero alpha to fully opaque — what we want.
    let to8 = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let bg = Color(to8(r), to8(g), to8(b), 255);
    // No injected ssh footer in strip mode — status lives in the strip.
    let init = bridge::init_script(&tab_label, &hex, false);

    // Per-tab cookie isolation (issue #3): each tab gets its own data partition
    // (separate jar) so the WebUI's HttpOnly `hermes_profile` selector is scoped
    // to the tab. macOS (HERMES_FORCE_STRIP) has no data_directory → no jar.
    let partition = if cfg!(not(target_os = "macos")) {
        tab_partition_dir(app, &tab_label)
    } else {
        None
    };
    if let Some(ref dir) = partition {
        // Guarantee a FRESH jar at the point of use — best-effort cleanup
        // elsewhere can leave a stale folder, and a recurring tab label would
        // then inherit a prior session's cookies.
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::create_dir_all(dir);
    }
    // The profile this tab opens on (the seeded `hermes_profile` value, if any)
    // — shown in the strip's profile dot immediately, before the first real
    // page load re-reads it (issue #8).
    let seed_profile = profile_from_cookies(&seed);

    // about:blank first so the seed lands before the real navigation (no race).
    let initial_url = if partition.is_some() {
        WebviewUrl::External(url::Url::parse("about:blank").unwrap())
    } else {
        WebviewUrl::External(target.clone())
    };

    let nav_app = app.clone();
    let load_window = window_label.to_string();
    let mut wb = WebviewBuilder::new(&tab_label, initial_url).background_color(bg);
    if let Some(ref dir) = partition {
        wb = wb.data_directory(dir.clone());
    }
    let wb = wb
        .initialization_script(&init)
        .on_navigation(move |url| {
            windows::navigation_allowed(&nav_app, url, allowed_host.as_deref())
        })
        // Native title source — replaces the JS EMIT('title') watcher, which
        // silently no-ops in remote-origin tab webviews (issue #15).
        .on_document_title_changed(|wv, title| {
            windows::apply_reported_title(wv.app_handle(), wv.label(), &title);
        })
        .on_page_load(move |webview, payload| {
            if !matches!(payload.event(), tauri::webview::PageLoadEvent::Finished) {
                return;
            }
            // Ignore the pre-seed about:blank load (isolated tabs only).
            if payload.url().scheme() == "about" {
                return;
            }
            let app = webview.app_handle().clone();
            let zoom = prefs::zoom_get(&app);
            if (zoom - 1.0).abs() > f64::EPSILON {
                let _ = webview.set_zoom(zoom);
            }
            // First successful load reveals the window (anti-flash).
            if let Some(w) = app.windows().get(&load_window) {
                if !w.is_visible().unwrap_or(true) {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
            // Re-read this tab's `hermes_profile` cookie after the real load
            // (the server may have just set/changed it) and refresh the strip's
            // profile dot (#8) + persist the session (#18). Off the wry callout:
            // the cookie read marshals via the dispatcher (wry#583 deadlock).
            let cap_app = app.clone();
            let cap_win = load_window.clone();
            let cap_tab = webview.label().to_string();
            std::thread::spawn(move || capture_tab_profile(&cap_app, &cap_win, &cap_tab));
        });

    let (pos, size) = content_bounds(&win);
    let webview = match win.add_child(wb, pos, size) {
        Ok(w) => w,
        Err(e) => {
            log::error!("strip: tab webview failed: {e}");
            return;
        }
    };

    // Seed the isolated jar, then load the real target — cookies are committed
    // before the navigation request fires (set_cookie is synchronous on
    // WebKitGTK; WebView2's AddOrUpdateCookie completes before navigate).
    if partition.is_some() {
        for cookie in seed {
            let _ = webview.set_cookie(cookie);
        }
        if let Err(e) = webview.navigate(target.clone()) {
            log::error!("strip: tab navigate failed: {e}");
        }
    }

    {
        let mut strip = state.strip.lock().unwrap();
        let Some(entry) = strip.get_mut(window_label) else {
            return;
        };
        if let Some(prev) = entry.tabs.get(entry.active) {
            if let Some(prev_wv) = find_webview(&win, &prev.label) {
                let _ = prev_wv.hide();
            }
        }
        entry.tabs.push(TabEntry {
            label: tab_label.clone(),
            title: "New Tab".into(),
            attention: false,
            profile: seed_profile,
        });
        entry.active = entry.tabs.len() - 1;
    }
    let _ = webview.set_focus();
    log::info!("strip: tab {tab_label} added to {window_label}");
    emit_tabs(app, window_label);
    refresh_window_title(app, window_label);
    crate::session::persist(app);
}

pub fn select_tab(app: &AppHandle, window_label: &str, tab_label: &str) {
    let Some(win) = app.windows().get(window_label).cloned() else {
        return;
    };
    let state = app.state::<AppState>();
    let (prev_label, ok) = {
        let mut strip = state.strip.lock().unwrap();
        let Some(entry) = strip.get_mut(window_label) else {
            return;
        };
        let Some(idx) = entry.tabs.iter().position(|t| t.label == tab_label) else {
            return;
        };
        let prev = entry.tabs.get(entry.active).map(|t| t.label.clone());
        entry.active = idx;
        (prev, true)
    };
    if !ok {
        return;
    }
    if let Some(prev) = prev_label {
        if prev != tab_label {
            if let Some(wv) = find_webview(&win, &prev) {
                let _ = wv.hide();
            }
        }
    }
    if let Some(wv) = find_webview(&win, tab_label) {
        // Linux: NEVER call set_position/set_size on an existing GTK child
        // webview — it crashes natively (isolated by Linux smoke v3/v5;
        // creation-time bounds render fine). Show/hide alone is safe.
        if !cfg!(target_os = "linux") {
            let (pos, size) = content_bounds(&win);
            let _ = wv.set_position(pos);
            let _ = wv.set_size(size);
        }
        let _ = wv.show();
        let _ = wv.set_focus();
    }
    emit_tabs(app, window_label);
    refresh_window_title(app, window_label);
    crate::session::persist(app);
}

pub fn close_tab(app: &AppHandle, window_label: &str, tab_label: &str) {
    let Some(win) = app.windows().get(window_label).cloned() else {
        return;
    };
    let state = app.state::<AppState>();
    let (remaining, next_active) = {
        let mut strip = state.strip.lock().unwrap();
        let Some(entry) = strip.get_mut(window_label) else {
            return;
        };
        if entry.tabs.len() <= 1 {
            // Last tab: close the whole window (D11 path handles quit).
            drop(strip);
            let _ = win.close();
            return;
        }
        let Some(idx) = entry.tabs.iter().position(|t| t.label == tab_label) else {
            return;
        };
        entry.tabs.remove(idx);
        if entry.active >= entry.tabs.len() {
            entry.active = entry.tabs.len() - 1;
        } else if idx < entry.active {
            entry.active -= 1;
        }
        (
            entry.tabs.len(),
            entry.tabs.get(entry.active).map(|t| t.label.clone()),
        )
    };
    state.raw_titles.lock().unwrap().remove(tab_label);
    if let Some(wv) = find_webview(&win, tab_label) {
        let _ = wv.close();
    }
    remove_tab_partition(app, tab_label);
    if let Some(next) = next_active {
        select_tab(app, window_label, &next);
    }
    log::info!("strip: closed {tab_label}, {remaining} tabs remain in {window_label}");
    crate::session::persist(app);
}

pub fn close_tab_by_label(app: &AppHandle, tab_label: &str) {
    if let Some(window_label) = window_of_tab(tab_label) {
        close_tab(app, &window_label, tab_label);
    }
}

pub fn cycle_tab(app: &AppHandle, tab_label: &str, forward: bool) {
    let Some(window_label) = window_of_tab(tab_label) else {
        return;
    };
    let state = app.state::<AppState>();
    let next = {
        let strip = state.strip.lock().unwrap();
        let Some(entry) = strip.get(&window_label) else {
            return;
        };
        if entry.tabs.len() < 2 {
            return;
        }
        let len = entry.tabs.len();
        let idx = if forward {
            (entry.active + 1) % len
        } else {
            (entry.active + len - 1) % len
        };
        entry.tabs[idx].label.clone()
    };
    select_tab(app, &window_label, &next);
}

/// The active tab webview of the focused (or most recent) strip window.
pub fn focused_active_webview(app: &AppHandle) -> Option<Webview<Wry>> {
    let handles = windows::content_window_handles(app);
    let win = handles
        .iter()
        .find(|w| w.is_focused().unwrap_or(false))
        .or_else(|| handles.last())?
        .clone();
    let state = app.state::<AppState>();
    let label = {
        let strip = state.strip.lock().unwrap();
        let entry = strip.get(win.label())?;
        entry.tabs.get(entry.active)?.label.clone()
    };
    find_webview(&win, &label)
}

/// Every content (tab) webview across all strip windows.
pub fn all_tab_webviews(app: &AppHandle) -> Vec<Webview<Wry>> {
    windows::content_window_handles(app)
        .iter()
        .flat_map(|w| w.webviews())
        .filter(|wv| wv.label().starts_with("tab-"))
        .collect()
}

/// Capture every strip window as a session window (issue #18). Each tab's live
/// URL is read from its webview; order/active/profile come from the registry.
/// Must run on the main thread (the webview URL getter touches the runtime).
pub fn session_windows(app: &AppHandle) -> Vec<crate::session::SessionWindow> {
    use crate::session::{SessionTab, SessionWindow};
    let p = prefs::load(app);
    let state = app.state::<AppState>();
    let mut out = Vec::new();
    for win in windows::content_window_handles(app) {
        let label = win.label().to_string();
        let (tabs_meta, active) = {
            let strip = state.strip.lock().unwrap();
            match strip.get(&label) {
                Some(e) => (e.tabs.clone(), e.active),
                None => continue,
            }
        };
        let tabs: Vec<SessionTab> = tabs_meta
            .iter()
            .map(|t| {
                let url = find_webview(&win, &t.label)
                    .and_then(|wv| crate::session::capture_url(|| wv.url()))
                    .unwrap_or_else(|| p.target_url.clone());
                SessionTab {
                    url,
                    profile: t.profile.clone(),
                }
            })
            .collect();
        if tabs.is_empty() {
            continue;
        }
        out.push(SessionWindow {
            frame: crate::session::frame_of(&win),
            active: active.min(tabs.len() - 1),
            tabs,
        });
    }
    out
}

/// Recompute strip + active tab bounds (window Resized handler).
pub fn layout(app: &AppHandle, window_label: &str) {
    // Linux: re-fitting GTK child webviews crashes natively (smoke v3/v5
    // finding) — skip entirely. Cost: window resizes don't re-fit webviews
    // there yet; tracked for the next sprint (upstream wry GTK geometry).
    if cfg!(target_os = "linux") {
        return;
    }
    let Some(win) = app.windows().get(window_label).cloned() else {
        return;
    };
    let scale = win.scale_factor().unwrap_or(1.0);
    let Ok(size) = win.inner_size() else { return };
    let logical = size.to_logical::<f64>(scale);
    log::debug!(
        "strip: layout {window_label} inner={}x{} scale={scale} logical={}x{}",
        size.width,
        size.height,
        logical.width,
        logical.height
    );
    let strip_label = format!("strip-{}", window_seq(window_label));
    if let Some(strip_wv) = find_webview(&win, &strip_label) {
        let _ = strip_wv.set_position(LogicalPosition::new(0.0, 0.0));
        let _ = strip_wv.set_size(LogicalSize::new(logical.width, STRIP_HEIGHT));
    }
    let state = app.state::<AppState>();
    let active = {
        let strip = state.strip.lock().unwrap();
        strip
            .get(window_label)
            .and_then(|e| e.tabs.get(e.active))
            .map(|t| t.label.clone())
    };
    if let Some(active) = active {
        if let Some(wv) = find_webview(&win, &active) {
            let (pos, sz) = content_bounds(&win);
            let _ = wv.set_position(pos);
            let _ = wv.set_size(sz);
        }
    }
}

/// A tab reported a new title (marker-free) and its pending-attention state.
/// Called from `windows::apply_reported_title`, which sources both from wry's
/// native title-changed hook and has already stripped the "● " marker.
pub fn set_tab_title(app: &AppHandle, tab_label: &str, title: &str, attention: bool) {
    let Some(window_label) = window_of_tab(tab_label) else {
        return;
    };
    let state = app.state::<AppState>();
    state
        .raw_titles
        .lock()
        .unwrap()
        .insert(tab_label.to_string(), title.to_string());
    {
        let mut strip = state.strip.lock().unwrap();
        if let Some(entry) = strip.get_mut(&window_label) {
            if let Some(tab) = entry.tabs.iter_mut().find(|t| t.label == tab_label) {
                let p = prefs::load(app);
                tab.title = windows::display_title(title, &p.connection_mode, &p.target_url, true);
                tab.attention = attention;
            }
        }
    }
    emit_tabs(app, &window_label);
    refresh_window_title(app, &window_label);
}

/// Extract the `hermes_profile` cookie value from a cookie set. `None` means
/// the default profile (the WebUI sets no such cookie for it).
pub fn profile_from_cookies(cookies: &[Cookie<'static>]) -> Option<String> {
    cookies
        .iter()
        .find(|c| c.name() == "hermes_profile")
        .map(|c| c.value().to_string())
}

/// Read a tab's current profile from its isolated cookie jar; if it changed,
/// update the registry, repaint the strip (profile dot — #8), and persist the
/// session (#18). Runs on a worker thread — the cookie read marshals via the
/// dispatcher, so it must not be called from inside a wry callout (wry#583).
fn capture_tab_profile(app: &AppHandle, window_label: &str, tab_label: &str) {
    let Some(win) = app.windows().get(window_label).cloned() else {
        return;
    };
    let Some(wv) = find_webview(&win, tab_label) else {
        return;
    };
    let profile = match wv.cookies() {
        Ok(cs) => profile_from_cookies(&cs),
        Err(e) => {
            log::debug!("strip: profile read failed for {tab_label}: {e}");
            return;
        }
    };
    let changed = {
        let state = app.state::<AppState>();
        let mut strip = state.strip.lock().unwrap();
        match strip
            .get_mut(window_label)
            .and_then(|e| e.tabs.iter_mut().find(|t| t.label == tab_label))
        {
            Some(tab) if tab.profile != profile => {
                tab.profile = profile;
                true
            }
            _ => false,
        }
    };
    if changed {
        emit_tabs(app, window_label);
        crate::session::persist(app);
    }
}

/// Move a tab to a new index within its window's strip (drag-to-reorder, #19).
/// The visible webview is unchanged — only the display order and the stored
/// `Vec` order move; `active` keeps pointing at the same tab label.
pub fn reorder_tab(app: &AppHandle, window_label: &str, tab_label: &str, new_index: usize) {
    let state = app.state::<AppState>();
    let moved = {
        let mut strip = state.strip.lock().unwrap();
        let Some(entry) = strip.get_mut(window_label) else {
            return;
        };
        if entry.tabs.is_empty() {
            return;
        }
        let Some(from) = entry.tabs.iter().position(|t| t.label == tab_label) else {
            return;
        };
        let to = new_index.min(entry.tabs.len() - 1);
        if from == to {
            return;
        }
        let active_label = entry.tabs.get(entry.active).map(|t| t.label.clone());
        let tab = entry.tabs.remove(from);
        entry.tabs.insert(to, tab);
        if let Some(al) = active_label {
            if let Some(idx) = entry.tabs.iter().position(|t| t.label == al) {
                entry.active = idx;
            }
        }
        true
    };
    if moved {
        emit_tabs(app, window_label);
        crate::session::persist(app);
    }
}

fn refresh_window_title(app: &AppHandle, window_label: &str) {
    let Some(win) = app.windows().get(window_label).cloned() else {
        return;
    };
    let state = app.state::<AppState>();
    let raw = {
        let strip = state.strip.lock().unwrap();
        strip
            .get(window_label)
            .and_then(|e| e.tabs.get(e.active))
            .and_then(|t| state.raw_titles.lock().unwrap().get(&t.label).cloned())
            .unwrap_or_default()
    };
    let p = prefs::load(app);
    let healthy = state.healthy.load(Ordering::SeqCst);
    let _ = win.set_title(&windows::display_title(
        &raw,
        &p.connection_mode,
        &p.target_url,
        healthy,
    ));
}

pub fn refresh_all_titles(app: &AppHandle) {
    let labels: Vec<String> = {
        let state = app.state::<AppState>();
        let strip = state.strip.lock().unwrap();
        strip.keys().cloned().collect()
    };
    for label in labels {
        refresh_window_title(app, &label);
    }
}

pub fn snapshot(app: &AppHandle, window_label: &str) -> serde_json::Value {
    let state = app.state::<AppState>();
    let strip = state.strip.lock().unwrap();
    let p = prefs::load(app);
    match strip.get(window_label) {
        Some(entry) => json!({
            "window": window_label,
            "tabs": entry.tabs,
            "active": entry.active,
            "mode": p.connection_mode,
            "target": p.target_url,
        }),
        None => json!({ "window": window_label, "tabs": [], "active": 0 }),
    }
}

pub fn emit_tabs(app: &AppHandle, window_label: &str) {
    let _ = app.emit("tabs-changed", snapshot(app, window_label));
}

/// Window destroyed: drop its registry entries.
pub fn forget_window(app: &AppHandle, window_label: &str) {
    let state = app.state::<AppState>();
    let removed = state.strip.lock().unwrap().remove(window_label);
    if let Some(entry) = removed {
        {
            let mut titles = state.raw_titles.lock().unwrap();
            for tab in &entry.tabs {
                titles.remove(&tab.label);
            }
        }
        for tab in &entry.tabs {
            remove_tab_partition(app, &tab.label);
        }
    }
}
