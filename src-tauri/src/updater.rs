//! Auto-update — the Sparkle-parity feature (roadmap "Auto-update" sprint).
//! Passive check shortly after launch + "Check for Updates…" in the macOS app
//! menu and the strip's ⋯ menu. Updates are minisign-verified against the
//! pubkey in tauri.conf.json; the manifest is GitHub Releases' latest.json.
//!
//! Coverage: Windows NSIS/MSI (installer runs, app exits), macOS (.app
//! replaced in place), Linux AppImage (binary swapped in place). `.deb`
//! installs aren't updatable by the protocol — interactive checks tell those
//! users to grab the new package from Releases.

use tauri::AppHandle;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_updater::UpdaterExt;

/// Spawn a check on a worker thread. `interactive` controls whether
/// "no update" / errors surface as dialogs (menu action) or just logs
/// (passive launch check).
pub fn spawn_check(app: &AppHandle, interactive: bool) {
    let app = app.clone();
    std::thread::spawn(move || run_check(app, interactive));
}

fn run_check(app: AppHandle, interactive: bool) {
    // Windows portable build (zip with a portable.txt marker next to the
    // exe): self-updating would silently convert it into an installed app —
    // degrade to a Releases pointer instead (tester request).
    #[cfg(windows)]
    {
        let portable = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|d| d.join("portable.txt").exists()))
            .unwrap_or(false);
        if portable {
            log::info!("updater: portable build — self-update disabled");
            if interactive {
                app.dialog()
                    .message(
                        "This is the portable build, which updates manually:\n\
                         download the new portable zip from the GitHub Releases page.",
                    )
                    .title("Updates")
                    .kind(MessageDialogKind::Info)
                    .blocking_show();
            }
            return;
        }
    }

    // Linux: only the AppImage build can self-update.
    #[cfg(target_os = "linux")]
    if std::env::var("APPIMAGE").is_err() {
        log::info!("updater: not an AppImage — self-update unavailable");
        if interactive {
            app.dialog()
                .message(
                    "Auto-update is available for the AppImage build.\n\n\
                     For .deb installs, download the new package from the GitHub Releases page.",
                )
                .title("Updates")
                .kind(MessageDialogKind::Info)
                .blocking_show();
        }
        return;
    }

    let updater = match app.updater() {
        Ok(u) => u,
        Err(e) => {
            log::warn!("updater: unavailable: {e}");
            if interactive {
                show_error(&app, &format!("The updater isn't available: {e}"));
            }
            return;
        }
    };

    let result = tauri::async_runtime::block_on(updater.check());
    match result {
        Ok(Some(update)) => {
            let current = app.package_info().version.to_string();
            let next = update.version.clone();
            log::info!("updater: update available {current} -> {next}");
            let install = app
                .dialog()
                .message(format!(
                    "Hermes WebUI Desktop {next} is available (you have {current}).\n\n\
                     Download and install it now?"
                ))
                .title("Update Available")
                .kind(MessageDialogKind::Info)
                .buttons(MessageDialogButtons::OkCancelCustom(
                    "Install and Relaunch".into(),
                    "Later".into(),
                ))
                .blocking_show();
            if !install {
                return;
            }
            let installed = tauri::async_runtime::block_on(update.download_and_install(
                |chunk, total| {
                    log::debug!("updater: downloaded {chunk} of {total:?}");
                },
                || log::info!("updater: download finished, installing"),
            ));
            match installed {
                Ok(()) => {
                    // Windows: the installer takes over and the app exits
                    // before reaching this point. macOS/Linux: relaunch.
                    log::info!("updater: installed {next} — relaunching");
                    app.restart();
                }
                Err(e) => {
                    log::error!("updater: install failed: {e}");
                    show_error(&app, &format!("The update couldn't be installed: {e}"));
                }
            }
        }
        Ok(None) => {
            log::info!("updater: up to date");
            if interactive {
                let v = app.package_info().version.to_string();
                app.dialog()
                    .message(format!("You're on the latest version (v{v})."))
                    .title("Up to Date")
                    .kind(MessageDialogKind::Info)
                    .blocking_show();
            }
        }
        Err(e) => {
            log::warn!("updater: check failed: {e}");
            if interactive {
                show_error(
                    &app,
                    "Couldn't reach the update service.\n\n\
                     Check your internet connection, or download the latest version \
                     from the GitHub Releases page.\n\n\
                     (Details are in the log file.)",
                );
            }
        }
    }
}

fn show_error(app: &AppHandle, message: &str) {
    app.dialog()
        .message(message)
        .title("Update Check Failed")
        .kind(MessageDialogKind::Error)
        .blocking_show();
}
