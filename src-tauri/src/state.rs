use std::collections::HashMap;
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
}

/// Per-window tab state for strip mode.
#[derive(Debug, Clone, Default)]
pub struct WindowTabs {
    pub tabs: Vec<TabEntry>,
    pub active: usize,
    pub tab_seq: u64,
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
            tunnel_child: Mutex::new(None),
            tunnel_status: Mutex::new(TunnelStatus::Disconnected),
            stderr_tail: Mutex::new(Vec::new()),
            last_error_hint: Mutex::new(String::new()),
        }
    }
}
