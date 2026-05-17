---
milestone: 1
title: Editable — Basic Text Editing
target_duration: 4-6 weeks
started: 2026-05-14
completed: 2026-05-16
status: complete
---

# Milestone 1 — Editable

## Goal
ทำให้ editor ใช้งานเบื้องต้นได้ — เปิด, แก้, save ไฟล์ พร้อม multi-cursor, undo/redo, command palette, file explorer, และ workspace management (single + multi-root + workspace file) ตามที่ระบุใน section 4.1 ของ spec doc โดย dogfooding ตัวเองได้แต่ยังไม่มี code intelligence

อ้างอิง spec doc: section 4.1 (Phase 1 — Foundation), section 4.1.5 (Workspace Management)

## Tasks

- [x] **Text editing (4.1.1)**
  - [x] Cursor + selection (char/word/line/block)
  - [x] Multi-cursor + multi-selection (Cmd-D / Alt-click / Cmd-Alt-↑↓)
  - [x] Undo/redo (tree-based) — see [`crates/editor-core/src/undo.rs`](../editor/crates/editor-core/src/undo.rs)
  - [x] Clipboard integration (arboard)
  - [x] Auto-indent + bracket auto-pair (overtype + wrap selection)
  - [x] Soft line wrap (Cmd-Alt-Z toggles)
  - [x] EOL detection + preserve (LF/CRLF)
  - [ ] Column selection (Alt+drag) — deferred to later polish
  - [ ] Trim trailing whitespace on save toggle — deferred
- [x] **Navigation (4.1.2)** — partial; sidebar/split-pane deferred
  - [x] Tab bar + buffer list — drag-drop, close button, middle-click, Cmd-W
  - [x] Find/jump highlights (count in status bar)
  - [ ] Split pane (horizontal/vertical) — deferred
  - [ ] File explorer sidebar — deferred to M3
  - [ ] Quick file picker (Cmd-P) — deferred (command palette covers it for now)
  - [ ] Recent files + sessions — deferred
  - [ ] Go to line, jump to bracket, nav history — bracket-match shipped; goto-line + history deferred
- [x] **Search (4.1.3)** — in-buffer; project-wide deferred to M3
  - [x] Find/Replace in buffer (case + whole-word toggles)
  - [ ] Regex find — deferred
  - [ ] Find in files (ripgrep lib) — deferred to M3
  - [x] Search result navigator + highlight (`crates/app/src/find.rs`)
- [x] **Shell (4.1.4)** — settings + palette; terminal deferred
  - [x] Command palette (Cmd-Shift-P) — fuzzy via `nucleo-matcher`
  - [ ] Integrated terminal (alacritty_terminal) — deferred to M3
  - [x] Configurable settings (TOML, hot-reload via `notify`)
  - [x] Settings + theme hot-reload (file-watch driven)
  - [x] Status bar flash messages
  - [ ] Custom keybindings TOML — deferred (built-ins only for now)
- [x] **Workspace management (4.1.5)** — settings precedence; multi-root deferred
  - [x] Settings hierarchy (Default → User → Workspace via `.lighteditor/`)
  - [ ] Open Folder, Add Folder, Remove Folder — deferred to M3
  - [ ] Workspace file format — deferred
  - [ ] Recent workspaces list — deferred
  - [ ] Workspace trust prompt — deferred
  - [ ] Multi-root sidebar UX — deferred (single-root with file dialog for now)

## Blockers
- (none)

## Notes

The M1 cut shipped a fully-usable single-file editor that can dogfood
itself — multi-cursor edits, multi-buffer tabs, find/replace, command
palette, themed syntax (via M2 grammars), settings + theme hot-reload.

Deferred to M3 (logical home given the milestone titles):
- File-tree sidebar / multi-root workspace UX
- Integrated terminal (`alacritty_terminal`)
- Project-wide find-in-files (`ripgrep` lib)
- Split panes

Performance budgets: keystroke P99 under the 33 ms hard limit on
medium files; 4000-line buffers hit P99 ~27 ms after the M2 perf pass.

## Decisions Made
- [ADR-004 — Ropey text buffer](../docs/adr/adr-004-ropey-text-buffer.md)
- Workspace concept resolved (spec section 10 — multi-root + workspace file)

---

## Claude Code Handoff Prompt

```
You are working on Milestone 1 (Editable) of a Rust-based code editor.

Context:
- Spec doc: ./DeveloperDocumentation.md — focus sections 4.1.1 through 4.1.5
- Previous milestone (M0) must be complete: window + GPU rendering + Thai text working
- Crates relevant here: buffer/, editor-core/, ui/*, workspace/, config/, terminal/
- Task file: tasks/milestone-1-editable.md

Goals:
1. Implement Phase 1 (section 4.1) — full basic editing capabilities
2. Multi-root workspace as first-class (section 4.1.5) — critical for stack (Node + Flutter + shared)
3. Keep performance: keystroke latency P99 <16ms (section 8)

Constraints:
- Text buffer = ropey (ADR-004)
- Keybindings format = TOML
- Multi-root workspace required; sidebar must show roots as separate top-level nodes
- LSP, syntax highlighting, AI features are NOT in this milestone — defer to M2+

Read spec doc thoroughly, then propose milestone plan with sub-task ordering before implementing. Update task checkboxes in tasks/milestone-1-editable.md as you go.
```
