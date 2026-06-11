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
        let state = app.state::<AppState>();
        state.connecting.store(false, Ordering::SeqCst);
        // SSH connect failed → arm auto-recovery (the in-run set_status hook
        // is suppressed while `connecting` is held, so arm it here).
        if prefs::load(&app).connection_mode == "ssh"
            && *state.tunnel_status.lock().unwrap() == crate::state::TunnelStatus::Disconnected
        {
            start_ssh_recovery(&app);
        }
    });
}

/// SSH-mode auto-recovery — the Swift app's NWPathMonitor reconnect (fix #38)
/// translated: while the tunnel is down, watch for the SSH host's TCP port to
/// become reachable and rerun the orchestrator the moment it is (covers
/// laptop sleep/wake, Wi-Fi drops, VPN flaps). Because port 22 may not be the
/// real ssh port (~/.ssh/config), there's also a blind retry every 60s.
pub fn start_ssh_recovery(app: &AppHandle) {
    use std::net::{TcpStream, ToSocketAddrs};
    let state = app.state::<AppState>();
    if state.connecting.load(Ordering::SeqCst) {
        return;
    }
    let generation = state.recovery_gen.fetch_add(1, Ordering::SeqCst) + 1;
    log::info!("recovery(ssh): armed");
    let app = app.clone();
    std::thread::spawn(move || {
        let mut ticks: u32 = 0;
        loop {
            std::thread::sleep(Duration::from_secs(10));
            ticks += 1;
            let state = app.state::<AppState>();
            if state.recovery_gen.load(Ordering::SeqCst) != generation
                || state.connecting.load(Ordering::SeqCst)
            {
                return;
            }
            if *state.tunnel_status.lock().unwrap() != crate::state::TunnelStatus::Disconnected {
                return;
            }
            let p = prefs::load(&app);
            if p.connection_mode != "ssh" {
                return;
            }
            let host = p.ssh_host.trim().to_string();
            let reachable = (host.as_str(), 22u16)
                .to_socket_addrs()
                .ok()
                .and_then(|mut addrs| addrs.next())
                .map(|addr| TcpStream::connect_timeout(&addr, Duration::from_secs(3)).is_ok())
                .unwrap_or(false);
            if reachable || ticks.is_multiple_of(6) {
                log::info!("recovery(ssh): attempting reconnect (host reachable: {reachable})");
                reconnect(&app);
                return;
            }
        }
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
    let content = windows::content_window_handles(app);
    let reuse = !content.is_empty() && windows::all_modes_match(app, &p.connection_mode);
    if reuse {
        for w in &content {
            let _ = w.hide();
        }
    } else {
        for w in &content {
            windows::forget(app, w.label());
            crate::strip::forget_window(app, w.label());
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
            windows::eval_all_content(app, "location.reload();");
            let content = windows::content_window_handles(app);
            for w in &content {
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
        for w in windows::content_window_handles(app) {
            windows::forget(app, w.label());
            crate::strip::forget_window(app, w.label());
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
            // Strip pages (Windows/Linux) show the health dot from this event.
            use tauri::Emitter;
            let _ = app.emit("health-changed", serde_json::json!({ "healthy": healthy }));
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
///
/// Threading is platform-split:
/// - Windows/Linux: a worker thread — WebView2 stalls forever when a webview
///   is created synchronously inside a main-thread command/event handler
///   (v0.1.2 bug, CLAUDE.md invariant #9).
/// - macOS: INLINE on the calling (main) thread — menu/Dock handlers already
///   run there, window creation on main is plain AppKit usage, and it avoids
///   pointless worker→main blocking round-trips. NOTE the actual Cmd+T
///   freeze fix lives in macos::add_tabbed_window (GCD main-queue deferral,
///   CLAUDE.md invariant #12) — tab attach must run outside tao's event
///   dispatch no matter which thread builds the window.
pub fn open_new_session(app: &AppHandle, as_tab: bool) {
    let app = app.clone();
    let work = move || {
        let p = prefs::load(&app);
        if p.connection_mode == "ssh"
            && tunnel::current_status(&app) != crate::state::TunnelStatus::Connected
        {
            return;
        }
        if p.connection_mode == "direct" && windows::content_window_handles(&app).is_empty() {
            reconnect(&app);
            return;
        }
        // Strip mode: "new tab" adds a tab to the focused window's strip.
        if as_tab && crate::strip::enabled() {
            if let Some(win) = windows::focused_or_recent_window_handle(&app) {
                crate::strip::add_tab(&app, win.label());
                return;
            }
        }
        windows::open_browser(&app, &p, as_tab);
    };
    #[cfg(target_os = "macos")]
    work();
    #[cfg(not(target_os = "macos"))]
    std::thread::spawn(work);
}
