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

/// S2 — Web Notifications shim → native OS notifications (issue #32). Tauri's
/// webviews don't implement the W3C Notification API (absent on WKWebView,
/// unsurfaced on WebView2/WebKitGTK), so the WebUI's `new Notification(...)`
/// calls — response complete, approval required, clarify needed — silently
/// no-op. This replaces `window.Notification` with a shim that reports through
/// the bridge to `tauri-plugin-notification`, so they fire natively with the
/// WebUI's real title/body. Permission is pre-granted so the WebUI takes its
/// notification path; the shell still gates on the app's notifications pref.
/// (Replaces the old stub, which forced permission to "denied".)
const NOTIFICATION_SHIM: &str = r##"
  (function () {
    try {
      function HermesNotification(title, opts) {
        opts = opts || {};
        try {
          EMIT('notify', {
            title: String(title == null ? 'Hermes' : title),
            body: String(opts.body || '')
          });
        } catch (e) {}
        this.title = title;
        this.body = opts.body || '';
        this.onclick = null;
        this.onclose = null;
        this.onerror = null;
        this.onshow = null;
      }
      HermesNotification.permission = 'granted';
      HermesNotification.requestPermission = function (cb) {
        if (typeof cb === 'function') cb('granted');
        return Promise.resolve('granted');
      };
      HermesNotification.prototype.close = function () {};
      HermesNotification.prototype.addEventListener = function () {};
      HermesNotification.prototype.removeEventListener = function () {};
      HermesNotification.prototype.dispatchEvent = function () { return false; };
      try {
        Object.defineProperty(window, 'Notification', {
          value: HermesNotification, writable: true, configurable: true
        });
      } catch (e) {
        window.Notification = HermesNotification;
      }
      // The WebUI prefers ServiceWorkerRegistration.showNotification when a SW
      // is active and only falls back to `new Notification` if that REJECTS.
      // In an embedded webview the SW path can resolve but display nothing,
      // swallowing the notification before it reaches the shim above. Route it
      // through the bridge too so SW-first notifications still fire natively.
      try {
        if (window.ServiceWorkerRegistration && ServiceWorkerRegistration.prototype) {
          ServiceWorkerRegistration.prototype.showNotification = function (title, opts) {
            opts = opts || {};
            try {
              EMIT('notify', {
                title: String(title == null ? 'Hermes' : title),
                body: String(opts.body || '')
              });
            } catch (e) {}
            return Promise.resolve();
          };
        }
      } catch (e) {}
    } catch (e) {}
  })();
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
/// page titlebar when the native tab bar shows, and the window drag region.
///
/// Window dragging uses Tauri's `data-tauri-drag-region` (Tauri's injected
/// `drag.js` listens for a mousedown in the region and invokes
/// `plugin:window|start_dragging`). NOTE: macOS WKWebView does NOT support
/// Chromium's `-webkit-app-region: drag` despite the prefix — that's an
/// Electron/Chromium feature, so an earlier attempt using it was a silent
/// no-op. The Tauri attribute was already present but couldn't move the window
/// because the remote-content capability granted only `core:event:default`, so
/// the `start_dragging` command was denied; granting
/// `core:window:allow-start-dragging` (capabilities/content.json) is what
/// actually fixes single-tab dragging (issue #22). With 2+ tabs the native tab
/// bar provided its own drag region, which is why the bug only showed with one.
///
/// `deep` makes the entire titlebar a drag region; `drag.js` automatically
/// exempts interactive descendants (button/a/input/select/[role]/contenteditable
/// /tabindex), so the titlebar's controls still receive their clicks.
const MACOS_TITLEBAR: &str = r##"
  try { document.documentElement.style.setProperty('--traffic-light-width', '80px'); } catch (e) {}
  (function () {
    try {
      var s = document.createElement('style');
      s.textContent =
        '.app-titlebar-icon { visibility: hidden !important; } ' +
        'body.hermes-mac-tabbed .app-titlebar { display: none !important; }';
      (document.head || document.documentElement).appendChild(s);
    } catch (e) {}
  })();
  (function () {
    var attach = function () {
      try {
        var tb = document.querySelector('.app-titlebar');
        if (tb && tb.getAttribute('data-tauri-drag-region') !== 'deep')
          tb.setAttribute('data-tauri-drag-region', 'deep');
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

// S4 — the old DOM-mutation "response is ready" heuristic was removed in favor
// of the Notification shim (issue #32): the WebUI now fires precise native
// notifications (response complete / approval / clarify) through window.
// Notification → EMIT('notify'), so the crude ≥20-chars-while-hidden watcher
// would only double-fire. (Removed; the shim is the single notification path.)

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
///
/// Only http(s) URLs are routed through the top-frame navigation:
/// `navigation_allowed` deliberately lets non-web schemes through (the engine
/// needs them internally), so navigating to `about:blank`/`blob:`/`javascript:`
/// here would REPLACE the app's top frame (a common `window.open` pattern in
/// libraries). Non-http(s) window.open calls are dropped (the old EMIT path's
/// effective behavior); non-http(s) `_blank` anchors keep their default action
/// so the download bridge / engine still handle them.
const WINDOW_OPEN: &str = r##"
  (function () {
    window.open = function (u) {
      if (!u) return null;
      try {
        var x = new URL(String(u), location.href);
        if (x.protocol === 'http:' || x.protocol === 'https:') location.href = x.href;
      } catch (e) {}
      return null;
    };
    document.addEventListener('click', function (e) {
      var a = e.target && e.target.closest ? e.target.closest('a[target="_blank"]') : null;
      if (!a || !a.href) return;
      if (a.protocol !== 'http:' && a.protocol !== 'https:') return;
      e.preventDefault();
      e.stopPropagation();
      location.href = a.href;
    }, true);
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
    if (k === 'b' && e.shiftKey) { send('toggle-bar'); return; }
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

/// S14 — route reporter (issue #30): report the page's live `location.href`
/// (including client-side `pushState`/hash routes) to the shell so session
/// restore can reopen the exact deep-linked session, not just the root. wry's
/// `url()` doesn't reliably reflect SPA history changes on every engine
/// (notably WebView2), so the page reports the URL itself, debounced to fire
/// only when it actually changes.
const ROUTE_REPORTER: &str = r##"
  (function () {
    var last = '';
    var report = function () {
      try {
        var h = location.href;
        if (h && h !== last) { last = h; EMIT('route', h); }
      } catch (e) {}
    };
    var wrap = function (name) {
      try {
        var orig = history[name];
        if (typeof orig === 'function') {
          history[name] = function () { var r = orig.apply(this, arguments); report(); return r; };
        }
      } catch (e) {}
    };
    wrap('pushState'); wrap('replaceState');
    window.addEventListener('popstate', report);
    window.addEventListener('hashchange', report);
    if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', report);
    else report();
    setInterval(report, 2000);
  })();
"##;

/// S15 — active-profile reporter (issue #31; extended for #8 in v0.6.3): report
/// the tab's active profile NAME so the strip can color the per-tab profile dot.
/// EVERY profile gets a dot, the DEFAULT profile INCLUDED — each name maps to
/// its own stable color (`profileColor`), so every tab on one profile shares a
/// color and three profiles show three colors. The WebUI sets the
/// `hermes_profile` cookie only on an explicit switch (never on boot), so the
/// cookie can't drive this; `/api/profile/active` always returns the real
/// active name (`{name, is_default}` — `name` is non-empty even for the
/// default / renamed-root profile, via `get_active_profile_name`). Reported on
/// load, on every client-side navigation (a profile switch routes, so the dot
/// recolors at once), and on a 3s / focus backstop — debounced to fire only on
/// change. `no-store` so a switch isn't masked by a cached response.
const PROFILE_REPORTER: &str = r##"
  (function () {
    var last = null;
    var report = function () {
      try {
        fetch('/api/profile/active', { credentials: 'same-origin', cache: 'no-store' })
          .then(function (r) { return r.ok ? r.json() : null; })
          .then(function (p) {
            if (!p) return;
            var name = p.name || '';
            if (name !== last) { last = name; EMIT('profile', name); }
          })
          .catch(function () {});
      } catch (e) {}
    };
    var wrap = function (n) {
      try {
        var orig = history[n];
        if (typeof orig === 'function') {
          history[n] = function () { var r = orig.apply(this, arguments); report(); return r; };
        }
      } catch (e) {}
    };
    wrap('pushState'); wrap('replaceState');
    window.addEventListener('popstate', report);
    window.addEventListener('hashchange', report);
    if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', report);
    else report();
    setInterval(report, 3000);
    window.addEventListener('focus', report);
  })();
"##;

/// macOS-only profile reporter (issue #44). macOS uses native tabs with no
/// strip, so there's nowhere to hang the Win/Linux color dot — surface the
/// active profile in the native tab TITLE instead. Mirrors `PROFILE_REPORTER`
/// but reports the name ONLY for a non-default profile (empty = default = clear
/// the prefix, so single-profile users' titles stay clean). A distinct
/// `mac-profile` kind keeps the Win/Linux dot's `profile` payload untouched.
const MACOS_PROFILE_REPORTER: &str = r##"
  (function () {
    var last = null;
    var report = function () {
      try {
        fetch('/api/profile/active', { credentials: 'same-origin', cache: 'no-store' })
          .then(function (r) { return r.ok ? r.json() : null; })
          .then(function (p) {
            if (!p) return;
            var name = p.is_default ? '' : (p.name || '');
            if (name !== last) { last = name; EMIT('mac-profile', name); }
          })
          .catch(function () {});
      } catch (e) {}
    };
    var wrap = function (n) {
      try {
        var orig = history[n];
        if (typeof orig === 'function') {
          history[n] = function () { var r = orig.apply(this, arguments); report(); return r; };
        }
      } catch (e) {}
    };
    wrap('pushState'); wrap('replaceState');
    window.addEventListener('popstate', report);
    window.addEventListener('hashchange', report);
    if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', report);
    else report();
    setInterval(report, 3000);
    window.addEventListener('focus', report);
  })();
"##;

/// Assemble the per-window initialization script.
pub fn init_script(label: &str, pre_paint_hex: &str, is_ssh: bool) -> String {
    let mut parts: Vec<&str> = vec![HELPER, PRE_PAINT, NOTIFICATION_SHIM, SPEECH_SUPPRESS];
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        parts.push(PASTE_SUPPRESS);
    }
    if cfg!(target_os = "macos") {
        parts.push(MACOS_TITLEBAR);
        // Surface the active profile in the native tab title (issue #44).
        parts.push(MACOS_PROFILE_REPORTER);
    }
    if cfg!(not(target_os = "macos")) {
        parts.push(SHORTCUT_FORWARDER);
        // Profile dot is strip-only (Windows/Linux); macOS native tabs have no
        // dot, so skip the reporter's polling there (issue #31).
        parts.push(PROFILE_REPORTER);
    }
    parts.push(THEME_BRIDGE);
    parts.push(ROUTE_REPORTER);
    parts.push(WINDOW_OPEN);
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
            // NOTE: titles no longer arrive via this bridge — they're sourced
            // natively through wry's on_document_title_changed hook (see
            // windows::apply_reported_title), which works even when the
            // remote-webview IPC that powers EMIT() is unavailable (issue #15).
            "theme" => {
                if let Some(css) = payload["value"].as_str() {
                    handle_theme_report(&handle, css);
                }
            }
            // Live URL incl. SPA routes (issue #30) — the authoritative per-tab
            // URL for session restore. Also a fast signal that the page
            // navigated (profile/session switch), so re-read the profile dot
            // now instead of waiting for the periodic sweep (issue #31).
            "route" => {
                if let Some(u) = payload["value"].as_str() {
                    crate::session::report_url(&handle, &label, u);
                }
                if crate::strip::enabled() && label.starts_with("tab-") {
                    let app = handle.clone();
                    let tab = label.clone();
                    std::thread::spawn(move || crate::strip::recapture_tab_profile(&app, &tab));
                }
            }
            // Active-profile NAME (issue #31) — drives the strip dot even on the
            // starting profile (where no hermes_profile cookie exists yet).
            "profile" => {
                if crate::strip::enabled() && label.starts_with("tab-") {
                    let name = payload["value"].as_str().unwrap_or("").to_string();
                    crate::strip::set_tab_dot_profile(&handle, &label, &name);
                }
            }
            // Active-profile NAME for the macOS native tab title (issue #44).
            // Non-default name → title prefix; empty → no prefix.
            "mac-profile" => {
                if cfg!(target_os = "macos") && label.starts_with("main-") {
                    let name = payload["value"].as_str().unwrap_or("");
                    windows::set_window_profile_name(&handle, &label, name);
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
                        "toggle-bar" => {
                            if crate::strip::enabled() && label.starts_with("tab-") {
                                let app = handle.clone();
                                let tab = label.clone();
                                std::thread::spawn(move || {
                                    if let Some(win) = crate::strip::window_of_tab(&tab) {
                                        crate::strip::toggle_strip(&app, &win);
                                    }
                                });
                            }
                        }
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
