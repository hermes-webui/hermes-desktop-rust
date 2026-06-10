//! Native paste pipeline — port of BrowserWindowController.handlePaste.
//! Image → PNG → base64 → the verbatim 3-strategy injection (synthetic paste
//! event, document-level paste, synthetic drag/drop). Text → insertText.

use base64::Engine;
use tauri::{AppHandle, Manager};

/// The 3-strategy JS, verbatim from the Swift app (only the base64 templated).
/// Base64 alphabet has no JS-special characters, so direct splice is safe.
const THREE_STRATEGY_JS: &str = r##"
(function() {
    const base64 = '__B64__';
    const binary = atob(base64);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
    const blob = new Blob([bytes], { type: 'image/png' });
    const file = new File([blob], 'screenshot.png', { type: 'image/png', lastModified: Date.now() });

    // Strategy 1: fire paste event on active element with clipboardData
    const active = document.activeElement || document.body;
    const dt = new DataTransfer();
    dt.items.add(file);

    const pasteEvent = new Event('paste', { bubbles: true, cancelable: true });
    Object.defineProperty(pasteEvent, 'clipboardData', { value: dt, writable: false });
    active.dispatchEvent(pasteEvent);

    // Strategy 2: also try on document
    document.dispatchEvent(new Event('paste', { bubbles: true }));

    // Strategy 3: simulate drop on active element
    const dropDt = new DataTransfer();
    dropDt.items.add(file);
    const rect = active.getBoundingClientRect();
    const cx = rect.left + rect.width / 2;
    const cy = rect.top + rect.height / 2;
    ['dragenter','dragover','drop'].forEach(type => {
        const ev = new DragEvent(type, {
            bubbles: true, cancelable: true, clientX: cx, clientY: cy, dataTransfer: dropDt
        });
        active.dispatchEvent(ev);
    });
    return 'ok';
})();
"##;

/// Paste into the focused window (content or shell — text path works in both).
pub fn paste_into_focused(app: &AppHandle) {
    let target = app
        .webview_windows()
        .values()
        .find(|w| w.is_focused().unwrap_or(false))
        .cloned()
        .or_else(|| crate::windows::focused_or_recent_content(app));
    let Some(win) = target else {
        return;
    };

    let Ok(mut clipboard) = arboard::Clipboard::new() else {
        return;
    };

    if let Ok(img) = clipboard.get_image() {
        let Some(b64) = encode_png_base64(&img) else {
            return;
        };
        let js = THREE_STRATEGY_JS.replace("__B64__", &b64);
        let _ = win.eval(&js);
        log::info!("paste: injected image ({}x{})", img.width, img.height);
        return;
    }

    if let Ok(text) = clipboard.get_text() {
        if let Ok(json) = serde_json::to_string(&text) {
            let _ = win.eval(format!(
                "document.execCommand('insertText', false, {json});"
            ));
        }
        return;
    }

    let _ = win.eval("document.execCommand('paste');");
}

fn encode_png_base64(img: &arboard::ImageData) -> Option<String> {
    let mut png_bytes: Vec<u8> = Vec::new();
    {
        use image::ImageEncoder;
        let encoder = image::codecs::png::PngEncoder::new(&mut png_bytes);
        encoder
            .write_image(
                img.bytes.as_ref(),
                img.width as u32,
                img.height as u32,
                image::ExtendedColorType::Rgba8,
            )
            .ok()?;
    }
    Some(base64::engine::general_purpose::STANDARD.encode(&png_bytes))
}
