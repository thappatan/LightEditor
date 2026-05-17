# Editor Changelog

Per-PR notes for the Rust workspace. Project-level history (folder
structure, milestones, ADRs) lives in [`../meta/changelog.md`](../meta/changelog.md).

Format loosely follows [Keep a Changelog](https://keepachangelog.com/),
adapted to a single-binary workspace pre-1.0.

## [Unreleased]

### Added — M3 (Developable)

- **File-tree sidebar** (`Cmd-B`). Root is derived from the active
  doc's project (`find_project_root` walk shared with the LSP
  layer); falls back to CWD when no doc has a path. Tree state is a
  flat `Vec<TreeNode>` representing only the *visible* rows —
  expanding a directory splices children in, collapsing drains them —
  so rendering is a single linear pass. `.git`, `node_modules`,
  `target`, `.next`, `dist`, `build` are filtered. Click a file to
  open; the row matching the active document gets a faint
  highlight underlay.
- **Two-pass render**: editor + chrome quads/text render first,
  then a separate overlay pass paints floating panels
  (palette / hover / completion / find / find-in-files) and their
  text. Fixes the bug where the editor's text bled through opaque
  popup backgrounds because all text used to batch after all quads
  in a single pass. New `TextGpu::overlay_renderer` +
  `State::overlay_quads` + `State::overlay_scene` carry the second
  layer; the wgpu render pass now does main quads → main text →
  overlay quads → overlay text in z-order. Theme overlay_bg colours
  also force alpha=ff across the default + 6 bundled themes so
  occlusion lands cleanly.
- **Git gutter**: `crates/app/src/git.rs` diffs the active doc's
  buffer text against its HEAD blob via libgit2 and paints a thin
  coloured bar on each line in the gutter (green added / blue
  modified / red deletion wedge). Untracked-but-in-repo files mark
  every line as Added. Result keyed on the editor's revision so
  tab switches stay free. Cost <5 ms on a 4000-line file. Path
  canonicalisation handles macOS's `/var` → `/private/var` symlink
  for tests under `/tmp`. 6 unit tests.
- **Find in files**: `Cmd-Shift-F` opens a centred overlay panel
  (760×500 dip) with a query row + scrollable results. `crates/app/
  src/find_in_files.rs::search` walks the workspace via
  `ignore::WalkBuilder` (`.gitignore` respected automatically),
  matches lines via a case-insensitive `RegexBuilder` (literal-
  escaped query), caps at 500 hits / files > 1 MB skipped /
  non-UTF8 blobs silently dropped. Panel uses the overlay layer
  (opaque scrim), focus toggles between input row and results
  (`Tab`), `Enter` runs search / opens selected, `Esc` dismisses.
  Visible-window TextStack so a 500-match list still shapes 16
  lines; wheel scrolls scrollback through the list. 6 unit tests
  cover substring / case / .gitignore / binary skip / wrap nav.
- **Embedded terminal** (`Cmd-J`): bottom-anchored pane at 260 dip,
  backed by `alacritty_terminal` 0.26. PTY forks `$SHELL` (or
  `/bin/sh`) rooted at the project root. `AppTermProxy` bridges
  alacritty's `EventListener` to `AppEvent::TerminalWakeup` so
  output triggers a redraw without polling. Cell count syncs from
  pane pixel size on spawn / toggle / window resize. Keyboard
  routes to PTY when focused (Enter→\r / Backspace→\x7f / arrows
  → ANSI CSI / printable→event.text). Mouse click in pane focuses;
  click outside unfocuses. Wheel scrolls scrollback via
  `Term::scroll_display(Scroll::Delta)`. Cursor block drawn at
  the PTY cursor's grid position (caret colour when focused, dim
  active-line colour when not). Editor area shrinks when pane is
  visible: `editor_bottom_y()` is the single helper that gutter +
  sidebar + text bounds + `visible_height` / `max_scroll` all use.

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
