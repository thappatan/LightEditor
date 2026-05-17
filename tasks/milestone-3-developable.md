---
milestone: 3
title: Developable — Node.js & Flutter Workflow
target_duration: 3-4 weeks
started: 2026-05-17
completed: in_progress
status: partial_complete
---

# Milestone 3 — Developable

## Goal
ทำให้ editor ใช้ dev จริงได้สำหรับ stack เป้าหมาย — Node.js/TypeScript และ Flutter/Dart รวมถึง integrated terminal, test runner, hot reload (Flutter), monorepo awareness, formatting/linting

อ้างอิง spec doc: section 4.4 (Node.js workflow), section 4.5 (Flutter workflow)

## Tasks

### Infrastructure (shipped this session)

- [x] **File-tree sidebar** (`Cmd-B`) — flat-vec model, lazy expand, click to open, active-doc highlight, hidden-dirs filter
- [x] **Two-pass render** — separate overlay layer so popup quads occlude editor text; theme `overlay_bg` forced opaque
- [x] **Git gutter** — libgit2 diff vs HEAD, per-line Added/Modified/Deleted markers
- [x] **Find in files** (`Cmd-Shift-F`) — ripgrep-style search via `ignore::WalkBuilder` + regex, overlay panel with scroll
- [x] **Embedded terminal** (`Cmd-J`) — alacritty_terminal-backed PTY pane, focus / wheel / cursor / resize wired

### Terminal polish — open

- [ ] **ANSI colour rendering** — 256-entry palette + per-cell rich-text spans; biggest open piece of the terminal story
- [ ] Title forwarded to pane header (alacritty `Event::Title`)
- [ ] Bell ring (`Event::Bell`)
- [ ] Bracketed-paste mode

### File-tree polish — open

- [ ] File watcher → auto-refresh on filesystem changes (notify)
- [ ] Keyboard navigation (↑/↓, Enter to open)
- [ ] Drag-resize handle on the sidebar width
- [ ] Hidden-dirs list moved into settings.toml
- [ ] Git-status decorations on tree rows (M / ? / U)

### Original M3 scope

- [ ] **Node.js / TypeScript (4.4)**
  - [ ] Auto-detect `package.json` → mark workspace
  - [ ] Script runner sidebar (npm/pnpm/yarn/bun, detect from lockfile)
  - [ ] Monorepo support: pnpm workspaces, npm workspaces, turborepo, nx
  - [ ] `.env` syntax + value resolution + `.env.local` precedence
  - [ ] tsconfig path mapping aware
  - [ ] Auto-import preference setting (relative vs alias)
  - [ ] Prettier + ESLint integration
  - [ ] Format on save toggle
  - [ ] Organize imports on save
  - [ ] Test detection: Jest, Vitest, Mocha, node:test
    - [ ] Gutter run/debug icons per test/file
    - [ ] Test explorer sidebar
    - [ ] Inline result indicators
    - [ ] Watch mode
- [ ] **Flutter / Dart (4.5)**
  - [ ] pubspec.yaml parsing + dependency tree view
  - [ ] Auto `pub get` after pubspec edit
  - [ ] Multi-package workspace (melos)
  - [ ] Lock file stale warning
  - [ ] **Hot reload** (killer feature)
    - [ ] `r` keybind → hot reload, `R` → hot restart
    - [ ] Toggle auto-reload on save
    - [ ] Visual indicator at status bar
    - [ ] Inline reload error + jump to error
    - [ ] Reload history log
  - [ ] Device management
    - [ ] `flutter devices` → device picker in status bar
    - [ ] `flutter emulators` launcher
    - [ ] Wireless debug devices
    - [ ] Multi-target run
  - [ ] `build_runner` integration (status + one-click trigger)
- [ ] Integrated terminal polish (multiple terminals, cwd per workspace folder)
- [ ] Auto-detect Dart SDK path (`which dart`, `FLUTTER_ROOT`)

## Blockers
- (none — depends on M2 LSP working)

## Notes

M3 split into two phases:

1. **Infrastructure** (this session) — the panes, panels, and gutter
   chrome that the workflow features need a home in. File tree,
   git gutter, find-in-files, embedded terminal. All shipped as
   small mergeable PRs (#38 → #43).
2. **Workflow features** (open) — the Node.js / TypeScript / Flutter
   pieces from the original task list above. They build on top of
   the terminal (npm script runner, test runners, hot reload) +
   the file tree (multi-root UX, package.json detection).

The render-layering refactor (PR #39) had to ship inside this
milestone too — the existing single-layer pipeline let editor text
bleed through popup backgrounds. Fixing it touched the whole render
path and unlocked everything that followed.

The biggest deferred item is **ANSI colour rendering** in the
terminal. The pane shows a monochrome grid for now; programs that
detect a TTY (ls, git, cargo) write colour escape codes that we
don't honour yet. ~200 LOC behind a per-cell palette + rich-text
span build.

## Decisions Made
- _(none new — Hot reload UX details may need ADR)_

---

## Claude Code Handoff Prompt

```
You are working on Milestone 3 (Developable) of a Rust-based code editor.

Context:
- Spec doc: ./DeveloperDocumentation.md — sections 4.4 (Node.js) and 4.5 (Flutter)
- Prerequisites: M0 + M1 + M2 complete (LSP working for TS + Dart)
- Crates relevant: workspace/, terminal/, ui/widgets/, plus a new "tooling" or "tasks" crate as needed
- Task file: tasks/milestone-3-developable.md

Goals:
1. Make this editor a daily driver for Node.js/TypeScript projects
2. Make Flutter hot reload UX better than official tools (section 4.5.3 — killer feature)
3. Test runners must work for Jest, Vitest, node:test (TS) and `flutter test` (Dart)
4. Monorepo: pnpm workspaces + melos must work out of the box

Constraints:
- DAP (debug) is NOT in this milestone — defer to M7
- Stay focused on TS + Dart; do not generalize for other languages yet
- Performance: hot reload latency should match or beat `flutter run` directly

Read spec doc sections 4.4 + 4.5 + 4.1.5 (multi-root) first. Plan how Flutter device picker and Node script runner share UI patterns. Update task checkboxes as you go.
```
