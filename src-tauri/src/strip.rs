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
use std::sync::atomic::Ordering;
use tauri::webview::WebviewBuilder;
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

/// Open a new strip window with its first tab. Runs on the caller's thread —
/// callers must already be off the main thread (CLAUDE.md invariant #9).
pub fn open_browser_window(app: &AppHandle, p: &prefs::Prefs) {
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
            return;
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

    // Frame: first window restores persisted frame / centers, others cascade.
    if is_first {
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

    add_tab(app, &label);
    log::info!("strip: window {label} ready");

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
}

/// Add a tab to an existing strip window. Caller must be off the main thread.
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
    // No injected ssh footer in strip mode — status lives in the strip.
    let init = bridge::init_script(&tab_label, &hex, false);

    let nav_app = app.clone();
    let load_window = window_label.to_string();
    let wb = WebviewBuilder::new(&tab_label, WebviewUrl::External(target))
        .initialization_script(&init)
        .on_navigation(move |url| {
            windows::navigation_allowed(&nav_app, url, allowed_host.as_deref())
        })
        .on_page_load(move |webview, payload| {
            if !matches!(payload.event(), tauri::webview::PageLoadEvent::Finished) {
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
        });

    let (pos, size) = content_bounds(&win);
    let webview = match win.add_child(wb, pos, size) {
        Ok(w) => w,
        Err(e) => {
            log::error!("strip: tab webview failed: {e}");
            return;
        }
    };

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
        });
        entry.active = entry.tabs.len() - 1;
    }
    let _ = webview.set_focus();
    log::info!("strip: tab {tab_label} added to {window_label}");
    emit_tabs(app, window_label);
    refresh_window_title(app, window_label);
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
        let (pos, size) = content_bounds(&win);
        let _ = wv.set_position(pos);
        let _ = wv.set_size(size);
        let _ = wv.show();
        let _ = wv.set_focus();
    }
    emit_tabs(app, window_label);
    refresh_window_title(app, window_label);
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
    if let Some(next) = next_active {
        select_tab(app, window_label, &next);
    }
    log::info!("strip: closed {tab_label}, {remaining} tabs remain in {window_label}");
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

/// Recompute strip + active tab bounds (window Resized handler).
pub fn layout(app: &AppHandle, window_label: &str) {
    let Some(win) = app.windows().get(window_label).cloned() else {
        return;
    };
    let scale = win.scale_factor().unwrap_or(1.0);
    let Ok(size) = win.inner_size() else { return };
    let logical = size.to_logical::<f64>(scale);
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

/// Title reported by a tab's title-watcher script.
pub fn set_tab_title(app: &AppHandle, tab_label: &str, raw: &str) {
    let Some(window_label) = window_of_tab(tab_label) else {
        return;
    };
    let state = app.state::<AppState>();
    state
        .raw_titles
        .lock()
        .unwrap()
        .insert(tab_label.to_string(), raw.to_string());
    {
        let mut strip = state.strip.lock().unwrap();
        if let Some(entry) = strip.get_mut(&window_label) {
            if let Some(tab) = entry.tabs.iter_mut().find(|t| t.label == tab_label) {
                let p = prefs::load(app);
                tab.title = windows::display_title(raw, &p.connection_mode, &p.target_url, true);
            }
        }
    }
    emit_tabs(app, &window_label);
    refresh_window_title(app, &window_label);
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
        let mut titles = state.raw_titles.lock().unwrap();
        for tab in entry.tabs {
            titles.remove(&tab.label);
        }
    }
}
