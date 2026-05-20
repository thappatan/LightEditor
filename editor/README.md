# Editor — Rust Workspace

Rust workspace for the code editor itself. Mirrors structure in spec doc section 7.

> Project management (milestones, ADRs, research notes) lives in the **parent folder** — `../docs/`, `../tasks/`, `../meta/`. This `editor/` directory is **code only**.

## Quickstart for Claude Code

1. Read `../DeveloperDocumentation.md` (spec doc — source of truth)
2. Read the milestone file for current work: `../tasks/milestone-N-*.md`
3. Read relevant ADRs in `../docs/adr/`
4. Initialize crates as needed:

   ```bash
   cargo new --lib crates/buffer
   cargo new --lib crates/ui/window
   # ...
   cargo new crates/app          # binary
   ```

5. Uncomment the matching `members` line in `Cargo.toml`
6. Build: `cargo build`

## Crate Map

Status legend: ✅ shipped · 🟡 partial · ⚪ planned

| Status | Path | Purpose | Spec |
|--------|------|---------|------|
| 🟡 | `crates/ui/render` | wgpu pipeline, scene rasterisation | 3.2 |
| ✅ | `crates/ui/text` | cosmic-text + swash + glyphon glue, per-line LCS diff | 3.3 |
| ✅ | `crates/ui/scene` | retained-mode scene graph + dirty tracking | 3.2 |
| ⚪ | `crates/ui/window` | (folded into `crates/app` for now) — winit + IME + clipboard | 3.5 |
| ⚪ | `crates/ui/widgets` | primitive widgets — building inline in `app` for v1 | 3 |
| ✅ | `crates/buffer` | ropey-backed `TextBuffer`, `Position`, `BufferDelta` | 4.1.1 |
| ✅ | `crates/editor-core` | grapheme-aware multi-cursor, tree-based undo, `PendingEdits` | 4.1.1 |
| ✅ | `crates/syntax` | tree-sitter + 17 grammars, incremental reparse, per-language classifiers | 4.2 |
| ✅ | `crates/lsp-client` | JSON-RPC client, reader+writer threads, high-level LSP wrappers | 4.2 |
| ✅ | `crates/config` | TOML settings + theme (+ VSCode-JSON theme import) + hot-reload | 4.1.4 / 4.7 |
| ✅ | `crates/app` | main binary; wires everything — see module table below | 7 |
| ⚪ | `crates/dap-client` | DAP client | 4.3 |
| 🟡 | `crates/git` | _inlined in `crates/app/src/git.rs`_ — libgit2 diff vs HEAD (gutter) + workspace status (sidebar). Promote when blame / status panel land. | 4.6 |
| 🟡 | `crates/terminal` | _inlined in `crates/app/src/terminal.rs`_ — `alacritty_terminal` PTY pane with ANSI colour. Promote when multi-pane lands. | 4.1.4 |
| ⚪ | `crates/workspace` | single/multi-root/workspace-file management (single-root works today inline) | 4.1.5 |
| ⚪ | `crates/theme` | (theme types live in `crates/config` for now) | 4.7 |
| ⚪ | `crates/ai/providers` | `LlmProvider` trait + Anthropic/OpenAI/Google/OR/Ollama/OAI-compat | 5.A |
| ⚪ | `crates/ai/completion` | inline completion / ghost text | 5.B |
| ⚪ | `crates/ai/chat` | chat sidebar + @ mentions | 5.D |
| ⚪ | `crates/ai/agent` | agent loop, tool dispatch, checkpoint | 5.F |
| ⚪ | `crates/ai/rag` | embedding + lancedb + hybrid retrieval | 5.E |
| ⚪ | `crates/ai/mcp` | MCP client | 5.G |

Per-PR detail is in [`CHANGELOG.md`](./CHANGELOG.md); project-level
milestone status is in [`../tasks/`](../tasks/).

### `crates/app` module layout

The app crate is the home for several features that should eventually
become their own crates. Today's modules:

| Module | Role |
|--------|------|
| `main.rs` | `App` / `State` / winit + wgpu glue, render loop, key routing |
| `document.rs` | per-tab document model (path / dirty / undo plumbing) |
| `palette.rs` | command palette (`Cmd-Shift-P`, fuzzy via `nucleo-matcher`, scrolled window) |
| `find.rs` | in-buffer find/replace state |
| `find_in_files.rs` | workspace search (`Cmd-Shift-F`, `ignore` + `regex`) |
| `file_tree.rs` | sidebar tree (`Cmd-B`) — keyboard nav, watcher reload, drag-resize, configurable hidden dirs |
| `git.rs` | per-line diff vs HEAD + workspace status + directory roll-up |
| `terminal.rs` | PTY pane via `alacritty_terminal` (`Cmd-J`) |
| `terminal_palette.rs` | ANSI / xterm-256 colour resolution for the pane |
| `scripts.rs` | `package.json` script detection + package-manager pick |
| `flutter.rs` | `pubspec.yaml` detection + `flutter devices` parsing |
| `lsp.rs` | per-document LSP state machine + popup overlays |

## Feature status

Working today (M1–M3):

- **Editing** — multi-cursor, tree-based undo (restores cursor),
  grapheme-aware movement, auto-indent, bracket auto-pair,
  comment toggle, find/replace, command palette, multi-tab.
- **Syntax** — tree-sitter for 17 languages (Rust, TS/TSX/JS,
  JSON, Python, Go, C, Markdown, TOML, YAML, Dart, Bash, Lua,
  Ruby, HTML, CSS), incremental reparse.
- **LSP** — diagnostics, hover, go-to-definition, completion for
  Rust + TypeScript.
- **Chrome** — file-tree sidebar (keyboard nav, fs-watcher
  refresh, drag-resize, git-status decorations with directory
  roll-up), git gutter, find-in-files, embedded terminal with
  ANSI colour + background colours.
- **Workflow** — npm/pnpm/yarn/bun script runner and Flutter
  run / hot-reload / device-picker via the palette; Cmd-S on a
  `.dart` file hot-reloads a running Flutter session.
- **Theming** — TOML themes with hot-reload, 7 bundled themes,
  VSCode-JSON theme import (`include` chains + scope mapping),
  sRGB-correct rendering, themable terminal palette.
- **Input** — layout-independent shortcuts (works on non-Latin
  keyboard layouts), clipboard paste into every dialog.

Not yet built: DAP debugging (M7), the AI stack (M4–M6),
multi-root workspace UX, split panes.

## Languages directory

`languages/{typescript,javascript,dart}/` — built-in language configs (tree-sitter grammars, LSP defaults, file associations). Initialized by M2.
