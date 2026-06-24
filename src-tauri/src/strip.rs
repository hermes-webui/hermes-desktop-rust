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
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use tauri::webview::{Color, Cookie, WebviewBuilder};
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

/// Numeric tab-sequence suffix of a partition id / tab label like `tab-1-3` → 3
/// (used to advance `tab_seq` past reused partition names on restore, #28).
fn partition_suffix(id: &str) -> Option<u64> {
    id.rsplit('-').next().and_then(|n| n.parse::<u64>().ok())
}

fn find_webview(win: &Window<Wry>, label: &str) -> Option<Webview<Wry>> {
    win.webviews().into_iter().find(|w| w.label() == label)
}

fn content_bounds(win: &Window<Wry>, top: f64) -> (LogicalPosition<f64>, LogicalSize<f64>) {
    let scale = win.scale_factor().unwrap_or(1.0);
    let size = win
        .inner_size()
        .map(|s| s.to_logical::<f64>(scale))
        .unwrap_or(LogicalSize::new(1280.0, 830.0));
    (
        LogicalPosition::new(0.0, top),
        LogicalSize::new(size.width, (size.height - top).max(0.0)),
    )
}

/// Whether the strip is hidden for `window_label` (issue #10).
fn strip_hidden(app: &AppHandle, window_label: &str) -> bool {
    let state = app.state::<AppState>();
    let strip = state.strip.lock().unwrap();
    strip
        .get(window_label)
        .map(|e| e.strip_hidden)
        .unwrap_or(false)
}

/// Y offset where the content webview begins: 0 when the strip is hidden, else
/// the strip height.
fn content_top(app: &AppHandle, window_label: &str) -> f64 {
    if strip_hidden(app, window_label) {
        0.0
    } else {
        STRIP_HEIGHT
    }
}

/// Hide/show the tab strip for `window_label` (issue #10). Windows-only: macOS
/// uses native tabs (no strip) and Linux can't re-fit GTK child webviews
/// (constraint #1), so this is a no-op there. Caller should be off the main
/// thread (it resizes the content webview).
pub fn toggle_strip(app: &AppHandle, window_label: &str) {
    if cfg!(target_os = "linux") {
        log::info!("strip: tab-bar toggle unavailable on Linux (GTK child-webview geometry)");
        return;
    }
    let now_hidden = {
        let state = app.state::<AppState>();
        let mut strip = state.strip.lock().unwrap();
        let Some(entry) = strip.get_mut(window_label) else {
            return;
        };
        entry.strip_hidden = !entry.strip_hidden;
        entry.strip_hidden
    };
    layout(app, window_label);
    // Discoverability (issue #10): hiding the strip removes the ⋯ button — the
    // only visible affordance — so until the user has proven they know how to
    // bring it back, surface an OS notification with the shortcut every time
    // they hide it.
    //
    // We retire the hint on the UN-HIDE transition, NOT on hide. Un-hiding (on
    // Windows, the only platform this runs on for real) REQUIRES Ctrl+Shift+B
    // since the ⋯ button is gone — so a successful un-hide is positive proof
    // the user learned the shortcut, and only then do we stop hinting. Gating
    // on the hide instead (the previous logic) burned the one-time flag even
    // when the toast was silently dropped — Focus Assist / Do-Not-Disturb,
    // notifications disabled for the app, or an unpackaged dev build — leaving
    // a first-time hider permanently stuck with the hint already "spent". This
    // way the hint simply re-fires on the next hide until it actually lands.
    if now_hidden {
        if !prefs::hide_hint_shown(app) {
            use tauri_plugin_notification::NotificationExt;
            let _ = app
                .notification()
                .builder()
                .title("Tab bar hidden")
                .body("Press Ctrl+Shift+B to show it again.")
                .show();
        }
    } else if !prefs::hide_hint_shown(app) {
        // Just un-hid → they know the shortcut now. Stop hinting.
        prefs::set_hide_hint_shown(app);
    }
}

/// Per-tab webview data partition path. Each tab gets its own directory → its
/// own cookie jar, so the WebUI's HttpOnly `hermes_profile` cookie (and the
/// login session) is scoped to that tab instead of shared across every tab
/// through one store — the profile-bleed root cause in issue #3.
fn tab_partition_dir(app: &AppHandle, tab_label: &str) -> Option<PathBuf> {
    app.path()
        .app_local_data_dir()
        .ok()
        .map(|d| d.join("tab-partitions").join(tab_label))
}

/// Best-effort removal of a closed tab's data partition. The startup wipe
/// (`clear_partitions`) is the safety net if this fails because the OS still
/// holds the folder open immediately after the webview is destroyed.
fn remove_tab_partition(app: &AppHandle, tab_label: &str) {
    if let Some(dir) = tab_partition_dir(app, tab_label) {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Wipe ORPHAN tab data partitions at startup (before any tab opens). Keeps the
/// partitions referenced by the saved session so a restored tab's login +
/// cookies survive the restart (issue #28); removes the rest — jars from closed
/// windows, crashes, or pre-0.5.0 session-scoped runs — so they don't pile up.
pub fn clear_partitions(app: &AppHandle) {
    let keep: std::collections::HashSet<String> = crate::session::load(app)
        .map(|s| {
            s.windows
                .iter()
                .flat_map(|w| w.tabs.iter())
                .filter_map(|t| t.partition.clone())
                .collect()
        })
        .unwrap_or_default();
    if let Ok(base) = app.path().app_local_data_dir() {
        let dir = base.join("tab-partitions");
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !keep.contains(&name) {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
    }
}

/// Open a new strip window with its first tab. Runs on the caller's thread —
/// callers must already be off the main thread (CLAUDE.md invariant #9).
pub fn open_browser_window(app: &AppHandle, p: &prefs::Prefs) {
    if let Some(label) = build_strip_window(app, p, None) {
        add_tab(app, &label);
        log::info!("strip: window {label} ready");
    }
}

/// Build a strip window (OS window + 38px shell webview + frame) WITHOUT any
/// content tab, returning its label. Shared by `open_browser_window` (then adds
/// one tab) and session restore (then adds the saved tabs). `frame_override`,
/// when set, positions/sizes the window to a saved frame instead of the
/// first-window-restore / cascade default. Caller must be off the main thread.
fn build_strip_window(
    app: &AppHandle,
    p: &prefs::Prefs,
    frame_override: Option<[i64; 4]>,
) -> Option<String> {
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
            return None;
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
        .initialization_script(&init)
        // Drag-to-reorder tabs (#19) is HTML5 drag in shell.html. Without this,
        // wry's native OS drag-drop handler intercepts the gesture and the page
        // never sees dragstart/drop — so reorder silently no-ops on Windows
        // (same root cause as #27 for content webviews). The strip hosts only
        // the tab bar + ⋯ menu; it needs no native file-drop, and window-drag
        // (data-tauri-drag-region) is a separate mechanism, unaffected.
        .disable_drag_drop_handler();
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

    // Frame: a saved frame (restore) wins; else first window restores the
    // persisted frame / centers, and others cascade off the host.
    if let Some([x, y, w, h]) = frame_override.filter(|f| f[2] >= 200 && f[3] >= 200) {
        let _ = win.set_size(tauri::PhysicalSize::new(w as u32, h as u32));
        let _ = win.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
    } else if is_first {
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
    Some(label)
}

/// Recreate one saved strip window and its tabs (issue #18). Each tab reloads
/// its saved URL and is re-seeded with its `hermes_profile` cookie so it
/// reopens on the same profile. Caller must be off the main thread.
pub fn restore_window(app: &AppHandle, sw: &crate::session::SessionWindow) {
    if sw.tabs.is_empty() {
        return;
    }
    let p = prefs::load(app);
    let Some(label) = build_strip_window(app, &p, sw.frame) else {
        return;
    };
    for tab in &sw.tabs {
        let url = match url::Url::parse(&tab.url) {
            Ok(u) => u,
            Err(_) => match url::Url::parse(&p.target_url) {
                Ok(u) => u,
                Err(_) => continue,
            },
        };
        // ALWAYS re-seed the profile cookie for a restored tab (issue #30/#37),
        // even when reusing the partition. `hermes_profile` is a SESSION cookie
        // (no expiry), and WebView2 discards session cookies from the
        // data_directory on process restart — so reusing the jar does NOT bring
        // the profile back on Windows: the tab boots on the default profile, and
        // its saved `/session/<id>` deep-link is then profile-gated to a 404
        // ("Session not available", #37), bouncing to root (#30). Re-seeding the
        // captured profile selector re-establishes it deterministically instead
        // of relying on cookie persistence. The partition is still REUSED
        // (login/localStorage survive); we only add back the one selector
        // cookie. (macOS strip = HERMES_FORCE_STRIP dev only, no data_directory,
        // so seeding is a no-op there.)
        let seed: Vec<Cookie<'static>> = if cfg!(not(target_os = "macos")) {
            tab.profile
                .as_deref()
                .map(|v| {
                    let mut c = crate::session::profile_cookie(v);
                    // CRITICAL (#30/#37): pin the cookie to the navigation host.
                    // wry's `set_cookie` carries no URL, so on WebView2 a
                    // domain-less cookie becomes `CreateCookie(.., domain="")`
                    // — which has no host to match and is SILENTLY never sent,
                    // leaving the restored tab on the default profile (the bug).
                    // Setting the domain to the target host (host-only-
                    // equivalent for localhost) is what actually makes the
                    // re-seed take effect on Windows. macOS native restore keeps
                    // the domain-less form WKWebView accepts (windows.rs).
                    if let Some(h) = url.host_str() {
                        c.set_domain(h.to_string());
                    }
                    vec![c]
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        // Arm the boot-404 retry (#37) only for a deep (non-root) session URL;
        // a root restore has nothing to recover.
        let restore_retry = (!matches!(url.path(), "" | "/")).then(|| url.clone());
        add_tab_with(
            app,
            &label,
            TabSpec {
                target: url,
                seed,
                partition_override: tab.partition.clone(),
                profile_hint: tab.profile.clone(),
                custom_title: tab.custom_title.clone(),
                restore_retry,
            },
        );
    }
    // Advance tab_seq past the highest REUSED partition suffix (#28 collision):
    // tab_seq restarts at 0 each launch, but restored tabs reuse their old
    // partition names (e.g. `tab-1-3`, with gaps from closed/reordered tabs).
    // Without this, a later new tab could be labeled `tab-1-3` and its
    // remove-before-create would wipe the restored tab's live jar — losing its
    // login and making two tabs share one jar (the #3 bleed). Bumping the
    // counter past every reused suffix guarantees new tabs get fresh names.
    {
        let max_suffix = sw
            .tabs
            .iter()
            .filter_map(|t| t.partition.as_deref())
            .filter_map(partition_suffix)
            .max()
            .unwrap_or(0);
        let state = app.state::<AppState>();
        let mut strip = state.strip.lock().unwrap();
        if let Some(entry) = strip.get_mut(&label) {
            entry.tab_seq = entry.tab_seq.max(max_suffix);
        }
    }
    // Select the saved active tab.
    let active_label = {
        let state = app.state::<AppState>();
        let strip = state.strip.lock().unwrap();
        strip
            .get(&label)
            .and_then(|e| e.tabs.get(sw.active.min(e.tabs.len().saturating_sub(1))))
            .map(|t| t.label.clone())
    };
    if let Some(al) = active_label {
        select_tab(app, &label, &al);
    }
    log::info!(
        "strip: restored window {label} with {} tab(s)",
        sw.tabs.len()
    );
}

/// Add a tab to an existing strip window, seeded from the currently-active
/// tab's cookie jar so it inherits the active profile + login (issue #3), then
/// diverges. Caller must be off the main thread (CLAUDE.md invariant #9).
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

    // Read the active tab's cookies BEFORE the new webview exists — the seed for
    // the fresh jar. Safe on this worker thread: the cookie dispatcher marshals
    // to the UI thread without the main-thread deadlock the docs warn about.
    //
    // cookies() (whole store), NOT cookies_for_url(): WKWebView's URL filter
    // drops host-only cookies (the WebUI sets `hermes_profile` host-only) — the
    // macOS inheritance bug (#3). A content tab only ever loads the one target
    // origin, so its whole store IS that origin's cookies — uniform + robust.
    let seed: Vec<Cookie<'static>> = if cfg!(not(target_os = "macos")) {
        let opener = {
            let state = app.state::<AppState>();
            let strip = state.strip.lock().unwrap();
            strip
                .get(window_label)
                .and_then(|e| e.tabs.get(e.active))
                .map(|t| t.label.clone())
        };
        match opener
            .and_then(|lbl| find_webview(&win, &lbl))
            .or_else(|| focused_active_webview(app))
        {
            Some(opener) => {
                match opener.cookies() {
                    Ok(cookies) => {
                        let names: Vec<&str> = cookies.iter().map(|c| c.name()).collect();
                        log::info!(
                        "strip: seeding new tab in {window_label} from opener — {} cookie(s): {:?}",
                        cookies.len(),
                        names
                    );
                        cookies
                    }
                    Err(e) => {
                        log::warn!("strip: seed read failed in {window_label} (opens on default profile): {e}");
                        Vec::new()
                    }
                }
            }
            None => {
                log::info!("strip: no opener in {window_label} (opens on default profile)");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    add_tab_with(
        app,
        window_label,
        TabSpec {
            target,
            seed,
            partition_override: None,
            profile_hint: None,
            custom_title: None,
            restore_retry: None,
        },
    );
}

/// Inputs to `add_tab_with` so `add_tab` (new tab, opener-seeded fresh jar) and
/// `restore_window` (saved tab, reused jar) share one creation path.
pub(crate) struct TabSpec {
    pub target: url::Url,
    /// Cookies to seed into a FRESH jar (new tab); empty when reusing a jar.
    pub seed: Vec<Cookie<'static>>,
    /// Some = reuse this on-disk partition dir (restore → login/cookies survive,
    /// issue #28); None = a fresh jar keyed by the new tab label.
    pub partition_override: Option<String>,
    /// Initial profile dot before the first load re-reads the cookie (restore).
    pub profile_hint: Option<String>,
    /// Restored user-given tab name (issue #7).
    pub custom_title: Option<String>,
    /// Set (to the saved deep-link URL) for a RESTORED tab on a non-root
    /// session. If the boot load bounces to root — the WebUI's self-heal when
    /// `GET /api/session` 404s transiently behind a proxy (issue #37) — the tab
    /// re-navigates here once, mirroring the user's switch-away-and-back
    /// recovery. None for normal new tabs and root restores.
    pub restore_retry: Option<url::Url>,
}

/// Shared tab-creation body: build an isolated content webview, append it to
/// `window_label`'s strip and make it active. Caller must be off the main
/// thread.
pub(crate) fn add_tab_with(app: &AppHandle, window_label: &str, spec: TabSpec) {
    let TabSpec {
        target,
        seed,
        partition_override,
        profile_hint,
        custom_title,
        restore_retry,
    } = spec;
    let Some(win) = app.windows().get(window_label).cloned() else {
        return;
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
    // Paint the native webview surface in the cached theme color so a freshly
    // added tab never flashes white before its first paint (issue #4). On
    // Windows wry clamps any non-zero alpha to fully opaque — what we want.
    let to8 = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let bg = Color(to8(r), to8(g), to8(b), 255);
    // No injected ssh footer in strip mode — status lives in the strip.
    let init = bridge::init_script(&tab_label, &hex, false);

    // Per-tab cookie isolation (issue #3): each tab gets its own data partition
    // (separate jar). A restored tab REUSES its saved partition so login +
    // cookies survive the restart (issue #28); a new tab gets a fresh jar keyed
    // by its label. macOS (HERMES_FORCE_STRIP) has no data_directory → no jar.
    let reuse = partition_override.is_some();
    let partition_id = partition_override.unwrap_or_else(|| tab_label.clone());
    let partition = if cfg!(not(target_os = "macos")) {
        tab_partition_dir(app, &partition_id)
    } else {
        None
    };
    if let Some(ref dir) = partition {
        if reuse {
            let _ = std::fs::create_dir_all(dir); // keep the existing jar
        } else {
            let _ = std::fs::remove_dir_all(dir); // guarantee a fresh jar
            let _ = std::fs::create_dir_all(dir);
        }
    }
    // The profile this tab opens on — a restore hint, else the seed cookie's
    // value — shown in the strip's profile dot before the first load re-reads it
    // (issue #8).
    let seed_profile = profile_hint.or_else(|| profile_from_cookies(&seed));

    // Seed whenever we have cookies to inject into an isolated jar — INCLUDING a
    // reused jar (issue #30/#37): the reused jar may have lost the session-scoped
    // `hermes_profile` cookie across restart (WebView2 drops session cookies), so
    // a restored tab must re-seed its profile selector before navigating, or its
    // deep-link load 404s under the wrong profile. set_cookie only adds/updates
    // that one cookie; the rest of the reused jar (login/localStorage) is intact.
    let do_seed = partition.is_some() && !seed.is_empty();
    // about:blank first ONLY when seeding (so the cookie lands before the real
    // navigation); reused / un-isolated tabs load the target directly.
    let initial_url = if do_seed {
        WebviewUrl::External(url::Url::parse("about:blank").unwrap())
    } else {
        WebviewUrl::External(target.clone())
    };

    let nav_app = app.clone();
    let load_window = window_label.to_string();
    // Disable wry's native OS drag-drop handler so the WebUI's own HTML5
    // drag-drop works (issue #27): with it enabled, the native layer intercepts
    // drag events over the webview and the page never sees `dragstart`/`drop`,
    // so dragging a workspace-tree item onto the composer shows the OS "no-drop"
    // cursor and never registers. Off, the page handles in-page drag AND
    // file-from-Finder drops (→ the WebUI's upload handler). The shell
    // intercepts no native drops, so nothing is lost.
    let mut wb = WebviewBuilder::new(&tab_label, initial_url)
        .background_color(bg)
        .disable_drag_drop_handler();
    if let Some(ref dir) = partition {
        wb = wb.data_directory(dir.clone());
    }
    let wb = wb
        .initialization_script(&init)
        .on_navigation(move |url| {
            windows::navigation_allowed(&nav_app, url, allowed_host.as_deref())
        })
        // Native title source — replaces the JS EMIT('title') watcher, which
        // silently no-ops in remote-origin tab webviews (issue #15).
        .on_document_title_changed(|wv, title| {
            windows::apply_reported_title(wv.app_handle(), wv.label(), &title);
        })
        .on_page_load(move |webview, payload| {
            if !matches!(payload.event(), tauri::webview::PageLoadEvent::Finished) {
                return;
            }
            // Ignore the pre-seed about:blank load (isolated tabs only).
            if payload.url().scheme() == "about" {
                return;
            }
            let app = webview.app_handle().clone();
            // This tab has a non-nil URL now — capture may read it (#18 crash
            // guard: never call url() on a not-yet-navigated webview).
            crate::session::mark_navigated(&app, webview.label());
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
            // Re-read this tab's `hermes_profile` cookie after the real load
            // (the server may have just set/changed it) and refresh the strip's
            // profile dot (#8) + persist the session (#18). Off the wry callout:
            // the cookie read marshals via the dispatcher (wry#583 deadlock).
            let cap_app = app.clone();
            let cap_win = load_window.clone();
            let cap_tab = webview.label().to_string();
            std::thread::spawn(move || capture_tab_profile(&cap_app, &cap_win, &cap_tab));
        });

    let (pos, size) = content_bounds(&win, content_top(app, window_label));
    let webview = match win.add_child(wb, pos, size) {
        Ok(w) => w,
        Err(e) => {
            log::error!("strip: tab webview failed: {e}");
            return;
        }
    };

    // Seed the FRESH jar, then load the real target — cookies are committed
    // before the navigation request fires (set_cookie is synchronous on
    // WebKitGTK; WebView2's AddOrUpdateCookie completes before navigate).
    if do_seed {
        for cookie in seed {
            let _ = webview.set_cookie(cookie);
        }
        if let Err(e) = webview.navigate(target.clone()) {
            log::error!("strip: tab navigate failed: {e}");
        }
    }

    {
        let mut strip = state.strip.lock().unwrap();
        let Some(entry) = strip.get_mut(window_label) else {
            return;
        };
        if let Some(prev) = entry.tabs.get(entry.active) {
            if let Some(prev_wv) = find_webview(&win, &prev.label) {
                let _ = prev_wv.hide();
                set_tab_backgrounded(&prev_wv, true); // hidden by the new tab (#32)
            }
        }
        let title = custom_title.clone().unwrap_or_else(|| "New Tab".into());
        entry.tabs.push(TabEntry {
            label: tab_label.clone(),
            title,
            attention: false,
            busy: false,
            profile: seed_profile.clone(),
            // Seed the dot from the profile we know at creation so a tab with a
            // cookie shows its color instantly; the page's active-profile
            // reporter sets/refines it after load — including for the DEFAULT
            // profile, which now gets its own colored dot like any other
            // (issue #8/#31, v0.6.3). No filter: every profile shows a color.
            dot_profile: seed_profile,
            partition: partition_id,
            custom_title,
        });
        entry.active = entry.tabs.len() - 1;
    }
    let _ = webview.set_focus();
    log::info!("strip: tab {tab_label} added to {window_label}");
    emit_tabs(app, window_label);
    refresh_window_title(app, window_label);
    crate::session::persist(app);

    // Boot-time session-restore retry (issue #37): a restored deep-link tab can
    // 404 on its first `GET /api/session` behind a proxy (server/session index
    // not ready yet), and the WebUI self-heals by bouncing the URL to root and
    // showing "Session not available". The session IS reachable a beat later
    // (the user's switch-away-and-back recovery proves it). So once, ~2.5s after
    // create, if the tab has bounced to root, re-navigate to the saved deep URL.
    // No-op if it loaded fine (route still deep) → no penalty on success.
    if let Some(deep) = restore_retry {
        let app2 = app.clone();
        let wv2 = webview.clone();
        let tab2 = tab_label.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(2500));
            let bounced = crate::session::reported_url(&app2, &tab2)
                .as_deref()
                .map(url_is_root)
                .unwrap_or(false);
            if bounced {
                log::info!("strip: restored tab {tab2} bounced to root — retrying {deep}");
                let _ = wv2.navigate(deep);
            }
        });
    }
}

/// Whether a reported URL is the WebUI root (path `/` or empty) — the shape the
/// WebUI bounces a tab to when a boot-time deep-link session load 404s (#37).
fn url_is_root(u: &str) -> bool {
    url::Url::parse(u)
        .map(|p| matches!(p.path(), "" | "/"))
        .unwrap_or(false)
}

/// Tell a tab's page whether it's backgrounded (issue #32). The WebUI gates OS
/// notifications on `document.hidden`, but a strip tab hidden via `wv.hide()`
/// still reports visible — so a hidden-but-streaming tab never fired its
/// completion/approval notification. The WebUI exposes
/// `window.__hermesSetBackgrounded(bool)` (hermes-webui #4753), which feeds the
/// notification gate ONLY (not the SSE-close-on-hidden path), so a background
/// tab notifies AND keeps streaming. No-op until the page defines it.
fn set_tab_backgrounded(wv: &Webview<Wry>, backgrounded: bool) {
    let _ = wv.eval(&format!(
        "window.__hermesSetBackgrounded&&window.__hermesSetBackgrounded({backgrounded})"
    ));
}

/// Mark a strip window's currently-visible tab backgrounded/foregrounded (#32),
/// used when the whole window gains/loses OS focus (app switch) — so a run
/// completing in the active tab while the app is in the background still
/// notifies. Hidden tabs are already flagged backgrounded by `select_tab`.
pub fn set_active_tab_backgrounded(app: &AppHandle, window_label: &str, backgrounded: bool) {
    let Some(win) = app.windows().get(window_label).cloned() else {
        return;
    };
    let active = {
        let state = app.state::<AppState>();
        let strip = state.strip.lock().unwrap();
        strip
            .get(window_label)
            .and_then(|e| e.tabs.get(e.active))
            .map(|t| t.label.clone())
    };
    if let Some(label) = active {
        if let Some(wv) = find_webview(&win, &label) {
            set_tab_backgrounded(&wv, backgrounded);
        }
    }
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
                set_tab_backgrounded(&wv, true); // now backgrounded (#32)
            }
        }
    }
    if let Some(wv) = find_webview(&win, tab_label) {
        // Linux: NEVER call set_position/set_size on an existing GTK child
        // webview — it crashes natively (isolated by Linux smoke v3/v5;
        // creation-time bounds render fine). Show/hide alone is safe.
        if !cfg!(target_os = "linux") {
            let (pos, size) = content_bounds(&win, content_top(app, window_label));
            let _ = wv.set_position(pos);
            let _ = wv.set_size(size);
        }
        let _ = wv.show();
        let _ = wv.set_focus();
        set_tab_backgrounded(&wv, false); // now foreground (#32)
    }
    emit_tabs(app, window_label);
    refresh_window_title(app, window_label);
    // Re-read the activated tab's profile — catches a profile switch made inside
    // it while it was hidden, so the dot is correct on switch-back (issue #26).
    capture_tab_profile(app, window_label, tab_label);
    crate::session::persist(app);
}

pub fn close_tab(app: &AppHandle, window_label: &str, tab_label: &str) {
    let Some(win) = app.windows().get(window_label).cloned() else {
        return;
    };
    let state = app.state::<AppState>();
    let (remaining, next_active, closed_partition) = {
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
        let closed_partition = entry.tabs[idx].partition.clone();
        entry.tabs.remove(idx);
        if entry.active >= entry.tabs.len() {
            entry.active = entry.tabs.len() - 1;
        } else if idx < entry.active {
            entry.active -= 1;
        }
        (
            entry.tabs.len(),
            entry.tabs.get(entry.active).map(|t| t.label.clone()),
            closed_partition,
        )
    };
    state.raw_titles.lock().unwrap().remove(tab_label);
    crate::session::forget_navigated(app, tab_label);
    crate::session::forget_url(app, tab_label);
    if let Some(wv) = find_webview(&win, tab_label) {
        let _ = wv.close();
    }
    // The user explicitly closed this tab → drop its jar (by partition id, which
    // for a restored tab differs from the regenerated label).
    remove_tab_partition(app, &closed_partition);
    if let Some(next) = next_active {
        select_tab(app, window_label, &next);
    }
    log::info!("strip: closed {tab_label}, {remaining} tabs remain in {window_label}");
    crate::session::persist(app);
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

/// Capture every strip window as a session window (issue #18). Each tab's live
/// URL is read from its webview; order/active/profile come from the registry.
/// Must run on the main thread (the webview URL getter touches the runtime).
pub fn session_windows(app: &AppHandle) -> Vec<crate::session::SessionWindow> {
    use crate::session::{SessionTab, SessionWindow};
    let p = prefs::load(app);
    let state = app.state::<AppState>();
    let mut out = Vec::new();
    for win in windows::content_window_handles(app) {
        let label = win.label().to_string();
        let (tabs_meta, active) = {
            let strip = state.strip.lock().unwrap();
            match strip.get(&label) {
                Some(e) => (e.tabs.clone(), e.active),
                None => continue,
            }
        };
        let tabs: Vec<SessionTab> = tabs_meta
            .iter()
            .map(|t| {
                // Prefer the page-reported live URL (captures SPA routes, #30).
                // Fall back to wry's url() only once the tab has navigated (it
                // panics on a not-yet-navigated webview on macOS), then to root.
                let url = crate::session::reported_url(app, &t.label)
                    .filter(|u| !u.starts_with("about:"))
                    .or_else(|| {
                        if crate::session::has_navigated(app, &t.label) {
                            find_webview(&win, &t.label)
                                .and_then(|wv| crate::session::capture_url(|| wv.url()))
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| p.target_url.clone());
                SessionTab {
                    url,
                    profile: t.profile.clone(),
                    // Persist the on-disk jar dir so restore reuses it (login
                    // survives, #28). Empty only on macOS forced-strip (no jar).
                    partition: (!t.partition.is_empty()).then(|| t.partition.clone()),
                    custom_title: t.custom_title.clone(),
                }
            })
            .collect();
        if tabs.is_empty() {
            continue;
        }
        out.push(SessionWindow {
            frame: crate::session::frame_of(&win),
            active: active.min(tabs.len() - 1),
            tabs,
        });
    }
    out
}

/// Recompute strip + active tab bounds (window Resized handler).
pub fn layout(app: &AppHandle, window_label: &str) {
    // Linux: re-fitting GTK child webviews crashes natively (smoke v3/v5
    // finding) — skip entirely. Cost: window resizes don't re-fit webviews
    // there yet; tracked for the next sprint (upstream wry GTK geometry).
    if cfg!(target_os = "linux") {
        return;
    }
    let Some(win) = app.windows().get(window_label).cloned() else {
        return;
    };
    let scale = win.scale_factor().unwrap_or(1.0);
    let Ok(size) = win.inner_size() else { return };
    let logical = size.to_logical::<f64>(scale);
    log::debug!(
        "strip: layout {window_label} inner={}x{} scale={scale} logical={}x{}",
        size.width,
        size.height,
        logical.width,
        logical.height
    );
    let hidden = strip_hidden(app, window_label);
    let strip_label = format!("strip-{}", window_seq(window_label));
    if let Some(strip_wv) = find_webview(&win, &strip_label) {
        if hidden {
            // Tab bar hidden (#10) — content reclaims the full window.
            let _ = strip_wv.hide();
        } else {
            let _ = strip_wv.show();
            let _ = strip_wv.set_position(LogicalPosition::new(0.0, 0.0));
            let _ = strip_wv.set_size(LogicalSize::new(logical.width, STRIP_HEIGHT));
        }
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
            let (pos, sz) = content_bounds(&win, if hidden { 0.0 } else { STRIP_HEIGHT });
            let _ = wv.set_position(pos);
            let _ = wv.set_size(sz);
        }
    }
}

/// A tab reported a new title (marker-free) and its pending-attention state.
/// Called from `windows::apply_reported_title`, which sources both from wry's
/// native title-changed hook and has already stripped the "● " marker.
pub fn set_tab_title(app: &AppHandle, tab_label: &str, title: &str, attention: bool) {
    let Some(window_label) = window_of_tab(tab_label) else {
        return;
    };
    let state = app.state::<AppState>();
    state
        .raw_titles
        .lock()
        .unwrap()
        .insert(tab_label.to_string(), title.to_string());
    let changed = {
        let mut strip = state.strip.lock().unwrap();
        match strip
            .get_mut(&window_label)
            .and_then(|e| e.tabs.iter_mut().find(|t| t.label == tab_label))
        {
            Some(tab) => {
                let p = prefs::load(app);
                // A user-renamed tab (#7) keeps its name regardless of the page
                // title; the page title is still recorded (raw_titles above) so
                // clearing the rename can fall back to it.
                let new_title = if tab.custom_title.is_none() {
                    windows::display_title(title, &p.connection_mode, &p.target_url, true)
                } else {
                    tab.title.clone()
                };
                let changed = tab.title != new_title || tab.attention != attention;
                tab.title = new_title;
                tab.attention = attention;
                changed
            }
            None => false,
        }
    };
    // Emit only when something the strip renders actually changed — the WebUI
    // mutates document.title constantly (token streaming), and emitting on every
    // one floods the strip with rebuilds (it destroyed the open rename input,
    // #38, and is needless churn). refresh_window_title is cheap/idempotent.
    if changed {
        emit_tabs(app, &window_label);
        refresh_window_title(app, &window_label);
    }
}

/// Set a tab's display profile name for the dot (issue #31), reported by the
/// page's active-profile reporter. Independent of the `hermes_profile` cookie
/// (which the WebUI doesn't set until an explicit switch), so a tab on its
/// starting profile still gets a dot. Empty name = default profile = no dot.
/// Emits only on change. No webview read here (just a string from the bridge),
/// so it's safe regardless of the menu modal loop (#33).
pub fn set_tab_dot_profile(app: &AppHandle, tab_label: &str, name: &str) {
    let Some(window_label) = window_of_tab(tab_label) else {
        return;
    };
    let value = (!name.is_empty()).then(|| name.to_string());
    let state = app.state::<AppState>();
    let changed = {
        let mut strip = state.strip.lock().unwrap();
        match strip
            .get_mut(&window_label)
            .and_then(|e| e.tabs.iter_mut().find(|t| t.label == tab_label))
        {
            Some(tab) if tab.dot_profile != value => {
                tab.dot_profile = value;
                true
            }
            _ => false,
        }
    };
    if changed {
        emit_tabs(app, &window_label);
    }
}

/// Set a tab's busy (actively-streaming) flag (issue #46), reported by the
/// page's busy reporter (the WebUI's `S.busy`). Emit-on-change so a streaming
/// tab — whose `S.busy` is polled, not its constantly-mutating title — doesn't
/// flood the strip. No webview read (just a bool from the bridge), so it's safe
/// regardless of the menu modal loop (#33).
pub fn set_tab_busy(app: &AppHandle, tab_label: &str, busy: bool) {
    let Some(window_label) = window_of_tab(tab_label) else {
        return;
    };
    let state = app.state::<AppState>();
    let changed = {
        let mut strip = state.strip.lock().unwrap();
        match strip
            .get_mut(&window_label)
            .and_then(|e| e.tabs.iter_mut().find(|t| t.label == tab_label))
        {
            Some(tab) if tab.busy != busy => {
                tab.busy = busy;
                true
            }
            _ => false,
        }
    };
    if changed {
        emit_tabs(app, &window_label);
    }
}

/// Rename a strip tab (issue #7). `name` = the user's label, or None/empty to
/// clear it and fall back to the page title. Persisted so it survives restart.
pub fn rename_tab(app: &AppHandle, window_label: &str, tab_label: &str, name: Option<String>) {
    let name = name.and_then(|n| {
        let t = n.trim();
        (!t.is_empty()).then(|| t.chars().take(60).collect::<String>())
    });
    let state = app.state::<AppState>();
    {
        let p = prefs::load(app);
        let raw = state
            .raw_titles
            .lock()
            .unwrap()
            .get(tab_label)
            .cloned()
            .unwrap_or_default();
        let mut strip = state.strip.lock().unwrap();
        let Some(tab) = strip
            .get_mut(window_label)
            .and_then(|e| e.tabs.iter_mut().find(|t| t.label == tab_label))
        else {
            return;
        };
        tab.title = match &name {
            Some(n) => n.clone(),
            None => windows::display_title(&raw, &p.connection_mode, &p.target_url, true),
        };
        tab.custom_title = name;
    }
    emit_tabs(app, window_label);
    refresh_window_title(app, window_label);
    crate::session::persist(app);
}

/// Extract the `hermes_profile` cookie value from a cookie set. `None` means
/// the default profile (the WebUI sets no such cookie for it).
pub fn profile_from_cookies(cookies: &[Cookie<'static>]) -> Option<String> {
    cookies
        .iter()
        .find(|c| c.name() == "hermes_profile")
        .map(|c| c.value().to_string())
}

/// Read a tab's current profile from its isolated cookie jar; if it changed,
/// update the registry, repaint the strip (profile dot — #8), and persist the
/// session (#18). Runs on a worker thread — the cookie read marshals via the
/// dispatcher, so it must not be called from inside a wry callout (wry#583).
fn capture_tab_profile(app: &AppHandle, window_label: &str, tab_label: &str) {
    // Guard the cookie read at the CHOKEPOINT, not just at the periodic sweep
    // (#33). `wv.cookies()` is a synchronous WebView2 read that marshals onto —
    // and pumps a nested loop on — the main thread. While the strip's "⋯" popup
    // owns the main thread (TrackPopupMenu modal loop), that re-entry deadlocks
    // the UI. recapture_profiles guards itself, but capture_tab_profile is ALSO
    // reached from the `route` bridge handler (recapture_tab_profile, fired by a
    // background SSE-driven navigation) and on_page_load — both unguarded
    // otherwise. Guarding here covers every caller; the value is re-read on the
    // next route/sweep once the menu closes.
    if app
        .state::<AppState>()
        .menu_open
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return;
    }
    let Some(win) = app.windows().get(window_label).cloned() else {
        return;
    };
    let Some(wv) = find_webview(&win, tab_label) else {
        return;
    };
    let profile = match wv.cookies() {
        Ok(cs) => profile_from_cookies(&cs),
        Err(e) => {
            log::debug!("strip: profile read failed for {tab_label}: {e}");
            return;
        }
    };
    let changed = {
        let state = app.state::<AppState>();
        let mut strip = state.strip.lock().unwrap();
        match strip
            .get_mut(window_label)
            .and_then(|e| e.tabs.iter_mut().find(|t| t.label == tab_label))
        {
            Some(tab) if tab.profile != profile => {
                tab.profile = profile;
                true
            }
            _ => false,
        }
    };
    if changed {
        emit_tabs(app, window_label);
        crate::session::persist(app);
    }
}

/// Re-read every strip tab's profile cookie and repaint dots that changed
/// (issue #26). The per-tab dot otherwise only refreshes on open / full reload,
/// so switching profile *inside* a tab (an SPA re-render, no page load) left it
/// stale, and the first tabs after a relogin could come up with no dot. A light
/// periodic sweep plus a re-capture on tab activation covers both without a
/// WebUI signal. `capture_tab_profile` only emits when a value actually
/// changed, so this is cheap. Runs on a worker thread (cookie reads marshal).
pub fn recapture_profiles(app: &AppHandle) {
    // Cookie reads marshal into each webview via the runtime dispatcher. If a
    // native menu modal loop is up (the strip's "⋯" popup), that marshal would
    // re-enter the main thread from inside the popup's modal loop on Windows and
    // deadlock the UI (#33). Skip while a menu is open — the periodic sweep
    // recovers on the next tick once it closes.
    if app
        .state::<AppState>()
        .menu_open
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return;
    }
    let pairs: Vec<(String, String)> = {
        let state = app.state::<AppState>();
        let strip = state.strip.lock().unwrap();
        strip
            .iter()
            .flat_map(|(win, e)| e.tabs.iter().map(move |t| (win.clone(), t.label.clone())))
            .collect()
    };
    for (win, tab) in pairs {
        capture_tab_profile(app, &win, &tab);
    }
}

/// Re-read one tab's profile immediately. Called from the route reporter
/// (issue #31): a profile/session switch navigates the page, so re-capturing on
/// the route change repaints the dot at once — without waiting for the periodic
/// sweep or needing another tab to open. Caller must be off the main thread.
pub fn recapture_tab_profile(app: &AppHandle, tab_label: &str) {
    if let Some(window_label) = window_of_tab(tab_label) {
        capture_tab_profile(app, &window_label, tab_label);
    }
}

/// Move a tab to a new index within its window's strip (drag-to-reorder, #19).
/// The visible webview is unchanged — only the display order and the stored
/// `Vec` order move; `active` keeps pointing at the same tab label.
pub fn reorder_tab(app: &AppHandle, window_label: &str, tab_label: &str, new_index: usize) {
    let state = app.state::<AppState>();
    let moved = {
        let mut strip = state.strip.lock().unwrap();
        let Some(entry) = strip.get_mut(window_label) else {
            return;
        };
        if entry.tabs.is_empty() {
            return;
        }
        let Some(from) = entry.tabs.iter().position(|t| t.label == tab_label) else {
            return;
        };
        let to = new_index.min(entry.tabs.len() - 1);
        if from == to {
            return;
        }
        let active_label = entry.tabs.get(entry.active).map(|t| t.label.clone());
        let tab = entry.tabs.remove(from);
        entry.tabs.insert(to, tab);
        if let Some(al) = active_label {
            if let Some(idx) = entry.tabs.iter().position(|t| t.label == al) {
                entry.active = idx;
            }
        }
        true
    };
    if moved {
        emit_tabs(app, window_label);
        crate::session::persist(app);
    }
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
            // User color overrides for the profile dots (issue #47).
            "colors": prefs::profile_colors(app),
        }),
        None => json!({ "window": window_label, "tabs": [], "active": 0 }),
    }
}

pub fn emit_tabs(app: &AppHandle, window_label: &str) {
    let _ = app.emit("tabs-changed", snapshot(app, window_label));
}

/// All strip-window labels (issue #47: repaint every window after a profile
/// color change).
pub fn window_labels(app: &AppHandle) -> Vec<String> {
    let state = app.state::<AppState>();
    let strip = state.strip.lock().unwrap();
    strip.keys().cloned().collect()
}

/// Window destroyed: drop its registry entries. NOTE: this does NOT delete the
/// tabs' data partitions — a Destroyed event also fires on quit, and wiping
/// then would erase the login/cookies we want to restore (issue #28). Partitions
/// are removed on explicit tab close (`close_tab`) and orphans are swept at the
/// next startup (`clear_partitions`), which keeps only session-referenced jars.
pub fn forget_window(app: &AppHandle, window_label: &str) {
    let state = app.state::<AppState>();
    let removed = state.strip.lock().unwrap().remove(window_label);
    if let Some(entry) = removed {
        {
            let mut titles = state.raw_titles.lock().unwrap();
            for tab in &entry.tabs {
                titles.remove(&tab.label);
            }
        }
        for tab in &entry.tabs {
            crate::session::forget_navigated(app, &tab.label);
            crate::session::forget_url(app, &tab.label);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::partition_suffix;

    #[test]
    fn parses_partition_suffix() {
        // restore advances tab_seq past these so new tabs can't reuse a live jar.
        assert_eq!(partition_suffix("tab-1-3"), Some(3));
        assert_eq!(partition_suffix("tab-2-17"), Some(17));
        assert_eq!(partition_suffix("tab-10-0"), Some(0));
        assert_eq!(partition_suffix("garbage"), None);
        assert_eq!(partition_suffix(""), None);
    }
}
