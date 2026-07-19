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
    /// This tab's session is actively streaming a response (issue #46). Reported
    /// by the page's busy reporter watching the WebUI's `S.busy` flag — the same
    /// state that drives the web app's in-progress spinner — so the strip can
    /// show a per-tab "working" glyph and you can see which background tabs have
    /// a run in flight. Transient: re-derived each launch, never persisted.
    pub busy: bool,
    /// The active WebUI profile for this tab — the value of its per-tab
    /// `hermes_profile` cookie (None = the default profile). Read from the
    /// tab's isolated cookie jar after each real page load. Drives the strip's
    /// per-tab profile dot (issue #8) and is persisted so a restored tab
    /// reopens on the same profile (issue #18).
    pub profile: Option<String>,
    /// The active profile NAME reported by the page (`/api/profile/active`),
    /// used to render the dot (issue #31). The WebUI sets the `hermes_profile`
    /// cookie only on an explicit switch (never on boot), so `profile` above is
    /// empty for a tab sitting on its starting profile → no dot. The page knows
    /// the name regardless, so it reports it; the frontend prefers this for the
    /// dot. Display-only (the canonical name, consistent across auth/no-auth) —
    /// `profile` (the cookie value) remains the source for restore re-seeding.
    /// Transient: re-derived by the reporter on every launch, not persisted.
    pub dot_profile: Option<String>,
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
    /// label -> the page's live `location.href`, reported by the injected route
    /// reporter. This is the authoritative per-tab URL for session restore
    /// (issue #30): wry's `url()` doesn't reliably reflect client-side
    /// `pushState` routing (notably WebView2), so the page reports it directly
    /// and capture prefers this over `url()`.
    pub tab_urls: Mutex<HashMap<String, String>>,
    /// label -> (tab_bar_visible, fullscreen) — last chrome state pushed into
    /// the page (hermes-mac-tabbed class, --traffic-light-width). macOS only.
    pub ui_state: Mutex<HashMap<String, (bool, bool)>>,
    /// Strip-mode (Windows/Linux) tab registry: window label -> tabs.
    pub strip: Mutex<HashMap<String, WindowTabs>>,
    /// macOS only: content-window label -> active `hermes_profile` value (the
    /// strip stores this per-TabEntry instead). Absent = default profile.
    /// Feeds session capture so a restored native tab reopens on its profile.
    pub window_profiles: Mutex<HashMap<String, String>>,
    /// macOS only: content-window label -> active profile NAME (non-default),
    /// reported by the page (`/api/profile/active`). Used to prefix the native
    /// tab title with the profile so macOS gets a per-tab profile indicator
    /// (issue #44) — the dot is Win/Linux-strip-only. Absent = default = no
    /// prefix. Display-only; the cookie-value `window_profiles` above remains
    /// the source for restore re-seeding.
    pub window_profile_names: Mutex<HashMap<String, String>>,
    /// macOS only: content-window label -> (busy, attention) for the native
    /// tab-title adornment (issues #64/#65) — "●" when the session waits on
    /// you, "⟳" while it works. Win/Linux keep these per-TabEntry (the strip
    /// dot and spinner) instead. busy arrives via BUSY_REPORTER bridge events
    /// (now injected on macOS too); attention via the leading ● marker on the
    /// reported document.title, which the mac branch previously discarded.
    pub window_indicators: Mutex<HashMap<String, (bool, bool)>>,
    /// macOS only: set once per launch after the first connect to fire the
    /// one-time "tabs exist" discoverability hint at most once per run (issue
    /// #42); the persisted `tabsHintShown` pref is the across-launch gate.
    pub tabs_hinted: AtomicBool,
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
    /// True while a NATIVE menu modal loop is up (the strip's "⋯" popup —
    /// `Menu::popup`). On Windows that popup runs a nested `TrackPopupMenu`
    /// message loop ON THE MAIN THREAD; any background work that marshals back
    /// into a webview (cookie/URL getters via the runtime dispatcher) would
    /// re-enter the main thread from inside that modal loop, which WebView2
    /// forbids → the UI thread deadlocks (#33: AppHangB1 ~2-3s after opening the
    /// menu, window stuck topmost, Preferences/Quit dead). Moving `popup` onto
    /// the event loop (v0.6.0 / #34) did NOT fix this — the modal loop still owns
    /// the main thread while it's up. The periodic autosave + profile-dot sweep
    /// checks this flag and skips its webview marshals while a menu is open,
    /// catching up on the next tick once it closes.
    pub menu_open: AtomicBool,
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
            tab_urls: Mutex::new(HashMap::new()),
            ui_state: Mutex::new(HashMap::new()),
            strip: Mutex::new(HashMap::new()),
            window_profiles: Mutex::new(HashMap::new()),
            window_profile_names: Mutex::new(HashMap::new()),
            window_indicators: Mutex::new(HashMap::new()),
            tabs_hinted: AtomicBool::new(false),
            navigated: Mutex::new(HashSet::new()),
            session_restored: AtomicBool::new(false),
            restoring: AtomicBool::new(false),
            persist_busy: AtomicBool::new(false),
            last_session: Mutex::new(String::new()),
            tunnel_child: Mutex::new(None),
            tunnel_status: Mutex::new(TunnelStatus::Disconnected),
            stderr_tail: Mutex::new(Vec::new()),
            last_error_hint: Mutex::new(String::new()),
            menu_open: AtomicBool::new(false),
        }
    }
}
