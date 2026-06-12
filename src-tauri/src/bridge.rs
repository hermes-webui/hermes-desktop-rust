//! Injected-script assembly + the bridge event handler.
//! Each script is a port of a WKUserScript from the Swift app
//! (docs/05-webview-bridge-spec.md). Pages report back by emitting the
//! `bridge` event ({label, kind, value}) — the only IPC surface granted to
//! remote content (capabilities/content.json).

use crate::state::AppState;
use crate::{conn, prefs, theme, windows};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Listener, Manager};
use tauri_plugin_notification::NotificationExt;

/// Shared emit helper prefix; every script below may use EMIT(kind, value).
const HELPER: &str = r##"
  const EMIT = function (kind, value) {
    try {
      if (window.__TAURI__ && window.__TAURI__.event) {
        window.__TAURI__.event.emit('bridge', { label: '__LABEL__', kind: kind, value: value });
      }
    } catch (e) {}
  };
"##;

/// S6 — pre-paint background (cached theme color, anti-flash).
const PRE_PAINT: &str = r##"
  try {
    document.documentElement.style.background = '__HEX__';
    var __ppb = function(){ if (document.body) document.body.style.background = ''; };
    // body background left to the page once it loads; documentElement holds the backstop.
  } catch (e) {}
"##;

/// S2 — web Notification stub (native notifications replace web ones).
const NOTIFICATION_STUB: &str = r##"
  try {
    if (window.Notification) {
      Notification.requestPermission = function (cb) {
        if (cb) cb('denied');
        return Promise.resolve('denied');
      };
    }
  } catch (e) {}
"##;

/// S3 — Web Speech suppression → webui falls back to MediaRecorder + /api/transcribe.
const SPEECH_SUPPRESS: &str = r##"
  try {
    window.SpeechRecognition = undefined;
    window.webkitSpeechRecognition = undefined;
  } catch (e) {}
"##;

/// S1 — paste suppression (mac/linux): the native Cmd+V path owns paste.
const PASTE_SUPPRESS: &str = r##"
  try {
    document.addEventListener('paste', function (e) { e.stopImmediatePropagation(); }, true);
  } catch (e) {}
"##;

/// S7/S8/S9 — macOS overlay-titlebar integration: traffic-light clearance,
/// hide the page logo (collides with traffic lights), CSS rule for hiding the
/// page titlebar when the native tab bar shows, drag region on .app-titlebar.
const MACOS_TITLEBAR: &str = r##"
  try { document.documentElement.style.setProperty('--traffic-light-width', '80px'); } catch (e) {}
  (function () {
    try {
      var s = document.createElement('style');
      s.textContent = '.app-titlebar-icon { visibility: hidden !important; } body.hermes-mac-tabbed .app-titlebar { display: none !important; }';
      (document.head || document.documentElement).appendChild(s);
    } catch (e) {}
  })();
  (function () {
    var attach = function () {
      try {
        var tb = document.querySelector('.app-titlebar');
        if (tb && !tb.hasAttribute('data-tauri-drag-region')) tb.setAttribute('data-tauri-drag-region', '');
      } catch (e) {}
    };
    if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', attach);
    else attach();
    setInterval(attach, 3000);
  })();
"##;

/// S5 — theme bridge. Byte-for-byte port of the Swift script's algorithm:
/// meta-tag first, pixel-sampling fallback, match-suppression + 2.5s
/// stability gate. Reports via EMIT('theme', cssColor).
const THEME_BRIDGE: &str = r##"
  (function () {
    const cachedHex = '__HEX__'.toUpperCase();
    let lastReportedHex = null;
    const isOpaque = (c) => c && c !== 'transparent' && c !== 'rgba(0, 0, 0, 0)';
    function rgbStringToHex(s) {
      const m = s.match(/^rgba?\((\d+)\D+(\d+)\D+(\d+)/);
      if (!m) return s.toUpperCase();
      return '#' + [m[1], m[2], m[3]].map(function (n) {
        return parseInt(n, 10).toString(16).padStart(2, '0').toUpperCase();
      }).join('');
    }
    function effectiveBackgroundAt(x, y) {
      if (!document.elementsFromPoint) return null;
      const els = document.elementsFromPoint(x, y);
      for (const el of els) {
        const bg = getComputedStyle(el).backgroundColor;
        if (isOpaque(bg)) return bg;
      }
      return null;
    }
    function themeColorMetaBackground() {
      const meta = document.getElementById('hermes-theme-color');
      if (!meta) return null;
      const content = (meta.getAttribute('content') || '').trim();
      if (!content) return null;
      if (!/^#[0-9a-fA-F]{3}([0-9a-fA-F]{3})?$|^rgba?\(/.test(content)) return null;
      return content;
    }
    function effectiveBackground() {
      const meta = themeColorMetaBackground();
      if (meta) return meta;
      const w = window.innerWidth || 1280;
      const h = window.innerHeight || 800;
      const points = [[w >> 1, h >> 1], [w >> 1, h >> 2], [w >> 2, h >> 1]];
      for (const [x, y] of points) {
        const bg = effectiveBackgroundAt(x, y);
        if (bg) return bg;
      }
      const bodyBg = document.body ? getComputedStyle(document.body).backgroundColor : null;
      if (isOpaque(bodyBg)) return bodyBg;
      return getComputedStyle(document.documentElement).backgroundColor;
    }
    const STABILITY_MS = 2500;
    let pendingHex = null;
    let pendingTimer = null;
    function report() {
      const bg = effectiveBackground();
      if (!bg) return;
      const hex = rgbStringToHex(bg);
      const currentChromeHex = lastReportedHex || cachedHex;
      if (hex === currentChromeHex) {
        pendingHex = null;
        clearTimeout(pendingTimer);
        return;
      }
      if (hex === pendingHex) return;
      pendingHex = hex;
      clearTimeout(pendingTimer);
      pendingTimer = setTimeout(function () {
        if (pendingHex === hex) {
          lastReportedHex = hex;
          EMIT('theme', bg);
        }
      }, STABILITY_MS);
    }
    const observer = new MutationObserver(() => requestAnimationFrame(report));
    function start() {
      report();
      observer.observe(document.documentElement, {
        attributes: true,
        attributeFilter: ['class', 'data-theme', 'style', 'data-mode']
      });
      if (document.body) {
        observer.observe(document.body, {
          attributes: true,
          attributeFilter: ['class', 'data-theme', 'style', 'data-mode']
        });
      }
      const themeMeta = document.getElementById('hermes-theme-color');
      if (themeMeta) {
        observer.observe(themeMeta, { attributes: true, attributeFilter: ['content'] });
      }
      setInterval(report, 2000);
    }
    if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', start);
    else start();
    window.addEventListener('focus', report);
    const mq = window.matchMedia('(prefers-color-scheme: dark)');
    if (mq.addEventListener) mq.addEventListener('change', report);
    else if (mq.addListener) mq.addListener(report);
  })();
"##;

/// S4 — response-ready notifier: characterData-only mutations, ≥20 chars,
/// 3s settle, fires only while hidden.
const NOTIFY_WATCHER: &str = r##"
  (function () {
    var startObs = function () {
      if (!document.body) return;
      let debounceTimer = null;
      let totalCharsAdded = 0;
      const MIN_CHARS = 20;
      const observer = new MutationObserver((mutations) => {
        let chars = 0;
        for (const m of mutations) {
          if (m.type === 'characterData') chars += (m.target.nodeValue || '').length;
        }
        if (chars === 0) return;
        totalCharsAdded += chars;
        clearTimeout(debounceTimer);
        debounceTimer = setTimeout(() => {
          if (document.hidden && totalCharsAdded >= MIN_CHARS) {
            EMIT('notify', { title: 'Hermes', body: 'Your response is ready' });
          }
          totalCharsAdded = 0;
        }, 3000);
      });
      observer.observe(document.body, { subtree: true, characterData: true });
    };
    if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', startObs);
    else startObs();
  })();
"##;

/// S11 — window.open / target=_blank: same-origin navigates, external opens
/// in the system browser (parity-plus; the Swift app drops these silently).
///
/// Both cases route through a top-frame navigation rather than the IPC
/// `open-external` emit. The native `on_navigation` hook
/// (`windows::navigation_allowed`) lets same-origin/localhost through and opens
/// external hosts in the system browser, cancelling the navigation so the page
/// stays put. We deliberately avoid `EMIT('open-external', …)` here: that posts
/// to `ipc.localhost`, which the remote page's own CSP `connect-src` blocks
/// (the WebUI server, not the shell, governs CSP because `app.security.csp` is
/// null) — so the emit silently fails and the click is a no-op (issue #12,
/// hermes-webui#4040). `on_navigation` is a native wry hook, not subject to CSP.
const WINDOW_OPEN: &str = r##"
  (function () {
    window.open = function (u) {
      if (!u) return null;
      location.href = String(u);
      return null;
    };
    document.addEventListener('click', function (e) {
      var a = e.target && e.target.closest ? e.target.closest('a[target="_blank"]') : null;
      if (a && a.href) {
        e.preventDefault();
        e.stopPropagation();
        location.href = a.href;
      }
    }, true);
  })();
"##;

/// Title watcher — feeds the native tab/window title pipeline.
const TITLE_WATCHER: &str = r##"
  (function () {
    let last = null;
    const report = function () {
      const t = document.title || '';
      if (t !== last) { last = t; EMIT('title', t); }
    };
    const start = function () {
      report();
      const el = document.querySelector('title');
      if (el) new MutationObserver(report).observe(el, { childList: true, characterData: true, subtree: true });
      setInterval(report, 2000);
    };
    if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', start);
    else start();
  })();
"##;

/// SSH footer — 28px status bar pinned to the bottom, tinted with the exact
/// page background (Swift parity: the footer is the only chrome painted with
/// the page RGB). Injected only in ssh-mode windows. The native side drives
/// it via window.__hermesSetTunnelStatus(...).
const SSH_FOOTER: &str = r##"
  (function () {
    var build = function () {
      if (document.getElementById('hermes-ssh-footer')) return;
      try {
        var s = document.createElement('style');
        s.textContent =
          'body { height: calc(100vh - 28px) !important; min-height: calc(100vh - 28px) !important; max-height: calc(100vh - 28px) !important; }' +
          '#hermes-ssh-footer { position: fixed; left: 0; right: 0; bottom: 0; height: 28px; display: flex; align-items: center; gap: 8px; padding: 0 12px; font: 11px -apple-system, system-ui, sans-serif; color: rgba(128,128,128,0.95); border-top: 1px solid rgba(128,128,128,0.25); background: __HEX__; z-index: 2147483647; box-sizing: border-box; }' +
          '#hermes-ssh-dot { width: 10px; height: 10px; border-radius: 5px; background: #8e8e93; flex: none; }' +
          '#hermes-ssh-reconnect { display: none; margin-left: auto; font-size: 11px; padding: 2px 10px; border-radius: 5px; border: 1px solid rgba(128,128,128,0.4); background: transparent; color: inherit; cursor: pointer; }';
        (document.head || document.documentElement).appendChild(s);
        var bar = document.createElement('div');
        bar.id = 'hermes-ssh-footer';
        bar.innerHTML = '<div id="hermes-ssh-dot"></div><span id="hermes-ssh-label">Connecting…</span><button id="hermes-ssh-reconnect">Reconnect</button>';
        document.body.appendChild(bar);
        document.getElementById('hermes-ssh-reconnect').addEventListener('click', function () { EMIT('reconnect', ''); });
        if (window.__hermesTunnelInit) window.__hermesSetTunnelStatus.apply(null, window.__hermesTunnelInit);
      } catch (e) {}
    };
    if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', build);
    else build();
  })();
  window.__hermesSetTunnelStatus = function (state, host, port) {
    try {
      window.__hermesTunnelInit = [state, host, port];
      var dot = document.getElementById('hermes-ssh-dot');
      var label = document.getElementById('hermes-ssh-label');
      var btn = document.getElementById('hermes-ssh-reconnect');
      if (!dot || !label || !btn) return;
      if (state === 'connected') {
        dot.style.background = '#34c759';
        label.textContent = 'Tunnel connected · ' + host + ' · port ' + port;
        btn.style.display = 'none';
      } else if (state === 'connecting') {
        dot.style.background = '#8e8e93';
        label.textContent = 'Connecting…';
        btn.style.display = 'none';
      } else {
        dot.style.background = '#ff3b30';
        label.textContent = 'Tunnel disconnected · click Reconnect to retry';
        btn.style.display = 'inline-block';
      }
    } catch (e) {}
  };
"##;

/// In-page find bar (Cmd/Ctrl+F) — the Tauri home of the Swift app's native
/// NSSearchField bar; search still runs through window.find(...) with the
/// same argument set. Exposes __hermesFindToggle/__hermesFindNext for the
/// macOS menu items; handles its own keys on Windows/Linux (no menu bar).
const FIND_BAR: &str = r##"
  (function () {
    var bar = null, input = null, visible = false;
    function build() {
      if (bar) return;
      var s = document.createElement('style');
      s.textContent =
        '#hermes-find{position:fixed;top:46px;right:18px;z-index:2147483647;display:none;align-items:center;gap:6px;padding:6px 8px;border-radius:8px;background:rgba(40,40,42,0.94);box-shadow:0 4px 18px rgba(0,0,0,0.35);font:12px -apple-system,system-ui,sans-serif;color:#eee;}' +
        '#hermes-find input{width:200px;font:12px -apple-system,system-ui,sans-serif;padding:3px 7px;border-radius:5px;border:1px solid rgba(255,255,255,0.2);background:rgba(0,0,0,0.3);color:#fff;outline:none;}' +
        '#hermes-find button{font:12px -apple-system,system-ui,sans-serif;padding:2px 8px;border-radius:5px;border:1px solid rgba(255,255,255,0.2);background:transparent;color:#eee;cursor:pointer;}';
      (document.head || document.documentElement).appendChild(s);
      bar = document.createElement('div');
      bar.id = 'hermes-find';
      bar.innerHTML = '<input placeholder="Find in page…"><button data-d="p">‹</button><button data-d="n">›</button><button data-d="x">Done</button>';
      document.body.appendChild(bar);
      input = bar.querySelector('input');
      input.addEventListener('keydown', function (e) {
        if (e.key === 'Enter') { e.preventDefault(); find(!e.shiftKey); }
        else if (e.key === 'Escape') { hide(); }
        e.stopPropagation();
      });
      bar.querySelector('[data-d="p"]').addEventListener('click', function () { find(false); });
      bar.querySelector('[data-d="n"]').addEventListener('click', function () { find(true); });
      bar.querySelector('[data-d="x"]').addEventListener('click', hide);
    }
    function find(forward) {
      var q = input && input.value;
      if (!q) return;
      try { window.find(q, false, !forward, true, false, true, false); } catch (e) {}
    }
    function show() { build(); bar.style.display = 'flex'; visible = true; input.focus(); input.select(); }
    function hide() { if (bar) bar.style.display = 'none'; visible = false; try { input && input.blur(); } catch (e) {} }
    function toggle() { if (visible) hide(); else show(); }
    window.__hermesFindToggle = toggle;
    window.__hermesFindNext = function (forward) { if (!visible) { show(); return; } find(forward); };
    document.addEventListener('keydown', function (e) {
      var mod = e.metaKey || e.ctrlKey;
      if (mod && !e.shiftKey && !e.altKey && (e.key === 'f' || e.key === 'F')) {
        e.preventDefault(); e.stopPropagation(); toggle();
      } else if (mod && !e.altKey && (e.key === 'g' || e.key === 'G')) {
        e.preventDefault(); e.stopPropagation(); window.__hermesFindNext(!e.shiftKey);
      } else if (e.key === 'Escape' && visible) {
        hide();
      }
    }, true);
  })();
"##;

/// S12 — app-shortcut forwarder (Windows/Linux only; no menu-bar
/// accelerators there). Forwards EXACTLY this set, never touches anything
/// else (the webui has its own shortcuts like Ctrl+K).
const SHORTCUT_FORWARDER: &str = r##"
  document.addEventListener('keydown', function (e) {
    if (!e.ctrlKey || e.altKey || e.metaKey) return;
    var k = (e.key || '').toLowerCase();
    var send = function (v) { e.preventDefault(); e.stopPropagation(); EMIT('shortcut', v); };
    if (k === 'tab') { send(e.shiftKey ? 'prev-tab' : 'next-tab'); return; }
    if (e.shiftKey) return;
    if (k === 't') send('new-tab');
    else if (k === 'n') send('new-window');
    else if (k === 'w') send('close');
    else if (k === 'r') { e.preventDefault(); e.stopPropagation(); location.reload(); }
    else if (k === ',') send('prefs');
    else if (k === '=' || k === '+') send('zoom-in');
    else if (k === '-') send('zoom-out');
    else if (k === '0') send('zoom-reset');
  }, true);
"##;

/// S13 — download bridge (mac/linux only): WKWebView and WebKitGTK do nothing
/// for SPA-style downloads (a[download] / blob: URLs — how hermes-webui
/// exports sessions). Intercept the click, read the blob in-page, and hand
/// the bytes to native, which saves into ~/Downloads and notifies. Windows
/// is NOT injected — WebView2's native Save As dialog is better UX.
const DOWNLOAD_BRIDGE: &str = r##"
  document.addEventListener('click', function (e) {
    var a = e.target && e.target.closest ? e.target.closest('a[download], a[href^="blob:"]') : null;
    if (!a) return;
    var href = a.getAttribute('href') || '';
    if (!href || href === '#') return;
    e.preventDefault();
    e.stopPropagation();
    var name = a.getAttribute('download');
    if (!name) {
      try { name = new URL(href, location.href).pathname.split('/').pop(); } catch (err) {}
    }
    name = name || 'download';
    fetch(href)
      .then(function (r) { return r.blob(); })
      .then(function (b) {
        if (b.size > 30 * 1024 * 1024) {
          EMIT('download-too-big', { name: name, size: b.size });
          return;
        }
        var fr = new FileReader();
        fr.onload = function () {
          var data = String(fr.result);
          EMIT('download', { name: name, data: data.slice(data.indexOf(',') + 1) });
        };
        fr.readAsDataURL(b);
      })
      .catch(function () {});
  }, true);
"##;

/// Assemble the per-window initialization script.
pub fn init_script(label: &str, pre_paint_hex: &str, is_ssh: bool) -> String {
    let mut parts: Vec<&str> = vec![HELPER, PRE_PAINT, NOTIFICATION_STUB, SPEECH_SUPPRESS];
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        parts.push(PASTE_SUPPRESS);
    }
    if cfg!(target_os = "macos") {
        parts.push(MACOS_TITLEBAR);
    }
    if cfg!(not(target_os = "macos")) {
        parts.push(SHORTCUT_FORWARDER);
    }
    parts.push(THEME_BRIDGE);
    parts.push(NOTIFY_WATCHER);
    parts.push(WINDOW_OPEN);
    parts.push(TITLE_WATCHER);
    parts.push(FIND_BAR);
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        parts.push(DOWNLOAD_BRIDGE);
    }
    if is_ssh {
        parts.push(SSH_FOOTER);
    }
    let body = parts.concat();
    format!("(function () {{\n{body}\n}})();")
        .replace("__LABEL__", label)
        .replace("__HEX__", pre_paint_hex)
}

/// S10 — cross-tab theme sync, evaluated on demand in every *other* window.
pub const THEME_SYNC_EVAL: &str = r##"
  (function () {
    try {
      if (typeof _applyTheme === 'function') _applyTheme(localStorage.getItem('hermes-theme') || 'dark');
      if (typeof _applySkin === 'function') _applySkin(localStorage.getItem('hermes-skin') || 'default');
      if (typeof _syncThemePicker === 'function') _syncThemePicker(localStorage.getItem('hermes-theme') || 'dark');
      if (typeof _syncSkinPicker === 'function') _syncSkinPicker(localStorage.getItem('hermes-skin') || 'default');
    } catch (e) {}
  })();
"##;

/// Wire the bridge listener — pages → native.
pub fn install(app: &AppHandle) {
    let handle = app.clone();
    app.listen_any("bridge", move |event| {
        let Ok(payload) = serde_json::from_str::<Value>(event.payload()) else {
            return;
        };
        let label = payload["label"].as_str().unwrap_or("").to_string();
        let kind = payload["kind"].as_str().unwrap_or("");
        match kind {
            "title" => {
                let raw = payload["value"].as_str().unwrap_or("").to_string();
                if label.starts_with("tab-") {
                    crate::strip::set_tab_title(&handle, &label, &raw);
                } else {
                    let state = handle.state::<AppState>();
                    state.raw_titles.lock().unwrap().insert(label.clone(), raw);
                    windows::refresh_title(&handle, &label);
                }
            }
            "theme" => {
                if let Some(css) = payload["value"].as_str() {
                    handle_theme_report(&handle, css);
                }
            }
            "notify" => {
                if prefs::load(&handle).notifications_enabled {
                    let title = payload["value"]["title"].as_str().unwrap_or("Hermes");
                    let body = payload["value"]["body"]
                        .as_str()
                        .unwrap_or("Your response is ready");
                    let _ = handle
                        .notification()
                        .builder()
                        .title(title)
                        .body(body)
                        .show();
                }
            }
            "open-external" => {
                if let Some(u) = payload["value"].as_str() {
                    if u.starts_with("http://") || u.starts_with("https://") {
                        let _ = tauri_plugin_opener::open_url(u, None::<&str>);
                    }
                }
            }
            "reconnect" => {
                conn::reconnect(&handle);
            }
            "download" => {
                use base64::Engine;
                let name = payload["value"]["name"].as_str().unwrap_or("download");
                let data = payload["value"]["data"].as_str().unwrap_or("");
                match base64::engine::general_purpose::STANDARD.decode(data) {
                    Ok(bytes) => save_download(&handle, name, &bytes),
                    Err(e) => log::warn!("download: bad payload for {name}: {e}"),
                }
            }
            "download-too-big" => {
                let name = payload["value"]["name"].as_str().unwrap_or("file");
                let _ = handle
                    .notification()
                    .builder()
                    .title("Download too large")
                    .body(format!(
                        "{name} is too large to save from the app — open Hermes in your browser for this one."
                    ))
                    .show();
            }
            "shortcut" => {
                if let Some(v) = payload["value"].as_str() {
                    match v {
                        "new-tab" => {
                            if crate::strip::enabled() && label.starts_with("tab-") {
                                // Add a tab to the emitting webview's window.
                                let app = handle.clone();
                                let tab = label.clone();
                                std::thread::spawn(move || {
                                    if let Some(win) = crate::strip::window_of_tab(&tab) {
                                        crate::strip::add_tab(&app, &win);
                                    }
                                });
                            } else {
                                conn::open_new_session(&handle, true);
                            }
                        }
                        "new-window" => conn::open_new_session(&handle, false),
                        "close" => {
                            if crate::strip::enabled() && label.starts_with("tab-") {
                                let app = handle.clone();
                                let tab = label.clone();
                                std::thread::spawn(move || {
                                    crate::strip::close_tab_by_label(&app, &tab);
                                });
                            } else if let Some(w) = handle.get_webview_window(&label) {
                                let _ = w.close();
                            }
                        }
                        "next-tab" => crate::strip::cycle_tab(&handle, &label, true),
                        "prev-tab" => crate::strip::cycle_tab(&handle, &label, false),
                        "prefs" => windows::open_prefs(&handle),
                        "zoom-in" => crate::menu::zoom_step(&handle, 0.1),
                        "zoom-out" => crate::menu::zoom_step(&handle, -0.1),
                        "zoom-reset" => crate::menu::zoom_reset(&handle),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    });
}

/// Save intercepted download bytes into ~/Downloads with collision-safe
/// naming, then notify (mac/linux — see DOWNLOAD_BRIDGE).
fn save_download(app: &AppHandle, name: &str, bytes: &[u8]) {
    let safe: String = name
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let safe = safe.trim().trim_start_matches('.');
    let safe = if safe.is_empty() { "download" } else { safe };
    let Ok(dir) = app.path().download_dir() else {
        log::warn!("download: no download dir");
        return;
    };
    let mut target = dir.join(safe);
    let mut counter = 1;
    while target.exists() {
        let stem = std::path::Path::new(safe)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("download");
        let ext = std::path::Path::new(safe)
            .extension()
            .and_then(|s| s.to_str())
            .map(|e| format!(".{e}"))
            .unwrap_or_default();
        target = dir.join(format!("{stem} ({counter}){ext}"));
        counter += 1;
    }
    match std::fs::write(&target, bytes) {
        Ok(()) => {
            log::info!(
                "download: saved {} ({} bytes)",
                target.display(),
                bytes.len()
            );
            let shown = target
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(safe)
                .to_string();
            let _ = app
                .notification()
                .builder()
                .title("Download complete")
                .body(format!("Saved {shown} to Downloads"))
                .show();
        }
        Err(e) => log::error!("download: write failed: {e}"),
    }
}

/// hermesTheme handler port: parse → luminance → appearance fan-out + persist.
fn handle_theme_report(app: &AppHandle, css: &str) {
    let Some((r, g, b)) = theme::parse_css_color(css) else {
        return;
    };
    let dark = theme::is_dark(r, g, b);
    let hex = theme::hex_string(r, g, b);
    log::info!("theme: page reported {css} -> {hex} (dark={dark})");
    app.set_theme(Some(if dark {
        tauri::Theme::Dark
    } else {
        tauri::Theme::Light
    }));
    prefs::theme_cache_save(app, r, g, b);
    // Tint the ssh footer with the exact page RGB in every content webview and
    // re-apply theme/skin from shared localStorage in the others (S10).
    windows::eval_all_content(
        app,
        &format!(
            "if (document.getElementById('hermes-ssh-footer')) document.getElementById('hermes-ssh-footer').style.background = '{hex}';"
        ),
    );
    windows::eval_all_content(app, THEME_SYNC_EVAL);
    let _ = app.emit(
        "theme-changed",
        serde_json::json!({ "hex": hex, "isDark": dark }),
    );
    let state = app.state::<AppState>();
    let _ = state; // theme cache persisted above; nothing else to track
}
