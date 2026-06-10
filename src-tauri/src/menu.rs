//! Menu bar (macOS) — mirrors the Swift app's menus (docs/03 § Menus).

use crate::{prefs, windows};
use tauri::menu::{Menu, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder};
use tauri::AppHandle;

pub fn build(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let app_menu = SubmenuBuilder::new(app, "Hermes WebUI Desktop")
        .about(None)
        .separator()
        .item(
            &MenuItemBuilder::with_id("preferences", "Preferences…")
                .accelerator("CmdOrCtrl+,")
                .build(app)?,
        )
        .separator()
        .quit()
        .build()?;

    let file_menu = SubmenuBuilder::new(app, "File")
        .item(
            &MenuItemBuilder::with_id("new_window", "New Window")
                .accelerator("CmdOrCtrl+N")
                .build(app)?,
        )
        .item(
            &MenuItemBuilder::with_id("new_tab", "New Tab")
                .accelerator("CmdOrCtrl+T")
                .build(app)?,
        )
        .separator()
        .item(&PredefinedMenuItem::close_window(
            app,
            Some("Close Window"),
        )?)
        .build()?;

    let edit_menu = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .item(
            // Custom Paste: routes through the native paste pipeline
            // (image → 3-strategy injection), like the Swift Cmd+V intercept.
            &MenuItemBuilder::with_id("paste", "Paste")
                .accelerator("CmdOrCtrl+V")
                .build(app)?,
        )
        .select_all()
        .separator()
        .item(
            &MenuItemBuilder::with_id("find", "Find…")
                .accelerator("CmdOrCtrl+F")
                .build(app)?,
        )
        .item(
            &MenuItemBuilder::with_id("find_next", "Find Next")
                .accelerator("CmdOrCtrl+G")
                .build(app)?,
        )
        .item(
            &MenuItemBuilder::with_id("find_prev", "Find Previous")
                .accelerator("CmdOrCtrl+Shift+G")
                .build(app)?,
        )
        .build()?;

    let view_menu = SubmenuBuilder::new(app, "View")
        .item(
            &MenuItemBuilder::with_id("reload", "Reload")
                .accelerator("CmdOrCtrl+R")
                .build(app)?,
        )
        .separator()
        .item(
            &MenuItemBuilder::with_id("zoom_in", "Zoom In")
                .accelerator("CmdOrCtrl+=")
                .build(app)?,
        )
        .item(
            &MenuItemBuilder::with_id("zoom_out", "Zoom Out")
                .accelerator("CmdOrCtrl+-")
                .build(app)?,
        )
        .item(
            &MenuItemBuilder::with_id("zoom_reset", "Actual Size")
                .accelerator("CmdOrCtrl+0")
                .build(app)?,
        )
        .separator()
        .item(&MenuItemBuilder::with_id("open_browser", "Open in Browser").build(app)?)
        .build()?;

    let window_menu = SubmenuBuilder::new(app, "Window")
        .item(
            &MenuItemBuilder::with_id("show_main", "Show Hermes")
                .accelerator("CmdOrCtrl+Shift+H")
                .build(app)?,
        )
        .separator()
        .minimize()
        .build()?;

    Menu::with_items(
        app,
        &[&app_menu, &file_menu, &edit_menu, &view_menu, &window_menu],
    )
}

/// Zoom step — clamped 0.5–3.0 in 0.1 increments, persisted (Swift fix #24/#43).
pub fn zoom_step(app: &AppHandle, delta: f64) {
    let next = (prefs::zoom_get(app) + delta).clamp(0.5, 3.0);
    prefs::zoom_set(app, next);
    if let Some(w) = windows::focused_or_recent_content(app) {
        let _ = w.set_zoom(next);
    }
}

pub fn zoom_reset(app: &AppHandle) {
    prefs::zoom_set(app, 1.0);
    if let Some(w) = windows::focused_or_recent_content(app) {
        let _ = w.set_zoom(1.0);
    }
}
