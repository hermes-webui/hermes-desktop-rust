// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bridge;
mod conn;
mod health;
mod macos;
mod menu;
mod paste;
mod prefs;
mod state;
mod theme;
mod tunnel;
mod windows;

use state::AppState;
use tauri::Manager;

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
fn close_prefs(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("prefs") {
        let _ = w.destroy();
    }
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
            close_prefs,
            get_boot_info
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            // App-wide appearance from the theme cache before any window
            // opens (Swift: loadCachedTheme at applicationDidFinishLaunching;
            // defaults dark). Without this, the chrome (menus, native tab
            // bar) follows the OS appearance instead of the page theme.
            handle.set_theme(Some(windows::cached_theme(&handle)));
            #[cfg(target_os = "macos")]
            {
                let m = menu::build(&handle)?;
                app.set_menu(m)?;
            }
            bridge::install(&handle);
            {
                use tauri_plugin_global_shortcut::GlobalShortcutExt;
                // Conflict with the Swift app's identical default is expected
                // when both run — log and continue (docs/11 § Coexistence).
                if let Err(e) = handle.global_shortcut().register("CmdOrCtrl+Shift+H") {
                    log::warn!("global shortcut unavailable: {e}");
                }
            }
            conn::reconnect(&handle);
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
            "new_window" => conn::open_new_session(app, false),
            "new_tab" => conn::open_new_session(app, true),
            "paste" => paste::paste_into_focused(app),
            "reload" => {
                if let Some(w) = windows::focused_or_recent_content(app) {
                    let _ = w.eval("location.reload();");
                }
            }
            "zoom_in" => menu::zoom_step(app, 0.1),
            "zoom_out" => menu::zoom_step(app, -0.1),
            "zoom_reset" => menu::zoom_reset(app),
            "find" => {
                if let Some(w) = windows::focused_or_recent_content(app) {
                    let _ = w.eval("window.__hermesFindToggle && window.__hermesFindToggle();");
                }
            }
            "find_next" => {
                if let Some(w) = windows::focused_or_recent_content(app) {
                    let _ = w.eval("window.__hermesFindNext && window.__hermesFindNext(true);");
                }
            }
            "find_prev" => {
                if let Some(w) = windows::focused_or_recent_content(app) {
                    let _ = w.eval("window.__hermesFindNext && window.__hermesFindNext(false);");
                }
            }
            "open_browser" => {
                let url = prefs::load(app).target_url;
                let _ = tauri_plugin_opener::open_url(url, None::<&str>);
            }
            "show_main" => windows::show_most_recent(app),
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
            }
            tauri::WindowEvent::Moved(_) => {
                let app = window.app_handle();
                windows::persist_first_frame(app, window);
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
