# Editor Changelog

Per-PR notes for the Rust workspace. Project-level history (folder
structure, milestones, ADRs) lives in [`../meta/changelog.md`](../meta/changelog.md).

Format loosely follows [Keep a Changelog](https://keepachangelog.com/),
adapted to a single-binary workspace pre-1.0.

## [Unreleased]

### Added — LSP completion

- `textDocument/completion` round trip wired through the existing LSP
  reader/writer threads. Trigger via Ctrl-Space (cross-platform
  default) or Cmd-. (macOS-friendly fallback when the Input Source
  Switcher claims Ctrl-Space).
- Popup overlay anchored under the caret. Server responses are
  filtered locally (case-insensitive prefix match against
  `filter_text` or `label`) as the user keeps typing, and dismiss
  when the prefix grows out of word characters or the caret moves
  before the anchor.
- Keyboard: ↑/↓ navigate (auto-scrolls the visible window when the
  selection passes its edge), Enter/Tab accept the selected item,
  Esc dismisses.
- TextStack capped to the visible window so a 200-item response
  shapes 10 lines instead of 200; the typing path stays bounded.

### Known issue

Typing or deleting characters that flip tree-sitter's parser state
(notably an opening `"` that leaves a string unterminated) causes
a ~150-250 ms render frame on long files: tree-sitter's incremental
parse has to recover the new tree, and cosmic-text re-shapes every
`BufferLine` whose highlight categories changed. The fix is
bounded-height shaping + on-demand reshape for off-screen lines,
which means refactoring TextStack's scroll/clipping model — punted
to a follow-up so M2 stays mergeable. Steady-state typing latency
is unaffected (sits at 5-30 ms).

### Added — M2 (Smart)

- **Tree-sitter syntax** with 15 grammars: Rust, TypeScript, TSX,
  JavaScript, JSON, Python, Go, C, Markdown, TOML, YAML, Dart, Bash,
  Lua, Ruby. Each language has a context-sensitive classifier that
  uses parent-node + field-name signals (e.g. Rust function names
  via `function_item.name`; Python via `function_definition.name`;
  Dart signatures; C `call_expression.function`).
- **Incremental parsing** — `editor-core` captures `BufferDelta` for
  every edit (start/old-end/new-end in bytes + tree-sitter points),
  the syntax highlighter replays them onto its cached `Tree` via
  `tree.edit()` so reparse touches only the affected subtrees.
  Undo / redo set `tree_invalidated` so the cached tree is dropped.
- **Highlight cache** keyed on the editor's monotonic `revision`:
  tab switches and selection-only changes skip the tree-sitter pass
  entirely.
- **Theme engine** — `theme.toml` at the same XDG path as `settings.toml`,
  hot-reloaded via `notify`. Bundled themes: Default Dark, Solarized
  Dark/Light, Monokai, Gruvbox Dark, Nord, Tokyo Night. Theme picker
  + "Browse…" file dialog in the command palette.
- **LSP client** — new crate `editor-lsp-client` with hand-rolled
  JSON-RPC 2.0 framing (`Content-Length: …\r\n\r\n` headers),
  subprocess wrapper that spawns a server, a reader thread that
  pushes incoming messages onto a channel, and a writer thread that
  drains outgoing messages onto stdin (so large `didChange` payloads
  never block render). Built-in defaults: rust-analyzer (Rust),
  typescript-language-server (TS/TSX/JS). Servers are spawned lazily
  on first matching file; missing binaries disable LSP silently.
- **LSP features wired:** diagnostics (gutter dots tinted by
  severity, status-bar count `⚠ E/W/I/H`), hover (`Cmd-I`, popup
  under caret), go-to-definition (`F12` / `Cmd-click`, opens new tab
  if cross-file). 100 ms poll cadence + 100 ms `didChange` debounce
  so rust-analyzer doesn't re-analyse on every keystroke.
- **Smart bracket auto-pair** — overtype the matching closer instead
  of inserting a duplicate; wrap a non-empty selection in
  brackets/quotes when typing the opener.
- **Find count in status bar** — `N/M` appears ahead of the
  Ln/Col/lines block when a find query is active.
- **Per-language LSP `languageId`** so TypeScript / TSX / JS share
  one process but identify themselves correctly.

### Changed

- `cosmic-text` reshape path now does a per-line prefix+suffix LCS
  diff instead of `set_rich_text` (which rebuilds every BufferLine).
  Keystroke P99 on a 4000-line file fell from ~280 ms (full reshape
  + LSP write blocking) to ~27 ms. Same diff path is used by the
  gutter, so Enter on a long file no longer re-shapes the digit
  column. Implementation: `editor-ui-text::TextStack::set_content_rich`.
- LSP `didChange` now ships on a dedicated writer thread, not the
  render thread. Outgoing message payload moves through an `mpsc`
  channel, so a multi-MB serialise+write can't block keystroke
  latency.
- Workspace root for LSP servers now walks up to the topmost
  Cargo.toml / package.json / tsconfig.json / .git ancestor, so
  rust-analyzer sees the workspace manifest instead of the inner
  crate manifest (cross-crate inference works).
- File paths from the command line and from `open_path` are
  canonicalised, so `file://` URLs are always absolute.

### Added — M1 (Editable)

- Buffer (`editor-buffer`): ropey-backed `TextBuffer` with
  `Position`, `LineEnding` detection (LF / CRLF), char/byte/line
  conversions, slice / insert / remove / replace.
- Editor (`editor-core`): grapheme-aware multi-cursor `Selection`
  set, tree-based `UndoTree`, word/line/buffer movement, indent /
  outdent / toggle-comment / move-lines / delete-line, monotonic
  `revision` counter and `take_pending_edits()` for cache callers.
- Scene graph (`editor-ui-scene`): retained quads + text nodes with
  dirty-region tracking, viewport intersection, damage rect union.
- Text (`editor-ui-text`): shared `FontSystem` + `SwashCache` +
  `TextGpu` (atlas, renderer, viewport) so all stacks batch into
  one `prepare` + `render` per frame. Monospace family default to
  keep Latin stable across the Thai fallback boundary.
- Render (`editor-ui-render`): wgpu pipeline that turns the scene's
  quads + text into one render pass.
- App (`crates/app`): main binary tying it all together. Tab bar,
  command palette (`Cmd-Shift-P`, fuzzy via `nucleo-matcher`),
  find/replace (case + whole-word toggles), gutter with line
  numbers + active-line highlight + indent guides, status bar
  (file info, position, flash messages), settings (TOML, hot-reload
  via `notify`), clipboard (`arboard`), auto-indent on Enter,
  bracket auto-pair, standard editor shortcuts (Cmd-D / Cmd-K /
  Cmd-Alt-↑↓ / Cmd-Backspace / Cmd-Shift-K / Cmd-/ / Cmd-Alt-Z for
  word-wrap toggle / Cmd-Alt-W for visible whitespace / …).
- File I/O: drag-and-drop, multi-file open, Save / Save As /
  Save All, dirty indicator, confirm-on-close-with-unsaved.

### Added — M0 (Spike)

- Initial winit window + wgpu surface + glyphon multilingual text
  rendering. Thai shaping (`Shaping::Advanced`) verified before
  any other code lands.

[Unreleased]: https://github.com/thappatan/LightEditor
