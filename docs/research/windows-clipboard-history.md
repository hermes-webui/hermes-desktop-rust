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

1. Injects a Windows-only wrapper around `window._copyText`, covering WebUI copy buttons and other helper-driven programmatic copies.
2. Adds a Windows-only `copy` event listener, covering selection, keyboard, and context-menu copy paths.
3. Emits a `clipboard-write` message through the existing desktop bridge.
4. Handles `clipboard-write` in Rust with `arboard::Clipboard::set_text`.
5. Includes a test that checks the native clipboard bridge is injected only on Windows.

This is better scoped than modifying individual WebUI copy call sites. The defect is specific to the Windows desktop host and its WebView2/native clipboard interaction, while the WebUI also runs in ordinary browsers and should remain platform-neutral.

Using both interception paths is justified. Wrapping only `_copyText` could miss ordinary DOM selection or context-menu copies. Listening only for `copy` could miss programmatic clipboard writes that do not dispatch a copy event.

## Recommendation

Proceed with the fix in [`hermes-webui/hermes-desktop-rust`](https://github.com/hermes-webui/hermes-desktop-rust).

Keep it Windows-scoped and retain both the `_copyText` wrapper and `copy` event listener. Before merging, verify on a real Windows/WebView2 build that `Win+V` records:

- message and code-block copy buttons;
- selected text copied with Ctrl+C;
- context-menu Copy;
- workspace path or content copy controls;
- repeated identical copies;
- copies after navigation or page reload.

Also check whether one user action can trigger both interception paths. If so, add lightweight deduplication to avoid writing the same value twice. The existing unit test confirms platform scoping, but a Windows manual or end-to-end test is still needed to prove that the native write reaches Clipboard History.

## Primary-source URLs

- [`nesquena/hermes-webui` pull requests](https://github.com/nesquena/hermes-webui/pulls)
- [WebUI PR #6957, open](https://github.com/nesquena/hermes-webui/pull/6957)
- [WebUI PR #6157, open](https://github.com/nesquena/hermes-webui/pull/6157)
- [`hermes-webui/hermes-desktop-rust`](https://github.com/hermes-webui/hermes-desktop-rust)
