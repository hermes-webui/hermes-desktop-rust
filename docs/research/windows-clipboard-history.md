# Windows Clipboard History research

## Conclusion

No existing PR or issue found in `nesquena/hermes-webui` proposes or merges a solution for the specific problem where copies made in the Windows Rust/Tauri/WebView2 desktop app do not appear in Windows Clipboard History (`Win+V`).

The relevant WebUI PRs only change browser-side copy behavior. They use or extend mechanisms such as `window._copyText`, `navigator.clipboard.writeText`, and the `document.execCommand("copy")` fallback, but do not route clipboard writes through Tauri/Rust or a native Windows clipboard library.

The fix belongs primarily in [`hermes-webui/hermes-desktop-rust`](https://github.com/hermes-webui/hermes-desktop-rust), not in the platform-neutral WebUI.

## Search scope

The GitHub repository/API search covered PRs and issues matching:

- `clipboard`
- `copy`
- `Windows clipboard history`
- `Win+V`
- `WebView2`
- `Tauri`
- `execCommand`
- `navigator.clipboard`

Issue and PR bodies, comments, reviews, and file patches were inspected rather than relying only on titles. The supplied repository remains accessible as [`nesquena/hermes-webui`](https://github.com/nesquena/hermes-webui), while the local desktop checkout uses the canonical remote [`hermes-webui/hermes-desktop-rust`](https://github.com/hermes-webui/hermes-desktop-rust).

## Relevant but unrelated WebUI PRs

### PR #6957, open

[`feat(copy icon): Add "Copy file contents" button to the workspace file preview panel`](https://github.com/nesquena/hermes-webui/pull/6957)

This PR adds a workspace-preview action that copies file contents. Its patch uses the existing WebUI copy helper and browser clipboard/fallback machinery. It does not mention or implement Windows Clipboard History, `Win+V`, WebView2 clipboard ownership, Tauri IPC, `clipboard-write`, or `arboard`.

**Status:** Open. **Relationship to this defect:** Copy-related, but not a solution.

### PR #6157, open

[`fix: sanitize mixed Markdown table copies`](https://github.com/nesquena/hermes-webui/pull/6157)

This PR changes the text produced when copying Markdown tables. It is content sanitization in the WebUI and does not introduce a native Windows clipboard write.

**Status:** Open. **Relationship to this defect:** Unrelated to Clipboard History recording.

No searched PR or issue supplied the missing WebView2-to-native Windows clipboard bridge.

## Comparison with the local proposed desktop fix

The local `src-tauri/src/bridge.rs` proposal:

1. **Intercepts `navigator.clipboard.writeText`** — the single choke point for all
   programmatic clipboard writes (`_copyText`, `_copyTextWithFallback`, and direct
   callers). This replaces the initial idea of wrapping `window._copyText` (Issue #2:
   `_copyText` is not an explicit `window` contract — it incidentally lands on `window`
   only because `static/ui.js` loads in script mode). By intercepting the Clipboard API
   instead, we catch every caller regardless of which helper function they use.
2. **Listens for `copy` events in bubble phase** — covers Ctrl+C, context-menu, and
   selection-based copies. Reads the **post-handler `clipboardData`** (after
   `_handleMarkdownTableCopy` has set sanitized `text/html` + `text/plain` flavors),
   preserving WebUI's Markdown table sanitization instead of clobbering it with raw
   `getSelection()` (Issue #1).
3. **Emits a `clipboard-write` message** through the existing desktop bridge with
   deduplication: a `lastText` guard prevents double-emits when both paths fire for the
   same copy, and `setTimeout(0)` defers the emit until after the browser's own
   clipboard write settles (Issue #4: the old wrapper swallowed promise rejections and
   raced the native write).
4. **Handles `clipboard-write` in Rust with `arboard::Clipboard::set_text`**, gated with
   `#[cfg(target_os = "windows")]` to match the JS injection scope (Issue #5). The
   `arboard` crate is now a Windows-only dependency in `Cargo.toml`.
5. **Test is parameterized** by an explicit `target_os` string argument to `init_script`,
   so it verifies all three branches (windows/macOS/linux) regardless of CI host
   (Issue #6: the old test used `cfg!(target_os = "windows")` which could only ever pass
   as `false == false` on non-Windows CI).

This is better scoped than modifying individual WebUI copy call sites. The defect is
specific to the Windows desktop host and its WebView2/native clipboard interaction, while
the WebUI also runs in ordinary browsers and should remain platform-neutral.

## Recommendation

Proceed with the fix in [`hermes-webui/hermes-desktop-rust`](https://github.com/hermes-webui/hermes-desktop-rust).

Keep it Windows-scoped and retain both the `_copyText` wrapper and `copy` event listener. Before merging, verify on a real Windows/WebView2 build that `Win+V` records:

- message and code-block copy buttons;
- selected text copied with Ctrl+C;
- context-menu Copy;
- workspace path or content copy controls;
- repeated identical copies;
- copies after navigation or page reload.

Also check whether one user action can trigger both interception paths — the dedup
`lastText` guard should prevent double-writes, but a Windows manual or end-to-end test
is still needed to confirm that the native write reaches Clipboard History.

## Primary-source URLs

- [`nesquena/hermes-webui` pull requests](https://github.com/nesquena/hermes-webui/pulls)
- [WebUI PR #6957, open](https://github.com/nesquena/hermes-webui/pull/6957)
- [WebUI PR #6157, open](https://github.com/nesquena/hermes-webui/pull/6157)
- [`hermes-webui/hermes-desktop-rust`](https://github.com/hermes-webui/hermes-desktop-rust)
