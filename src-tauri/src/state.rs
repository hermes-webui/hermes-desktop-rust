use std::collections::{HashMap, HashSet};
use std::process::Child;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Mutex;

/// Tunnel status — mirrors the Swift TunnelManager's enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TunnelStatus {
    Connecting,
    Connected,
    Disconnected,
}

/// One tab in a strip-mode window (Windows/Linux custom tab bar).
#[derive(Debug, Clone, serde::Serialize)]
pub struct TabEntry {
    pub label: String,
    pub title: String,
    /// The tab's session has a pending approval/clarify popup waiting on the
    /// user. Derived from the WebUI's leading "● " title marker (issue #14);
    /// the strip renders an attention badge so a blocked background tab is
    /// findable without clicking through every tab.
    pub attention: bool,
    /// The active WebUI profile for this tab — the value of its per-tab
    /// `hermes_profile` cookie (None = the default profile). Read from the
    /// tab's isolated cookie jar after each real page load. Drives the strip's
    /// per-tab profile dot (issue #8) and is persisted so a restored tab
    /// reopens on the same profile (issue #18).
    pub profile: Option<String>,
    /// The on-disk data-partition directory name backing this tab's cookie jar
    /// (Windows/Linux). Normally the tab label, but a tab restored from a saved
    /// session reuses its *previous* partition so login + cookies survive the
    /// restart (issue #28). Not sent to the frontend.
    #[serde(skip)]
    pub partition: String,
    /// A user-given tab name that overrides the page title in the strip
    /// (issue #7). None = follow the document title.
    pub custom_title: Option<String>,
}

/// Per-window tab state for strip mode.
#[derive(Debug, Clone, Default)]
pub struct WindowTabs {
    pub tabs: Vec<TabEntry>,
    pub active: usize,
    pub tab_seq: u64,
    /// The tab strip is hidden to reclaim its 38px for content (issue #10).
    /// Toggled via the strip menu / Ctrl+Shift+B; the content webview then
    /// fills the whole window. Windows-only (macOS = native tabs; Linux can't
    /// re-fit GTK child webviews — constraint #1).
    pub strip_hidden: bool,
}

pub struct AppState {
    /// Guards against concurrent orchestrator runs (Save & Reconnect spam).
    pub connecting: AtomicBool,
    /// Monotonic counter for content window labels (main-1, main-2, ...).
    pub window_seq: AtomicU64,
    /// Generation counters: bumping one invalidates the matching background loop.
    pub health_gen: AtomicU64,
    pub monitor_gen: AtomicU64,
    pub recovery_gen: AtomicU64,
    /// Direct-mode health state (drives title dot + dock badge).
    pub healthy: AtomicBool,
    /// label -> connection mode the window was built for ("direct" | "ssh").
    pub window_modes: Mutex<HashMap<String, String>>,
    /// label -> last raw document.title reported by the title-watcher script.
    pub raw_titles: Mutex<HashMap<String, String>>,
    /// label -> (tab_bar_visible, fullscreen) — last chrome state pushed into
    /// the page (hermes-mac-tabbed class, --traffic-light-width). macOS only.
    pub ui_state: Mutex<HashMap<String, (bool, bool)>>,
    /// Strip-mode (Windows/Linux) tab registry: window label -> tabs.
    pub strip: Mutex<HashMap<String, WindowTabs>>,
    /// macOS only: content-window label -> active `hermes_profile` value (the
    /// strip stores this per-TabEntry instead). Absent = default profile.
    /// Feeds session capture so a restored native tab reopens on its profile.
    pub window_profiles: Mutex<HashMap<String, String>>,
    /// Webview/window labels that have committed at least one real (non-`about:`)
    /// navigation. Session capture reads a webview's live URL ONLY for these:
    /// wry's macOS `url()` unwraps a nil `WKWebView.URL` on a not-yet-navigated
    /// webview and panics — and that panic poisons a tauri-runtime-wry mutex
    /// mid-dispatch, so a later `navigate()` aborts the process (caught unwinds
    /// don't undo the poison). Gating the call is the only safe fix.
    pub navigated: Mutex<HashSet<String>>,
    /// Set once the saved-session decision has been made on this launch —
    /// restore runs on the FIRST successful connect only, never on later
    /// same-process reconnects/mode-switches (issue #18).
    pub session_restored: AtomicBool,
    /// Held while restore is rebuilding windows so a mid-restore `persist`
    /// can't clobber the saved session with a half-built capture.
    pub restoring: AtomicBool,
    /// Single-flight guard for `session::persist` (skip overlapping captures;
    /// the periodic tick catches anything dropped).
    pub persist_busy: AtomicBool,
    /// Serialized form of the last-saved session — persist writes only when the
    /// captured session differs, so the periodic tick + event calls don't thrash
    /// the store.
    pub last_session: Mutex<String>,
    /// The ssh child process, if a tunnel is up.
    pub tunnel_child: Mutex<Option<Child>>,
    pub tunnel_status: Mutex<TunnelStatus>,
    /// Last few stderr lines from ssh — surfaced in the error window as a hint.
    pub stderr_tail: Mutex<Vec<String>>,
    /// Human-readable reason for the last connection failure.
    pub last_error_hint: Mutex<String>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            connecting: AtomicBool::new(false),
            window_seq: AtomicU64::new(0),
            health_gen: AtomicU64::new(0),
            monitor_gen: AtomicU64::new(0),
            recovery_gen: AtomicU64::new(0),
            healthy: AtomicBool::new(true),
            window_modes: Mutex::new(HashMap::new()),
            raw_titles: Mutex::new(HashMap::new()),
            ui_state: Mutex::new(HashMap::new()),
            strip: Mutex::new(HashMap::new()),
            window_profiles: Mutex::new(HashMap::new()),
            navigated: Mutex::new(HashSet::new()),
            session_restored: AtomicBool::new(false),
            restoring: AtomicBool::new(false),
            persist_busy: AtomicBool::new(false),
            last_session: Mutex::new(String::new()),
            tunnel_child: Mutex::new(None),
            tunnel_status: Mutex::new(TunnelStatus::Disconnected),
            stderr_tail: Mutex::new(Vec::new()),
            last_error_hint: Mutex::new(String::new()),
        }
    }
}
