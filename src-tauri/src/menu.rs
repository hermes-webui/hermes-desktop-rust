//! Menu bar (macOS) — mirrors the Swift app's menus (docs/03 § Menus).

use crate::{prefs, windows};
use tauri::menu::{Menu, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder};
use tauri::AppHandle;

pub fn build(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let app_menu = SubmenuBuilder::new(app, "Hermes WebUI Desktop")
        .about(None)
        .separator()
        .item(&MenuItemBuilder::with_id("check_updates", "Check for Updates…").build(app)?)
        .item(&MenuItemBuilder::with_id("whats_new", "What's New").build(app)?)
        .item(&MenuItemBuilder::with_id("reveal_logs", "Reveal Log File").build(app)?)
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
    windows::active_content_zoom(app, next);
}

pub fn zoom_reset(app: &AppHandle) {
    prefs::zoom_set(app, 1.0);
    windows::active_content_zoom(app, 1.0);
}

/// The strip's "⋯" popup — a NATIVE context menu (Windows/Linux), doubling as
/// the discoverability surface: every action lists its keyboard shortcut.
/// Shortcut hints ride in the item text ("\t" column) rather than real
/// accelerators so they never double-fire with the injected key forwarder.
pub fn build_strip_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    use tauri::menu::IsMenuItem;
    let version = app.package_info().version.to_string();
    let item = |id: &str, text: &str| MenuItemBuilder::with_id(id, text).build(app);

    let mut items: Vec<Box<dyn IsMenuItem<tauri::Wry>>> = Vec::new();
    items.push(Box::new(item("new_tab", "New Tab\tCtrl+T")?));
    items.push(Box::new(item("new_window", "New Window\tCtrl+N")?));
    // Hide the tab bar (issue #10) — not on Linux (GTK child-webview geometry,
    // constraint #1). The shortcut is in the label since hiding removes this
    // button, so Ctrl+Shift+B is how you bring the bar back.
    if !cfg!(target_os = "linux") {
        items.push(Box::new(item(
            "toggle_strip",
            "Hide Tab Bar\tCtrl+Shift+B",
        )?));
    }
    items.push(Box::new(PredefinedMenuItem::separator(app)?));
    items.push(Box::new(item("reload", "Reload\tCtrl+R")?));
    items.push(Box::new(item("find", "Find in Page…\tCtrl+F")?));
    items.push(Box::new(PredefinedMenuItem::separator(app)?));
    items.push(Box::new(item("zoom_in", "Zoom In\tCtrl+=")?));
    items.push(Box::new(item("zoom_out", "Zoom Out\tCtrl+-")?));
    items.push(Box::new(item("zoom_reset", "Actual Size\tCtrl+0")?));
    items.push(Box::new(PredefinedMenuItem::separator(app)?));
    items.push(Box::new(item("preferences", "Preferences…\tCtrl+,")?));
    items.push(Box::new(item("open_browser", "Open in Browser")?));
    items.push(Box::new(PredefinedMenuItem::separator(app)?));
    items.push(Box::new(
        MenuItemBuilder::with_id("about_version", format!("Hermes WebUI Desktop v{version}"))
            .enabled(false)
            .build(app)?,
    ));
    items.push(Box::new(item("whats_new", "What's New")?));
    items.push(Box::new(item("check_updates", "Check for Updates…")?));
    items.push(Box::new(item("reveal_logs", "Reveal Log File")?));
    items.push(Box::new(item("quit", "Quit")?));

    let refs: Vec<&dyn IsMenuItem<tauri::Wry>> = items.iter().map(|b| b.as_ref()).collect();
    Menu::with_items(app, &refs)
}
