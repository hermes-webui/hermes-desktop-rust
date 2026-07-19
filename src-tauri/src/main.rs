// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bridge;
mod conn;
mod health;
mod macos;
mod menu;
mod paste;
mod prefs;
mod session;
mod state;
mod strip;
mod theme;
mod tunnel;
mod updater;
mod windows;

use state::AppState;
use tauri::Manager;

/// The shipped changelog, bundled at build time so "What's New" works offline
/// (issue #6). Repo-root CHANGELOG.md, relative to this file (src-tauri/src/).
const CHANGELOG_MD: &str = include_str!("../../CHANGELOG.md");

/// Extract the `## [vX.Y.Z]` section body for `version` from the changelog
/// (everything up to the next `## ` heading). None if absent.
fn changelog_section(md: &str, version: &str) -> Option<String> {
    let header = format!("## [v{version}]");
    let start = md.lines().position(|l| l.starts_with(&header))?;
    let body: Vec<&str> = md
        .lines()
        .skip(start + 1)
        .take_while(|l| !l.starts_with("## "))
        .collect();
    let trimmed = body.join("\n").trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

#[tauri::command]
fn get_prefs(app: tauri::AppHandle) -> prefs::Prefs {
    prefs::load(&app)
}

#[tauri::command]
fn set_prefs(app: tauri::AppHandle, new_prefs: prefs::Prefs) -> Result<(), String> {
    prefs::validate(&new_prefs)?;
    prefs::save(&app, &new_prefs);
    if let Some(w) = app.get_webview_window("prefs") {
        let _ = w.destroy();
    }
    conn::reconnect(&app);
    Ok(())
}

#[tauri::command]
async fn test_connection(url: String) -> bool {
    tauri::async_runtime::spawn_blocking(move || {
        health::http_reachable(&url, std::time::Duration::from_secs(5))
    })
    .await
    .unwrap_or(false)
}

#[tauri::command]
fn retry_connect(app: tauri::AppHandle) {
    conn::reconnect(&app);
}

#[tauri::command]
fn open_preferences(app: tauri::AppHandle) {
    windows::open_prefs(&app);
}

#[tauri::command]
fn open_whats_new(app: tauri::AppHandle) {
    windows::open_whats_new(&app);
}

#[tauri::command]
fn open_releases_page() {
    let _ = tauri_plugin_opener::open_url(
        "https://github.com/hermes-webui/hermes-desktop-rust/releases",
        None::<&str>,
    );
}

/// Current version + this version's changelog section (issue #6).
#[tauri::command]
fn whats_new(app: tauri::AppHandle) -> serde_json::Value {
    let version = app.package_info().version.to_string();
    let body = changelog_section(CHANGELOG_MD, &version)
        .unwrap_or_else(|| "Release notes are on the GitHub releases page.".to_string());
    serde_json::json!({ "version": version, "body": body })
}

#[tauri::command]
fn close_prefs(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("prefs") {
        let _ = w.destroy();
    }
}

// ---- Strip-mode (Windows/Linux tab bar) commands. Mutating ops run on a
// worker thread: webview creation/destruction must stay off the main thread
// inside commands (CLAUDE.md invariant #9). ----

#[tauri::command]
fn tabs_snapshot(app: tauri::AppHandle, window: String) -> serde_json::Value {
    strip::snapshot(&app, &window)
}

#[tauri::command]
fn tab_new(app: tauri::AppHandle, window: String) {
    std::thread::spawn(move || strip::add_tab(&app, &window));
}

#[tauri::command]
fn tab_select(app: tauri::AppHandle, window: String, tab: String) {
    std::thread::spawn(move || strip::select_tab(&app, &window, &tab));
}

#[tauri::command]
fn tab_close(app: tauri::AppHandle, window: String, tab: String) {
    std::thread::spawn(move || strip::close_tab(&app, &window, &tab));
}

#[tauri::command]
fn tab_reorder(app: tauri::AppHandle, window: String, tab: String, index: usize) {
    std::thread::spawn(move || strip::reorder_tab(&app, &window, &tab, index));
}

#[tauri::command]
fn tab_rename(app: tauri::AppHandle, window: String, tab: String, name: Option<String>) {
    std::thread::spawn(move || strip::rename_tab(&app, &window, &tab, name));
}

#[tauri::command]
fn new_window_cmd(app: tauri::AppHandle) {
    conn::open_new_session(&app, false);
}

/// Set a custom color for a profile's dot (issue #47); empty/invalid color
/// clears the override. Persists to prefs and repaints the strip.
#[tauri::command]
fn set_profile_color(app: tauri::AppHandle, name: String, color: String) {
    prefs::set_profile_color(&app, &name, &color);
    // Repaint every strip window so the new color shows immediately.
    for label in strip::window_labels(&app) {
        strip::emit_tabs(&app, &label);
    }
}

#[tauri::command]
fn strip_menu(app: tauri::AppHandle, window: String) {
    // Pop the "⋯" menu on the MAIN event-loop thread, not inline in this IPC
    // command (issue #33). On Windows `Menu::popup` enters a modal
    // `TrackPopupMenu` loop; running it from the IPC command thread wedges the
    // WebView2 message pump → the menu sticks, the window gets stuck topmost,
    // and Preferences/Quit stop firing (AppHangB1). Marshaled onto the event
    // loop, the modal loop pumps normally and the command returns immediately.
    //
    // `run_on_main_thread` (not the GCD `dispatch_main_async` of invariant #12)
    // is fine because the strip — and thus this command — is Windows/Linux only
    // (macOS uses native tabs; it's reachable on macOS only via the dev
    // HERMES_FORCE_STRIP). A context-menu popup doesn't force a window redraw
    // the way addTabbedWindow does, so there's no draw_rect re-entrancy here. If
    // the strip ever ships on macOS, route this popup through dispatch_main_async.
    //
    // Marshaling the popup onto the event loop (above) was NOT enough to fix #33
    // on its own: `popup` still runs a nested `TrackPopupMenu` modal loop that
    // owns the main thread until dismissed, and the periodic 4s autosave +
    // profile-dot timer reads each tab's cookie/URL (which marshal back onto the
    // main thread) — that re-entry during the modal loop is the actual deadlock
    // (b3nw on v0.6.0: "2-3 seconds after the 3 dots is open, doesn't matter
    // what you hover/select"). Flag `menu_open` for the duration so the timer
    // skips its webview reads while the menu is up and catches up once it closes.
    let _ = app.clone().run_on_main_thread(move || {
        use std::sync::atomic::Ordering;
        use tauri::menu::ContextMenu;
        let Some(win) = app.windows().get(&window).cloned() else {
            return;
        };
        if let Ok(menu) = menu::build_strip_menu(&app) {
            let state = app.state::<AppState>();
            state.menu_open.store(true, Ordering::SeqCst);
            // Reset on scope exit, even if `popup` were to panic — a stuck
            // `menu_open` would silently freeze session autosave + the profile
            // dot until restart. (`popup` returns a Result we already ignore.)
            struct Reset<'a>(&'a std::sync::atomic::AtomicBool);
            impl Drop for Reset<'_> {
                fn drop(&mut self) {
                    self.0.store(false, std::sync::atomic::Ordering::SeqCst);
                }
            }
            let _reset = Reset(&state.menu_open);
            let _ = menu.popup(win);
        }
    });
}

#[tauri::command]
fn get_boot_info(app: tauri::AppHandle) -> serde_json::Value {
    let p = prefs::load(&app);
    let state = app.state::<AppState>();
    let hint = state.last_error_hint.lock().unwrap().clone();
    let (r, g, b) = prefs::pre_paint_color(&app);
    serde_json::json!({
        "mode": p.connection_mode,
        "target": p.target_url,
        "sshHost": p.ssh_host,
        "sshUser": p.ssh_user,
        "errorHint": hint,
        "bgHex": theme::hex_string(r, g, b),
        "isDark": theme::is_dark(r, g, b),
    })
}

fn main() {
    // Linux/X11: switch Xlib to thread-safe mode BEFORE any other X call in
    // the process — must stay the first statement, ahead of GTK/GDK init.
    // WebKitGTK's internal threads talk X11 directly; without this they race
    // the main loop and intermittently corrupt Xlib's reply stream at startup
    // ("[xcb] Unknown sequence number while awaiting reply …
    // xcb_xlib_threads_sequence_lost" abort, or a silent fatal-IO exit(1) —
    // 5 of 7 Linux smoke runs on identical code). dlopen via x11-dl so
    // Wayland-only systems without libX11 simply skip it.
    #[cfg(target_os = "linux")]
    if let Ok(xlib) = x11_dl::xlib::Xlib::open() {
        unsafe { (xlib.XInitThreads)() };
    }

    // Linux safe-render escape hatch (issue #78): on some GPU/driver combos
    // (NVIDIA proprietary, some VMs, Fedora+EGL mismatches) WebKitGTK's DMABUF
    // renderer fails hard — "Could not create default EGL display:
    // EGL_BAD_PARAMETER" and a blank window, app unusable. Setting
    // HERMES_DESKTOP_SAFE_RENDER=1 flips WebKitGTK to its safe paths BEFORE
    // any webview initializes. Explicit user-set WEBKIT_* values always win.
    #[cfg(target_os = "linux")]
    if std::env::var("HERMES_DESKTOP_SAFE_RENDER").is_ok_and(|v| v == "1") {
        for (k, v) in [
            ("WEBKIT_DISABLE_DMABUF_RENDERER", "1"),
            ("WEBKIT_DISABLE_COMPOSITING_MODE", "1"),
        ] {
            if std::env::var(k).is_err() {
                std::env::set_var(k, v);
            }
        }
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // Second launch (e.g. double-opening the .app/.exe): focus the
            // existing window instead of a new process.
            windows::show_most_recent(app);
        }))
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .targets([
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::LogDir {
                        file_name: Some("hermes-webui-desktop".into()),
                    }),
                ])
                .build(),
        )
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        windows::show_most_recent(app);
                    }
                })
                .build(),
        )
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            get_prefs,
            set_prefs,
            test_connection,
            retry_connect,
            open_preferences,
            open_whats_new,
            whats_new,
            open_releases_page,
            close_prefs,
            get_boot_info,
            tabs_snapshot,
            tab_new,
            tab_select,
            tab_close,
            tab_reorder,
            tab_rename,
            new_window_cmd,
            set_profile_color,
            strip_menu
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            // App-wide appearance from the theme cache before any window
            // opens (Swift: loadCachedTheme at applicationDidFinishLaunching;
            // defaults dark). Without this, the chrome (menus, native tab
            // bar) follows the OS appearance instead of the page theme.
            handle.set_theme(Some(windows::cached_theme(&handle)));
            prefs::seed_if_needed(&handle);
            #[cfg(target_os = "macos")]
            {
                let m = menu::build(&handle)?;
                app.set_menu(m)?;
            }
            bridge::install(&handle);
            // Wipe stale per-tab cookie partitions from a prior run before any
            // tab opens (issue #3 — partitions are session-scoped).
            strip::clear_partitions(&handle);
            {
                use tauri_plugin_global_shortcut::GlobalShortcutExt;
                // Conflict with the Swift app's identical default is expected
                // when both run — log and continue (docs/11 § Coexistence).
                if let Err(e) = handle.global_shortcut().register("CmdOrCtrl+Shift+H") {
                    log::warn!("global shortcut unavailable: {e}");
                }
            }
            conn::reconnect(&handle);
            // Test hook: drive the Cmd+T path without UI scripting, then
            // heartbeat the main thread — used by the macOS tab-deadlock
            // repro and (later) the Linux smoke multi-tab exercise. Inert
            // unless HERMES_TEST_TAB_AFTER=<seconds> is set.
            if let Ok(v) = std::env::var("HERMES_TEST_TAB_AFTER") {
                if let Ok(secs) = v.parse::<u64>() {
                    let h = handle.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_secs(secs));
                        let h2 = h.clone();
                        let _ = h.run_on_main_thread(move || {
                            conn::open_new_session(&h2, true);
                            log::info!("test: tab hook dispatched on main");
                        });
                        for i in 1..=8u32 {
                            std::thread::sleep(std::time::Duration::from_secs(2));
                            let h3 = h.clone();
                            let _ = h.run_on_main_thread(move || {
                                log::info!(
                                    "test: heartbeat {i} windows={}",
                                    windows::content_window_handles(&h3).len()
                                );
                            });
                        }
                    });
                }
            }
            // Test hook: drive the Show-All-Tabs toggle (#42) from the main
            // thread, then heartbeat — guards the invariant-#12 GCD path for the
            // `toggleTabBar:` NSWindow mutation (a frozen main thread = missing
            // heartbeats). Inert unless HERMES_TEST_TOGGLE_TABBAR=<seconds>.
            #[cfg(target_os = "macos")]
            if let Ok(v) = std::env::var("HERMES_TEST_TOGGLE_TABBAR") {
                if let Ok(secs) = v.parse::<u64>() {
                    let h = handle.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_secs(secs));
                        let h2 = h.clone();
                        let _ = h.run_on_main_thread(move || {
                            if let Some(w) = windows::focused_or_recent_content(&h2) {
                                macos::toggle_tab_bar(&w);
                                log::info!("test: toggle-tabbar hook dispatched on main");
                            }
                        });
                        for i in 1..=8u32 {
                            std::thread::sleep(std::time::Duration::from_secs(2));
                            let h3 = h.clone();
                            let _ = h.run_on_main_thread(move || {
                                log::info!(
                                    "test: heartbeat {i} windows={}",
                                    windows::content_window_handles(&h3).len()
                                );
                            });
                        }
                    });
                }
            }
            // Passive update check shortly after launch (Sparkle parity) —
            // never blocks startup; only surfaces a dialog when an update
            // actually exists.
            {
                let update_handle = handle.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(10));
                    updater::spawn_check(&update_handle, false);
                });
            }
            // Session autosave (issue #18) + per-tab profile-dot refresh (#26):
            // one 4s timer. `persist` captures windows/tabs so a frame move or
            // SPA navigation (no structural event) is reflected; the profile
            // sweep re-reads each strip tab's cookie so a profile switched
            // *inside* a tab (an SPA re-render) repaints its dot. Both are cheap
            // and only act on a real change.
            {
                let sess_handle = handle.clone();
                std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_secs(4));
                    // Skip this tick's webview work while a native menu modal
                    // loop is up (the strip's "⋯" popup). Both `persist` (reads
                    // each tab's URL) and `recapture_profiles` (reads each tab's
                    // cookie) marshal into the webviews via the runtime
                    // dispatcher; on Windows that would re-enter the main thread
                    // from inside the popup's `TrackPopupMenu` modal loop and
                    // deadlock the UI (#33). The next tick (once the menu closes)
                    // catches up — both are idempotent and only act on a change.
                    if sess_handle
                        .state::<AppState>()
                        .menu_open
                        .load(std::sync::atomic::Ordering::SeqCst)
                    {
                        continue;
                    }
                    session::persist(&sess_handle);
                    if strip::enabled() {
                        strip::recapture_profiles(&sess_handle);
                    }
                    // Busy re-poll (issue #74): hidden webviews throttle their
                    // timers, so BUSY_REPORTER's 700ms interval can go quiet in
                    // a background tab and leave the spinner stale after the
                    // session finishes. An eval executes regardless of timer
                    // throttling and re-reads the live S.busy; the reporter's
                    // debounce means this emits only on an actual change.
                    windows::eval_all_content(
                        &sess_handle,
                        "window.__hermesReportBusy && window.__hermesReportBusy();",
                    );
                });
            }
            // Chrome poller — the Tauri stand-in for the Swift app's
            // tabbedWindows KVO: keeps webview layout + tabbed class +
            // traffic-light var in sync with native tab-bar/fullscreen
            // changes we can't observe directly (tab drag-out, merge,
            // Show Tab Bar menu).
            #[cfg(target_os = "macos")]
            {
                let poll_handle = handle.clone();
                std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_millis(1000));
                    windows::refresh_macos_chrome(&poll_handle);
                });
            }
            Ok(())
        })
        .on_menu_event(|app, event| match event.id().as_ref() {
            "preferences" => windows::open_prefs(app),
            "whats_new" => windows::open_whats_new(app),
            "toggle_strip" => {
                // Hide/show the tab strip of the focused window (issue #10).
                // Off-thread: it resizes the content webview.
                let app = app.clone();
                std::thread::spawn(move || {
                    if let Some(w) = windows::focused_or_recent_window_handle(&app) {
                        strip::toggle_strip(&app, w.label());
                    }
                });
            }
            "new_window" => conn::open_new_session(app, false),
            "new_tab" => conn::open_new_session(app, true),
            "paste" => paste::paste_into_focused(app),
            "reload" => windows::active_content_eval(app, "location.reload();"),
            // Reload EVERY tab in every window (issue #76): one action to
            // clear the per-tab "must hard refresh" banners after a WebUI
            // update, instead of clicking through each tab.
            "reload_all" => windows::eval_all_content(app, "location.reload();"),
            // Click-to-copy the version from the ⋮/app menu (issue #75) —
            // exact paste-ready string for bug reports, confirmed by a
            // notification (it's a direct user action, so always confirm).
            "copy_version" => {
                let v = format!(
                    "Hermes WebUI Desktop v{} ({})",
                    app.package_info().version,
                    std::env::consts::OS
                );
                let copied = arboard::Clipboard::new()
                    .and_then(|mut c| c.set_text(v.clone()))
                    .is_ok();
                if copied {
                    use tauri_plugin_notification::NotificationExt;
                    let _ = app.notification().builder().title("Copied").body(v).show();
                }
            }
            "quit" => app.exit(0),
            "check_updates" => updater::spawn_check(app, true),
            "reveal_logs" => {
                // The #1 support tool — testers needed the log path twice on
                // day one. Reveals the live log in Finder/Explorer/Files.
                if let Ok(dir) = app.path().app_log_dir() {
                    let file = dir.join("hermes-webui-desktop.log");
                    let target = if file.exists() { file } else { dir };
                    if tauri_plugin_opener::reveal_item_in_dir(&target).is_err() {
                        let _ = tauri_plugin_opener::open_path(
                            target.to_string_lossy().to_string(),
                            None::<&str>,
                        );
                    }
                }
            }
            "zoom_in" => menu::zoom_step(app, 0.1),
            "zoom_out" => menu::zoom_step(app, -0.1),
            "zoom_reset" => menu::zoom_reset(app),
            "find" => windows::active_content_eval(
                app,
                "window.__hermesFindToggle && window.__hermesFindToggle();",
            ),
            "find_next" => windows::active_content_eval(
                app,
                "window.__hermesFindNext && window.__hermesFindNext(true);",
            ),
            "find_prev" => windows::active_content_eval(
                app,
                "window.__hermesFindNext && window.__hermesFindNext(false);",
            ),
            "open_browser" => {
                let url = prefs::load(app).target_url;
                let _ = tauri_plugin_opener::open_url(url, None::<&str>);
            }
            "show_main" => windows::show_most_recent(app),
            // macOS: summon the native tab bar with one window open (#42). The
            // helper queues onto the GCD main queue (invariant #12) and returns
            // at once, so this menu callout never touches AppKit inline.
            #[cfg(target_os = "macos")]
            "show_tab_bar" => {
                if let Some(w) = windows::focused_or_recent_content(app) {
                    macos::toggle_tab_bar(&w);
                }
            }
            _ => {}
        })
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                // Swift behavior: the LAST browser window hides on Cmd+W and
                // the app stays in the Dock (macOS). Win/Linux: closing the
                // last window quits (D11).
                let label = window.label().to_string();
                if label.starts_with("main-") {
                    let app = window.app_handle();
                    if windows::content_windows(app).len() <= 1 {
                        #[cfg(target_os = "macos")]
                        {
                            api.prevent_close();
                            let _ = window.hide();
                        }
                        #[cfg(not(target_os = "macos"))]
                        let _ = api;
                    }
                }
            }
            tauri::WindowEvent::Destroyed => {
                let app = window.app_handle();
                windows::forget(app, window.label());
                strip::forget_window(app, window.label());
                windows::refresh_macos_chrome(app);
                // Win/Linux (D11): quit when the USER closed the last
                // meaningful window — never during the orchestrator's
                // rebuild gaps (it sets `connecting` while windows churn).
                windows::maybe_quit_after_close(app);
            }
            tauri::WindowEvent::Resized(_) => {
                let app = window.app_handle();
                windows::persist_first_frame(app, window);
                // Resize can change contentLayoutRect (fullscreen toggles,
                // tab bar transitions) — recompute (Swift windowDidResize).
                windows::refresh_macos_chrome(app);
                // Strip mode: re-fit the strip + active tab webview bounds.
                if strip::enabled() && window.label().starts_with("main-") {
                    strip::layout(app, window.label());
                }
            }
            tauri::WindowEvent::Moved(_) => {
                let app = window.app_handle();
                windows::persist_first_frame(app, window);
            }
            tauri::WindowEvent::Focused(focused) => {
                // Tell the page when its tab is backgrounded so the WebUI fires
                // OS notifications for an unfocused-but-streaming tab (#32). On
                // macOS each native tab is its own window, so window focus IS the
                // tab-switch signal; on the Win/Linux strip this catches the
                // whole app losing/gaining focus (per-tab switching is handled in
                // strip::select_tab). The flag feeds only the notification gate,
                // not SSE-close, so streams keep running (hermes-webui #4753).
                let app = window.app_handle();
                let label = window.label();
                if label.starts_with("main-") {
                    let bg = !*focused;
                    if strip::enabled() {
                        strip::set_active_tab_backgrounded(app, label, bg);
                    } else if let Some(wv) = app.get_webview_window(label) {
                        let _ = wv.eval(format!(
                            "window.__hermesSetBackgrounded&&window.__hermesSetBackgrounded({bg})"
                        ));
                    }
                }
            }
            _ => {}
        })
        .build(tauri::generate_context!())
        .expect("error while building Hermes WebUI Desktop")
        .run(|app, event| match event {
            tauri::RunEvent::ExitRequested { code, api, .. } => {
                // ALL platforms: a code-less exit request means "the last
                // window was destroyed" — which also happens transiently
                // inside the connection flow (splash destroyed before the
                // error/main window exists). Letting it through killed the
                // app right after the splash on Windows (v0.1.0 bug).
                // Explicit quits call app.exit(code) and pass through;
                // Win/Linux close-last-window-quits is implemented
                // deliberately in windows::maybe_quit_after_close.
                if code.is_none() {
                    api.prevent_exit();
                }
            }
            tauri::RunEvent::Exit => {
                tunnel::stop(app);
            }
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => {
                windows::show_most_recent(app);
            }
            _ => {}
        });
}

#[cfg(test)]
mod tests {
    use super::changelog_section;

    #[test]
    fn extracts_current_version_section() {
        let md = "# Changelog\n\n## [v0.5.0] — 2026-06-17\n\n### Fixed\n\n- A real fix\n\n## [v0.4.1] — 2026-06-17\n\n- Older stuff\n";
        let s = changelog_section(md, "0.5.0").expect("section");
        assert!(s.contains("A real fix"));
        assert!(!s.contains("Older stuff"), "must stop at the next heading");
        assert!(changelog_section(md, "9.9.9").is_none());
    }
}
