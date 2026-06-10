//! macOS-only AppKit shims (objc2). Three jobs the Swift app does natively
//! that Tauri's public API doesn't cover:
//!  1. Explicit `addTabbedWindow` for Cmd+T (AppKit auto-tab is flaky —
//!     Swift app lesson, docs/03 § Windows & tabs).
//!  2. Tab-bar-aware webview layout — port of updateWebViewLayout: with
//!     .fullSizeContentView the webview extends under the title/tab-bar zone,
//!     so when the tab bar appears its top must be pinned to
//!     contentLayoutRect.maxY or the tab bar clips the page (the "first tab
//!     is garbled" bug; visible as the cropped/overlapping header).
//!  3. Cmd+N's tabbingMode dance (disallowed at show, preferred after) so a
//!     new window stays standalone but can still Merge All Windows later.

#![cfg(target_os = "macos")]

use objc2::msg_send;
use objc2::runtime::AnyClass;
use objc2::ClassType;
use objc2_app_kit::{NSView, NSWindow, NSWindowOrderingMode, NSWindowTabbingMode};
use objc2_foundation::{CGPoint, CGRect, CGSize};
use tauri::WebviewWindow;

/// Attach `new_win` to `host`'s native tab group (Cmd+T behavior).
pub fn add_tabbed_window(host: &WebviewWindow, new_win: &WebviewWindow) {
    let host2 = host.clone();
    let new_win = new_win.clone();
    let result = host.run_on_main_thread(move || {
        let (Ok(host_ptr), Ok(new_ptr)) = (host2.ns_window(), new_win.ns_window()) else {
            return;
        };
        unsafe {
            let host_ns: &NSWindow = &*(host_ptr as *const NSWindow);
            let new_ns: &NSWindow = &*(new_ptr as *const NSWindow);
            host_ns.addTabbedWindow_ordered(new_ns, NSWindowOrderingMode::NSWindowAbove);
        }
    });
    if let Err(e) = result {
        log::warn!("macos: addTabbedWindow dispatch failed: {e}");
    }
}

/// Set NSWindow.tabbingMode. `disallowed=true` before showing a Cmd+N window
/// keeps it standalone; restore preferred afterwards (Swift fix).
pub fn set_tabbing_mode(window: &WebviewWindow, disallowed: bool) {
    let w = window.clone();
    let _ = window.run_on_main_thread(move || {
        if let Ok(ptr) = w.ns_window() {
            unsafe {
                let ns: &NSWindow = &*(ptr as *const NSWindow);
                ns.setTabbingMode(if disallowed {
                    NSWindowTabbingMode::Disallowed
                } else {
                    NSWindowTabbingMode::Preferred
                });
            }
        }
    });
}

/// MUST be called on the main thread. Recomputes the webview frame for the
/// tab-bar state and returns whether the tab bar is visible.
///
/// Port of BrowserWindowController.updateWebViewLayout:
///   topY = tabBarVisible ? contentLayoutRect.maxY : contentView.bounds.height
///   webView.frame = (0, 0, width, topY)
/// (No native status bar here — the ssh footer is DOM-injected — so the
/// bottom inset is always 0.)
pub fn update_webview_layout(window: &WebviewWindow) -> bool {
    let Ok(ptr) = window.ns_window() else {
        return false;
    };
    unsafe {
        let ns: &NSWindow = &*(ptr as *const NSWindow);
        let tab_visible = ns.tabGroup().map(|g| g.isTabBarVisible()).unwrap_or(false);
        let Some(content) = ns.contentView() else {
            return tab_visible;
        };
        let bounds = content.bounds();
        let layout = ns.contentLayoutRect();
        let top_y = if tab_visible {
            layout.origin.y + layout.size.height
        } else {
            bounds.size.height
        };
        if let Some(webview) = find_wk_webview(&content) {
            let current = webview.frame();
            let target = CGRect::new(
                CGPoint::new(0.0, 0.0),
                CGSize::new(bounds.size.width, top_y),
            );
            let differs = (current.size.height - target.size.height).abs() > 0.5
                || (current.size.width - target.size.width).abs() > 0.5
                || current.origin.y.abs() > 0.5;
            if differs {
                webview.setFrame(target);
            }
        }
        tab_visible
    }
}

/// Find the WKWebView in the content view hierarchy (wry adds it as a
/// subview; search two levels deep to be safe).
unsafe fn find_wk_webview(view: &NSView) -> Option<objc2::rc::Retained<NSView>> {
    let wk_class = AnyClass::get("WKWebView")?;
    let subviews = view.subviews();
    for sub in subviews.iter() {
        let is_wk: bool = msg_send![sub, isKindOfClass: wk_class];
        if is_wk {
            return Some(sub.retain());
        }
    }
    for sub in subviews.iter() {
        let inner = sub.subviews();
        for sub2 in inner.iter() {
            let is_wk: bool = msg_send![sub2, isKindOfClass: wk_class];
            if is_wk {
                return Some(sub2.retain());
            }
        }
    }
    None
}
