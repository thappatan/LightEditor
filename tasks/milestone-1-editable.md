---
milestone: 1
title: Editable — Basic Text Editing
target_duration: 4-6 weeks
started: TBD
completed: TBD
status: not_started
---

# Milestone 1 — Editable

## Goal
ทำให้ editor ใช้งานเบื้องต้นได้ — เปิด, แก้, save ไฟล์ พร้อม multi-cursor, undo/redo, command palette, file explorer, และ workspace management (single + multi-root + workspace file) ตามที่ระบุใน section 4.1 ของ spec doc โดย dogfooding ตัวเองได้แต่ยังไม่มี code intelligence

อ้างอิง spec doc: section 4.1 (Phase 1 — Foundation), section 4.1.5 (Workspace Management)

## Tasks

- [ ] **Text editing (4.1.1)**
  - [ ] Cursor + selection (char/word/line/block)
  - [ ] Multi-cursor + multi-selection (Ctrl+D, Alt+Click)
  - [ ] Undo/redo (tree-based)
  - [ ] Clipboard integration (arboard)
  - [ ] Auto-indent + bracket auto-pair
  - [ ] Column selection (Alt+drag)
  - [ ] Soft + hard line wrap
  - [ ] Trim trailing whitespace on save toggle
  - [ ] EOL detection + preserve (LF/CRLF)
- [ ] **Navigation (4.1.2)**
  - [ ] Tab bar + buffer list
  - [ ] Split pane (horizontal/vertical, nested)
  - [ ] File explorer sidebar
  - [ ] Quick file picker (Ctrl+P) using nucleo
  - [ ] Recent files + sessions
  - [ ] Go to line, jump to bracket, nav history
- [ ] **Search (4.1.3)**
  - [ ] Find/Replace in buffer (regex/case/whole word)
  - [ ] Find in files (ripgrep lib)
  - [ ] Search result navigator + highlight
- [ ] **Shell (4.1.4)**
  - [ ] Command palette (Ctrl+Shift+P) — fuzzy
  - [ ] Integrated terminal (alacritty_terminal)
  - [ ] Configurable keybindings (TOML)
  - [ ] Settings UI + reload on edit
  - [ ] Notification toaster
- [ ] **Workspace management (4.1.5)**
  - [ ] Open Folder, Add Folder, Remove Folder
  - [ ] Workspace file format (.editor-workspace.json or TOML — see open question)
  - [ ] Recent workspaces list
  - [ ] Workspace trust prompt
  - [ ] Settings hierarchy (Default → User → Workspace → Folder)
  - [ ] Multi-root sidebar UX (folder roots as top-level nodes)

## Blockers
- (none)

## Notes
_(log สำคัญที่เกิดระหว่าง milestone นี้)_

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
