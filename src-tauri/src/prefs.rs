//! Preferences over tauri-plugin-store. Key names and defaults mirror the
//! Swift app's UserDefaults 1:1 (see docs/03-swift-mac-reference.md § Settings).

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::AppHandle;
use tauri_plugin_store::StoreExt;

pub const STORE_FILE: &str = "prefs.json";
pub const DEFAULT_TARGET_URL: &str = "http://localhost:8787";
pub const DEFAULT_SSH_USER: &str = "hermes";
pub const DEFAULT_SSH_HOST: &str = "your-server.com";
pub const DEFAULT_PORT: &str = "8787";
const THEME_CACHE_STALENESS_SECS: f64 = 7.0 * 24.0 * 3600.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prefs {
    #[serde(rename = "connectionMode")]
    pub connection_mode: String,
    #[serde(rename = "targetURL")]
    pub target_url: String,
    #[serde(rename = "sshUser")]
    pub ssh_user: String,
    #[serde(rename = "sshHost")]
    pub ssh_host: String,
    #[serde(rename = "localPort")]
    pub local_port: String,
    #[serde(rename = "remotePort")]
    pub remote_port: String,
    #[serde(rename = "notificationsEnabled")]
    pub notifications_enabled: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            connection_mode: "direct".into(),
            target_url: DEFAULT_TARGET_URL.into(),
            ssh_user: DEFAULT_SSH_USER.into(),
            ssh_host: DEFAULT_SSH_HOST.into(),
            local_port: DEFAULT_PORT.into(),
            remote_port: DEFAULT_PORT.into(),
            notifications_enabled: true,
        }
    }
}

fn get_str(app: &AppHandle, key: &str, default: &str) -> String {
    app.store(STORE_FILE)
        .ok()
        .and_then(|s| s.get(key))
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| default.to_string())
}

fn get_bool(app: &AppHandle, key: &str, default: bool) -> bool {
    app.store(STORE_FILE)
        .ok()
        .and_then(|s| s.get(key))
        .and_then(|v| v.as_bool())
        .unwrap_or(default)
}

/// Write the full default set on first-ever launch (Swift seedDefaultsIfNeeded
/// parity) — so prefs.json shows the real schema instead of "{}".
pub fn seed_if_needed(app: &AppHandle) {
    let seeded = app
        .store(STORE_FILE)
        .ok()
        .map(|s| s.get("connectionMode").is_some())
        .unwrap_or(false);
    if !seeded {
        save(app, &Prefs::default());
    }
}

pub fn load(app: &AppHandle) -> Prefs {
    let d = Prefs::default();
    Prefs {
        connection_mode: get_str(app, "connectionMode", &d.connection_mode),
        target_url: get_str(app, "targetURL", &d.target_url),
        ssh_user: get_str(app, "sshUser", &d.ssh_user),
        ssh_host: get_str(app, "sshHost", &d.ssh_host),
        local_port: get_str(app, "localPort", &d.local_port),
        remote_port: get_str(app, "remotePort", &d.remote_port),
        notifications_enabled: get_bool(app, "notificationsEnabled", true),
    }
}

pub fn save(app: &AppHandle, p: &Prefs) {
    if let Ok(store) = app.store(STORE_FILE) {
        store.set("connectionMode", json!(p.connection_mode));
        store.set("targetURL", json!(p.target_url));
        store.set("sshUser", json!(p.ssh_user));
        store.set("sshHost", json!(p.ssh_host));
        store.set("localPort", json!(p.local_port));
        store.set("remotePort", json!(p.remote_port));
        store.set("notificationsEnabled", json!(p.notifications_enabled));
        let _ = store.save();
    }
}

/// Validation — same rules and messages as the Swift preferences window.
pub fn validate(p: &Prefs) -> Result<(), String> {
    if p.target_url.trim().is_empty() {
        return Err("Please fill in the Target URL.".into());
    }
    let parsed = url::Url::parse(p.target_url.trim())
        .map_err(|_| "Target URL must be a valid http:// or https:// URL.".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("Target URL must be a valid http:// or https:// URL.".into());
    }
    if p.connection_mode == "ssh" {
        if p.ssh_user.trim().is_empty()
            || p.ssh_host.trim().is_empty()
            || p.local_port.trim().is_empty()
            || p.remote_port.trim().is_empty()
        {
            return Err("Please fill in all SSH settings.".into());
        }
        let lp: u32 = p
            .local_port
            .trim()
            .parse()
            .map_err(|_| "Local port must be a number between 1 and 65535.".to_string())?;
        if !(1..=65535).contains(&lp) {
            return Err("Local port must be a number between 1 and 65535.".into());
        }
        let rp: u32 = p
            .remote_port
            .trim()
            .parse()
            .map_err(|_| "Remote port must be a number between 1 and 65535.".to_string())?;
        if !(1..=65535).contains(&rp) {
            return Err("Remote port must be a number between 1 and 65535.".into());
        }
    }
    Ok(())
}

// ---- Zoom (webViewMagnification, clamped 0.5–3.0) ----

pub fn zoom_get(app: &AppHandle) -> f64 {
    let v = app
        .store(STORE_FILE)
        .ok()
        .and_then(|s| s.get("webViewMagnification"))
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0);
    if (0.5..=3.0).contains(&v) {
        v
    } else {
        1.0
    }
}

pub fn zoom_set(app: &AppHandle, v: f64) {
    if let Ok(store) = app.store(STORE_FILE) {
        store.set("webViewMagnification", json!(v));
        let _ = store.save();
    }
}

// ---- Theme cache (7-day staleness, components sanity-checked into [0,1]) ----

pub fn theme_cache_load(app: &AppHandle) -> Option<(f64, f64, f64)> {
    let store = app.store(STORE_FILE).ok()?;
    let ts = store.get("themeCacheTimestamp")?.as_f64()?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs_f64();
    let age = now - ts;
    if !(0.0..THEME_CACHE_STALENESS_SECS).contains(&age) {
        return None;
    }
    let r = store.get("themeCacheRed")?.as_f64()?;
    let g = store.get("themeCacheGreen")?.as_f64()?;
    let b = store.get("themeCacheBlue")?.as_f64()?;
    if [r, g, b].iter().all(|c| (0.0..=1.0).contains(c)) {
        Some((r, g, b))
    } else {
        None
    }
}

pub fn theme_cache_save(app: &AppHandle, r: f64, g: f64, b: f64) {
    if let Ok(store) = app.store(STORE_FILE) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        store.set("themeCacheRed", json!(r));
        store.set("themeCacheGreen", json!(g));
        store.set("themeCacheBlue", json!(b));
        store.set("themeCacheTimestamp", json!(now));
        let _ = store.save();
    }
}

/// The pre-paint color: cached page background if fresh, else the safe dark
/// default #1a1a1a (matches the Swift app's hardcoded fallback).
pub fn pre_paint_color(app: &AppHandle) -> (f64, f64, f64) {
    theme_cache_load(app).unwrap_or((0.10, 0.10, 0.10))
}

// ---- First-window frame persistence (≈ "NSWindow Frame HermesMainWindow") ----

pub fn frame_save(app: &AppHandle, x: i32, y: i32, w: u32, h: u32) {
    if let Ok(store) = app.store(STORE_FILE) {
        store.set("windowFrame", json!({ "x": x, "y": y, "w": w, "h": h }));
        let _ = store.save();
    }
}

pub fn frame_load(app: &AppHandle) -> Option<(i32, i32, u32, u32)> {
    let store = app.store(STORE_FILE).ok()?;
    let v = store.get("windowFrame")?;
    let w = v["w"].as_u64()? as u32;
    let h = v["h"].as_u64()? as u32;
    if w < 200 || h < 200 {
        return None;
    }
    Some((v["x"].as_i64()? as i32, v["y"].as_i64()? as i32, w, h))
}

pub fn fullscreen_get(app: &AppHandle) -> bool {
    app.store(STORE_FILE)
        .ok()
        .and_then(|s| s.get("windowWasFullScreen"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

pub fn fullscreen_set(app: &AppHandle, v: bool) {
    if let Ok(store) = app.store(STORE_FILE) {
        store.set("windowWasFullScreen", json!(v));
        let _ = store.save();
    }
}

// ---- One-time "tab bar hidden" hint (issue #10 discoverability) ----

/// Whether the one-time "Tab bar hidden — Ctrl+Shift+B to show it" hint has
/// already been shown. Hiding the strip removes the ⋯ button (the only visible
/// affordance), so a first-time hider needs to learn the un-hide shortcut.
pub fn hide_hint_shown(app: &AppHandle) -> bool {
    get_bool(app, "tabBarHideHintShown", false)
}

pub fn set_hide_hint_shown(app: &AppHandle) {
    if let Ok(store) = app.store(STORE_FILE) {
        store.set("tabBarHideHintShown", json!(true));
        let _ = store.save();
    }
}

// ---- One-time "tabs exist" discoverability hint (issue #42, macOS) ----

/// Whether the one-time "this app has tabs — ⌘T" hint has been retired. On
/// macOS tabs are a hidden feature (no on-screen affordance; the native tab bar
/// only appears with 2+ tabs), so a first-time user may never discover them.
/// Retired once the user actually opens a tab (proof they found the feature) —
/// NOT when the hint is shown, mirroring the issue-#10 lesson (a dropped
/// notification must not permanently spend the hint).
pub fn tabs_hint_shown(app: &AppHandle) -> bool {
    get_bool(app, "tabsHintShown", false)
}

pub fn set_tabs_hint_shown(app: &AppHandle) {
    if let Ok(store) = app.store(STORE_FILE) {
        store.set("tabsHintShown", json!(true));
        let _ = store.save();
    }
}

// ---- Per-profile dot color overrides (issue #47) ----

/// `true` for a `#rrggbb` hex string. Gates what we persist + later inject into
/// the strip's `style.background`, so a non-color value can't poison the CSS.
fn is_hex_color(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 7 && b[0] == b'#' && b[1..].iter().all(u8::is_ascii_hexdigit)
}

/// User-chosen dot-color overrides as a JSON object `{ "<profile>": "#rrggbb" }`
/// (issue #47). The strip's `profileColor` consults this before the auto
/// palette. `{}` when unset. Sent to the strip in the tabs snapshot.
pub fn profile_colors(app: &AppHandle) -> serde_json::Value {
    let stored = app
        .store(STORE_FILE)
        .ok()
        .and_then(|s| s.get("profileColors"));
    let Some(serde_json::Value::Object(obj)) = stored else {
        return json!({});
    };
    // Re-validate on read (defense-in-depth): a hand-edited prefs.json could
    // hold a non-`#rrggbb` value that bypassed `set_profile_color`, and the
    // strip feeds this straight into `style.background` (issue #47). Drop any
    // entry that isn't a valid hex color so only safe values ever reach the DOM.
    let clean: serde_json::Map<String, serde_json::Value> = obj
        .into_iter()
        .filter(|(_, v)| v.as_str().map(is_hex_color).unwrap_or(false))
        .collect();
    serde_json::Value::Object(clean)
}

/// Set (or clear) one profile's dot color. A valid `#rrggbb` is stored
/// (lowercased); any other value clears that profile's override (revert to the
/// auto palette).
pub fn set_profile_color(app: &AppHandle, name: &str, color: &str) {
    if name.is_empty() {
        return;
    }
    if let Ok(store) = app.store(STORE_FILE) {
        let mut map = store
            .get("profileColors")
            .filter(|v| v.is_object())
            .unwrap_or_else(|| json!({}));
        if let Some(obj) = map.as_object_mut() {
            if is_hex_color(color) {
                obj.insert(name.to_string(), json!(color.to_lowercase()));
            } else {
                obj.remove(name);
            }
        }
        store.set("profileColors", map);
        let _ = store.save();
    }
}

#[cfg(test)]
mod tests {
    use super::is_hex_color;

    #[test]
    fn hex_color_validation_is_strict() {
        // The only accepted shape is #rrggbb — this gates what gets injected
        // into the strip's CSS `style.background` (issue #47).
        assert!(is_hex_color("#4f9dff"));
        assert!(is_hex_color("#000000"));
        assert!(is_hex_color("#FFFFFF"));
        // Rejected: missing #, wrong length, non-hex, and CSS-injection shapes.
        assert!(!is_hex_color("4f9dff"));
        assert!(!is_hex_color("#4f9df"));
        assert!(!is_hex_color("#4f9dfff"));
        assert!(!is_hex_color("#12345g"));
        assert!(!is_hex_color(""));
        assert!(!is_hex_color("#fff"));
        assert!(!is_hex_color("red"));
        assert!(!is_hex_color("#000;}body{x"));
    }
}
