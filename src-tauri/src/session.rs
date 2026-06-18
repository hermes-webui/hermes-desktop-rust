//! Session persistence — bring the user's windows + tabs back across restart
//! and in-app updates (issue #18). The server owns chat content; the shell
//! persists just enough to recreate the *shape* of the workspace: per window,
//! the ordered set of tabs (each tab's current URL + active WebUI profile),
//! which tab was active, and the window frame. On the first successful
//! connection of a fresh launch the saved session is replayed instead of
//! opening one bare tab.
//!
//! Two tab models feed this (see [`crate::strip`] / [`crate::windows`]):
//!   * Windows/Linux strip — the `AppState.strip` registry is the source of
//!     truth (tab order/active/profile live there already).
//!   * macOS native tabs — NSWindow tab groups are queried via objc2
//!     ([`crate::macos::session_windows`]) so user-driven drag/merge/reorder is
//!     captured faithfully (we get no KVO for those).
//!
//! Restoration re-seeds each tab's `hermes_profile` cookie so it reopens on the
//! same profile (the per-tab profile isolation of v0.3.7/v0.3.8). Only that
//! non-sensitive profile *selector* is persisted — never auth/login/session
//! cookies — so an authenticated server simply re-prompts for login after a
//! restart. The cookie is reconstructed host-only + HttpOnly + Path=/ to match
//! how the WebUI sets it.

use crate::state::AppState;
use crate::{prefs, strip};
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;
use tauri::{AppHandle, Manager};

const KEY: &str = "session";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionTab {
    pub url: String,
    #[serde(default)]
    pub profile: Option<String>,
    /// The tab's on-disk partition dir (Windows/Linux) — reused on restore so
    /// login/cookies survive (issue #28). Absent for macOS / pre-0.5.0 blobs.
    #[serde(default)]
    pub partition: Option<String>,
    /// User-given tab name (issue #7), restored verbatim.
    #[serde(default)]
    pub custom_title: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionWindow {
    /// [x, y, w, h] in physical pixels, or None to center/cascade.
    #[serde(default)]
    pub frame: Option<[i64; 4]>,
    #[serde(default)]
    pub active: usize,
    pub tabs: Vec<SessionTab>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Session {
    pub mode: String,
    pub target: String,
    pub windows: Vec<SessionWindow>,
}

/// Reconstruct the WebUI's `hermes_profile` cookie for re-seeding a restored
/// tab. Host-only (no Domain), HttpOnly, Path=/ — matching the server's
/// `Set-Cookie` so the WebUI reads it back as the same selector.
pub fn profile_cookie(value: &str) -> tauri::webview::Cookie<'static> {
    let mut c = tauri::webview::Cookie::new("hermes_profile".to_string(), value.to_string());
    c.set_path("/");
    c.set_http_only(true);
    c
}

// ---- store I/O ----

pub fn save(app: &AppHandle, s: &Session) {
    use tauri_plugin_store::StoreExt;
    if let Ok(store) = app.store(prefs::STORE_FILE) {
        if let Ok(v) = serde_json::to_value(s) {
            store.set(KEY, v);
            let _ = store.save();
        }
    }
}

pub fn load(app: &AppHandle) -> Option<Session> {
    use tauri_plugin_store::StoreExt;
    let store = app.store(prefs::STORE_FILE).ok()?;
    let v = store.get(KEY)?;
    serde_json::from_value(v).ok()
}

// ---- capture ----

/// Build a [`Session`] from the live window/tab state. Runs the actual read on
/// the main thread (AppKit tab-group queries on macOS, webview URL getters
/// everywhere must touch the runtime on its own thread) and blocks the caller
/// for the result — so call it only from a worker thread, never the main one.
fn capture(app: &AppHandle) -> Session {
    let (tx, rx) = mpsc::channel();
    let app2 = app.clone();
    if app
        .run_on_main_thread(move || {
            let _ = tx.send(capture_inner(&app2));
        })
        .is_err()
    {
        return Session::default();
    }
    rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default()
}

fn capture_inner(app: &AppHandle) -> Session {
    let p = prefs::load(app);
    let windows = if strip::enabled() {
        strip::session_windows(app)
    } else {
        #[cfg(target_os = "macos")]
        {
            crate::macos::session_windows(app)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Vec::new()
        }
    };
    Session {
        mode: p.connection_mode,
        target: p.target_url,
        windows,
    }
}

/// Capture the current session and persist it — but only if it changed since
/// the last write (cheap dedupe so the periodic tick + structural-change calls
/// don't thrash the store). Off-loads to a worker thread; overlapping calls are
/// dropped (the periodic tick recovers anything missed).
pub fn persist(app: &AppHandle) {
    let state = app.state::<AppState>();
    // Never write while restore is rebuilding windows — a half-built capture
    // would clobber the saved session. (A capture before the first connect is
    // harmless: it's empty and the empty-guard below drops it.)
    if state.restoring.load(Ordering::SeqCst) {
        return;
    }
    if state.persist_busy.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let s = capture(&app);
        let state = app.state::<AppState>();
        // Never overwrite a real saved session with an empty capture (e.g. a
        // transient zero-window moment during reconnect).
        if !s.windows.is_empty() {
            let json = serde_json::to_string(&s).unwrap_or_default();
            let mut last = state.last_session.lock().unwrap();
            if *last != json {
                *last = json;
                drop(last);
                save(&app, &s);
            }
        }
        state.persist_busy.store(false, Ordering::SeqCst);
    });
}

// ---- restore ----

/// On the FIRST successful connection of a launch, replay the saved session if
/// one exists and matches the current connection (same mode + target — a
/// different server's tabs/profiles wouldn't make sense). Returns true if it
/// restored windows; false if the caller should fall back to opening one bare
/// window. Marks the session as restored either way.
pub fn maybe_restore(app: &AppHandle) -> bool {
    let state = app.state::<AppState>();
    if state.session_restored.swap(true, Ordering::SeqCst) {
        return false; // already handled this launch
    }
    let p = prefs::load(app);
    let Some(saved) = load(app) else {
        return false;
    };
    // Lenient target match (issue #28 "tabs lost"): a trailing-slash difference
    // between runs (e.g. a Tailscale URL saved with vs without "/") must not
    // silently skip restore.
    let norm = |s: &str| s.trim_end_matches('/').to_string();
    if saved.windows.is_empty()
        || saved.mode != p.connection_mode
        || norm(&saved.target) != norm(&p.target_url)
    {
        return false;
    }
    // Seed last_session so the first persist() doesn't immediately rewrite an
    // identical blob.
    if let Ok(json) = serde_json::to_string(&saved) {
        *state.last_session.lock().unwrap() = json;
    }
    let total: usize = saved.windows.iter().map(|w| w.tabs.len()).sum();
    log::info!(
        "session: restoring {} window(s), {} tab(s)",
        saved.windows.len(),
        total
    );
    state.restoring.store(true, Ordering::SeqCst);
    if strip::enabled() {
        for sw in &saved.windows {
            strip::restore_window(app, sw);
        }
    } else {
        #[cfg(target_os = "macos")]
        for sw in &saved.windows {
            crate::windows::restore_macos_window(app, sw);
        }
    }
    state.restoring.store(false, Ordering::SeqCst);
    // Guard against a total restore failure leaving the app with zero windows
    // (on Win/Linux a later Destroyed event could then quit the app). If
    // nothing was built, fall through so the caller opens one bare window.
    if crate::windows::content_window_handles(app).is_empty() {
        log::warn!("session: restore produced no windows — opening a fresh one");
        return false;
    }
    // Deliberately DON'T persist here: the restored tabs haven't navigated yet
    // (the seed+navigate is deferred), so a capture now would read about:blank
    // and clobber the saved URLs. `last_session` is already seeded with the
    // saved blob; the periodic autosave re-captures once the tabs have loaded.
    true
}

/// Record that a webview/window has committed at least one real navigation, so
/// session capture may safely read its live URL (see `AppState.navigated`).
pub fn mark_navigated(app: &AppHandle, label: &str) {
    app.state::<AppState>()
        .navigated
        .lock()
        .unwrap()
        .insert(label.to_string());
}

/// Whether `label` has committed a real navigation (URL is non-nil).
pub fn has_navigated(app: &AppHandle, label: &str) -> bool {
    app.state::<AppState>()
        .navigated
        .lock()
        .unwrap()
        .contains(label)
}

/// Drop a closed webview/window from the navigated set.
pub fn forget_navigated(app: &AppHandle, label: &str) {
    app.state::<AppState>()
        .navigated
        .lock()
        .unwrap()
        .remove(label);
}

/// Read a webview's current URL, tolerating the engine not having set one yet.
/// wry's `url()` unwraps the native URL (nil for a freshly-created, not-yet-
/// navigated webview) and panics — and capture can race a just-opened tab — so
/// guard it. Returns None for nil/`about:` URLs (callers fall back to target).
pub fn capture_url(get: impl FnOnce() -> tauri::Result<url::Url>) -> Option<String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| get().ok()))
        .ok()
        .flatten()
        .map(|u| u.to_string())
        .filter(|u| !u.starts_with("about:"))
}

/// Frame helper shared by both capture paths.
pub fn frame_of(win: &tauri::Window) -> Option<[i64; 4]> {
    if win.is_minimized().unwrap_or(false) || win.is_fullscreen().unwrap_or(false) {
        return None;
    }
    let pos = win.outer_position().ok()?;
    let size = win.inner_size().ok()?;
    if size.width < 200 || size.height < 200 {
        return None;
    }
    Some([
        pos.x as i64,
        pos.y as i64,
        size.width as i64,
        size.height as i64,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_round_trips() {
        let s = Session {
            mode: "direct".into(),
            target: "http://localhost:8787".into(),
            windows: vec![SessionWindow {
                frame: Some([10, 20, 1280, 830]),
                active: 1,
                tabs: vec![
                    SessionTab {
                        url: "http://localhost:8787/".into(),
                        profile: None,
                        partition: Some("tab-1-1".into()),
                        custom_title: None,
                    },
                    SessionTab {
                        url: "http://localhost:8787/c/42".into(),
                        profile: Some("work".into()),
                        partition: Some("tab-1-2".into()),
                        custom_title: Some("My Renamed Tab".into()),
                    },
                ],
            }],
        };
        let back: Session = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.windows.len(), 1);
        assert_eq!(back.windows[0].active, 1);
        assert_eq!(back.windows[0].frame, Some([10, 20, 1280, 830]));
        assert_eq!(back.windows[0].tabs[1].profile.as_deref(), Some("work"));
        assert_eq!(
            back.windows[0].tabs[1].partition.as_deref(),
            Some("tab-1-2")
        );
        assert_eq!(
            back.windows[0].tabs[1].custom_title.as_deref(),
            Some("My Renamed Tab")
        );
    }

    #[test]
    fn profile_cookie_matches_webui_attrs() {
        // Must reconstruct the host-only + HttpOnly + Path=/ cookie the server
        // sets, or a restored tab wouldn't be recognized on the right profile.
        let c = profile_cookie("work");
        assert_eq!(c.name(), "hermes_profile");
        assert_eq!(c.value(), "work");
        assert_eq!(c.path(), Some("/"));
        assert_eq!(c.http_only(), Some(true));
        assert_eq!(c.domain(), None); // host-only
    }

    #[test]
    fn tolerates_partial_blob() {
        // Older/partial saved sessions must deserialize via serde defaults
        // rather than dropping the whole restore.
        let j =
            r#"{"mode":"direct","target":"http://x","windows":[{"tabs":[{"url":"http://x"}]}]}"#;
        let s: Session = serde_json::from_str(j).unwrap();
        assert_eq!(s.windows[0].active, 0);
        assert!(s.windows[0].frame.is_none());
        assert!(s.windows[0].tabs[0].profile.is_none());
    }
}
