//! Connection orchestrator — port of AppDelegate.startTunnel (docs/04 § state
//! machine): splash → (ssh tunnel | direct preflight) → reuse-or-open windows
//! | error window. Single-flight guarded.

use crate::state::AppState;
use crate::{health, prefs, tunnel, windows};
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::{AppHandle, Manager};

pub fn reconnect(app: &AppHandle) {
    let state = app.state::<AppState>();
    if state.connecting.swap(true, Ordering::SeqCst) {
        log::info!("conn: reconnect already in flight, ignoring");
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        run(&app);
        app.state::<AppState>()
            .connecting
            .store(false, Ordering::SeqCst);
    });
}

fn run(app: &AppHandle) {
    let p = prefs::load(app);
    log::info!(
        "conn: connecting (mode={}, target={})",
        p.connection_mode,
        p.target_url
    );
    let state = app.state::<AppState>();
    *state.last_error_hint.lock().unwrap() = String::new();

    // Invalidate background loops from the previous connection.
    state.health_gen.fetch_add(1, Ordering::SeqCst);
    state.recovery_gen.fetch_add(1, Ordering::SeqCst);

    windows::show_splash(app, &p);
    if let Some(w) = app.get_webview_window("error") {
        let _ = w.destroy();
    }
    tunnel::stop(app);

    // Reuse-vs-rebuild (fix #10 in the Swift app): same-mode reconnects keep
    // every webview alive (cookies, scroll, in-flight chat); a mode switch
    // rebuilds because the footer chrome differs.
    let content = windows::content_windows(app);
    let reuse = !content.is_empty() && windows::all_modes_match(app, &p.connection_mode);
    if reuse {
        for w in &content {
            let _ = w.hide();
        }
    } else {
        for w in &content {
            windows::forget(app, w.label());
            let _ = w.destroy();
        }
    }

    let ok = if p.connection_mode == "ssh" {
        let lp: u32 = p.local_port.trim().parse().unwrap_or(8787);
        let rp: u32 = p.remote_port.trim().parse().unwrap_or(8787);
        tunnel::start(app, p.ssh_user.trim(), p.ssh_host.trim(), lp, rp)
    } else {
        let reachable = health::http_reachable(&p.target_url, Duration::from_secs(4));
        if !reachable {
            *state.last_error_hint.lock().unwrap() = format!(
                "Nothing answered at {}. Is hermes-webui running? (./start.sh)",
                p.target_url
            );
        }
        reachable
    };

    // The Swift app lingers half a beat so the splash doesn't strobe.
    std::thread::sleep(Duration::from_millis(500));
    windows::close_splash(app);

    if ok {
        if reuse {
            let content = windows::content_windows(app);
            for w in &content {
                let _ = w.eval("location.reload();");
                let _ = w.show();
            }
            if let Some(last) = content.last() {
                let _ = last.set_focus();
            }
        } else {
            windows::open_browser(app, &p, false);
        }
        windows::set_offline_badge(app, false);
        if p.connection_mode == "direct" {
            start_health_loop(app, p.target_url.clone());
        }
        log::info!("conn: connected");
    } else {
        for w in windows::content_windows(app) {
            windows::forget(app, w.label());
            let _ = w.destroy();
        }
        windows::show_error(app, &p);
        windows::set_offline_badge(app, true);
        log::warn!("conn: failed — showing error window");
        if p.connection_mode == "direct" {
            start_recovery_loop(app, p.target_url.clone());
        }
    }
}

/// Direct-mode health polling: GET /health every 30s (Swift fix #29).
fn start_health_loop(app: &AppHandle, target: String) {
    let state = app.state::<AppState>();
    let generation = state.health_gen.fetch_add(1, Ordering::SeqCst) + 1;
    state.healthy.store(true, Ordering::SeqCst);
    windows::refresh_all_titles(app);
    let app = app.clone();
    let url = health::health_url(&target);
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(30));
        let state = app.state::<AppState>();
        if state.health_gen.load(Ordering::SeqCst) != generation {
            return;
        }
        let healthy = health::http_reachable(&url, Duration::from_secs(5));
        let was = state.healthy.swap(healthy, Ordering::SeqCst);
        if was != healthy {
            log::info!("health: {} -> {}", was, healthy);
            windows::set_offline_badge(&app, !healthy);
            windows::refresh_all_titles(&app);
        }
    });
}

/// Error-state auto-recovery (divergence F-08, direct mode): probe the target
/// every 5s while the error window is up; reconnect on first success.
fn start_recovery_loop(app: &AppHandle, target: String) {
    let state = app.state::<AppState>();
    let generation = state.recovery_gen.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(5));
        let state = app.state::<AppState>();
        if state.recovery_gen.load(Ordering::SeqCst) != generation {
            return;
        }
        if app.get_webview_window("error").is_none() {
            return;
        }
        if health::http_reachable(&target, Duration::from_secs(3)) {
            log::info!("recovery: target answered — auto-reconnecting");
            reconnect(&app);
            return;
        }
    });
}

/// Guarded entry for New Window / New Tab (port of openNewBrowserSession).
pub fn open_new_session(app: &AppHandle, as_tab: bool) {
    let p = prefs::load(app);
    if p.connection_mode == "ssh"
        && tunnel::current_status(app) != crate::state::TunnelStatus::Connected
    {
        return;
    }
    if p.connection_mode == "direct" && windows::content_windows(app).is_empty() {
        reconnect(app);
        return;
    }
    windows::open_browser(app, &p, as_tab);
}
