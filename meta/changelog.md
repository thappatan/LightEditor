# Project Changelog

> Track project management changes (folder structure, milestones, ADRs).
> Code-level changelog (per-PR feature/fix detail) lives in
> [`editor/CHANGELOG.md`](../editor/CHANGELOG.md).

## 2026-05-17 — Session 9 (M3 Developable — infrastructure)

- **Milestone 3 partial-complete** — infrastructure (file tree, git gutter, find-in-files, embedded terminal) shipped; Phase-2 workflow features (npm script runner, test runners, Flutter hot reload) and ANSI colour rendering in the terminal still open ([tasks/milestone-3-developable.md](../tasks/milestone-3-developable.md)).
- **File-tree sidebar** (`Cmd-B`) — flat-vec node list, root from `find_project_root`, lazy directory expansion, click-to-open + active-doc row highlight. Hardcoded ignore list (`.git`, `node_modules`, `target`, `.next`, `dist`, `build`).
- **Render layering refactor** — separate overlay layer (`overlay_quads` + `overlay_scene` + `text_gpu.overlay_renderer`) so popup quads correctly occlude editor text. Theme `overlay_bg` colours forced to alpha=ff across default + 6 bundled themes.
- **Git gutter** (`crates/app/src/git.rs`) — libgit2 diff between buffer and HEAD blob, per-line Added/Modified/Deleted markers in the gutter, keyed on editor revision (cheap, <5 ms on 4000-line files).
- **Find in files** (`Cmd-Shift-F`) — overlay panel + `ignore::WalkBuilder` + case-insensitive regex per line, capped at 500 hits / files > 1 MB skipped / binaries dropped. Visible-window TextStack so 500-match lists still shape only 16 lines.
- **Embedded terminal** (`Cmd-J`) — bottom-anchored pane backed by `alacritty_terminal` 0.26. PTY forks `$SHELL`, keyboard routes to PTY when focused (printable + Enter/Backspace/Tab/Esc/arrows/Home/End/Del/PgUp/PgDn → correct CSI bytes), mouse click focuses, wheel scrolls scrollback via `Scroll::Delta`, cursor block at the PTY's grid position, window resize syncs cell count. Editor's `editor_bottom_y()` shrinks above the pane so the gutter / sidebar / text bounds / max_scroll all clip correctly.

## 2026-05-16 → 2026-05-17 — Sessions 7–8 (M2 Smart)

- **Milestone 2 partial-complete** — diagnostics + hover + goto-def slice through real LSPs ([tasks/milestone-2-smart.md](../tasks/milestone-2-smart.md)). Completion, references, rename, formatting, multi-root LSP deferred to a follow-up milestone.
- **tree-sitter** integration with 15 grammars (Rust + TS/TSX/JS + JSON + Python + Go + C + Markdown + TOML + YAML + Dart + Bash + Lua + Ruby), incremental reparse via `tree.edit()` and `editor-core::PendingEdits`, per-language context-sensitive classifiers.
- **LSP client** crate (`editor-lsp-client`) with hand-rolled JSON-RPC framing + reader/writer threads. rust-analyzer + typescript-language-server wired as built-in defaults; binaries missing on PATH disable LSP silently.
- **Theme engine** — TOML themes (`theme.toml`) with hot-reload, 6 bundled themes (Solarized Dark/Light, Monokai, Gruvbox Dark, Nord, Tokyo Night), in-palette theme picker + "Browse…" file dialog.
- **Performance pass** — keystroke P99 fell from ~280 ms to ~27 ms on a 4000-line buffer. Root cause was cosmic-text re-shaping the whole buffer per keystroke; fixed with a per-line prefix+suffix-LCS diff in `editor-ui-text` that scopes `BufferLine::set_text` to the lines that actually changed.

## 2026-05-14 → 2026-05-16 — Session 6 (M1 Editable)

- **Milestone 1 complete** — single-file editor that can dogfood itself ([tasks/milestone-1-editable.md](../tasks/milestone-1-editable.md)).
- Shipped: ropey-backed `editor-core` (multi-cursor, tree-based undo, grapheme-aware movement), retained-mode scene graph (`editor-ui-scene`), wgpu/glyphon text pipeline (`editor-ui-render` + `editor-ui-text`), file I/O + Save All + drag-drop, tab strip with close button + middle-click, command palette (`nucleo`), find/replace in buffer with case + whole-word toggles, status bar with caret + line-count + flash messages, gutter with line numbers + active-line highlight + indent guides.
- Settings (`editor-config`): TOML at `~/Library/Application Support/lighteditor/settings.toml` + workspace override at `<cwd>/.lighteditor/settings.toml`, file-watched hot-reload via `notify` ([ADR-009](../docs/adr/adr-009-config-format-toml.md)).
- M1 deferrals (intentionally pushed to M3): file-tree sidebar, embedded terminal, multi-root workspace UX, split panes, find-in-files.

## 2026-05-14 — Session 5 (M0 Spike)

- **Milestone 0 complete** — technology stack de-risked (ADR-002/003 validated)
- Initialized `crates/app` binary; spike implements winit window + wgpu 29 surface + glyphon 0.11/cosmic-text 0.18 multilingual text rendering
- Centralized M0 dependencies in `editor/Cargo.toml` `[workspace.dependencies]`; spec's draft pins were stale (wgpu 0.20→29, cosmic-text 0.12→0.18, glyphon 0.5→0.11)
- Benchmark: frame time ~8ms (½ of 16ms target), cold start 130-170ms warm / 923ms first-ever — over 100ms target, M1 follow-up needed
- CI fixes: `cache-bin: false` + `prefix-key` bump (stale macOS cargo-bin cache), `--no-tests=pass` (nextest on empty test suite)
- Findings: [docs/research/m0-spike-results.md](../docs/research/m0-spike-results.md)

## 2026-05-14 — Session 4 (License decision)

- Resolved license open question → Apache 2.0 ([ADR-011](../docs/adr/adr-011-license-apache-2-0.md)) ratifying the LICENSE file present at repo root
- Updated `editor/Cargo.toml` workspace license metadata
- First PR through the trunk-based workflow (branch `docs/license-decision`)

## 2026-05-13 — Session 3 (CI + release pipeline)

- Added `CONTRIBUTING.md` — Conventional Commits 1.0, trunk-based branching, PR-only `main`, branch protection rules
- Added `.github/workflows/ci.yml` — fmt / check / clippy / nextest matrix (ubuntu + macos) on PR
- Added `.github/workflows/release.yml` — tag-driven build (aarch64-apple-darwin, x86_64-apple-darwin, x86_64-unknown-linux-gnu) + GitHub Release with artifacts
- Added `.github/pull_request_template.md` — perf-impact section required for render/text/AI paths
- Added `editor/release.toml` — cargo-release config (workspace-shared version, no crates.io publish, push tag)
- Updated `CLAUDE.md` with git workflow section (no AI-attribution trailers in commits)

## 2026-05-13 — Session 2 (Claude Code setup)

- Init git repo + `.gitignore` (Rust + macOS) — pre-implementation phase now under VCS
- Added `.editorconfig` (LF, UTF-8, trim trailing whitespace, 4-space indent default)
- Added CLAUDE.md (project guide for Claude Code instances)
- Installed 15 Claude Code skills (Rust, wgpu, MCP, LSP, tree-sitter, RAG, GPUI patterns, TS, Flutter, a11y, design tokens)
- Expanded `.claude/settings.local.json` permissions for common cargo/git/search commands
- Resolved 2 open questions:
  - Config format → TOML ([ADR-009](../docs/adr/adr-009-config-format-toml.md))
  - Modal editing → not supported in v1 ([ADR-010](../docs/adr/adr-010-no-modal-editing.md))

## 2026-05-13 — Session 1 (Bootstrap)

- 🎬 **Project bootstrap**
- Created folder structure per Cowork project instructions:
  - `docs/` (adr, meetings, research, inspiration, backups)
  - `tasks/` (8 milestone files + weekly/)
  - `assets/` (logos, mockups, screenshots)
  - `meta/` (this file, open-questions, tools-cost)
  - `editor/` (Cargo workspace skeleton — crates/, languages/, assets/)
- Created 8 milestone task files: M0 (Spike) → M7 (Production), all `not_started`
- Logged 8 ADRs from spec doc section 9: ADR-001 → ADR-008
- Wired open-questions.md from spec doc section 10
- Spec doc remains at root as `DeveloperDocumentation.md` (not renamed to align with project instructions path `docs/code-editor-dev-doc.md` — pending user decision)
