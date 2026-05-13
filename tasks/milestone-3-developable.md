---
milestone: 3
title: Developable — Node.js & Flutter Workflow
target_duration: 3-4 weeks
started: TBD
completed: TBD
status: not_started
---

# Milestone 3 — Developable

## Goal
ทำให้ editor ใช้ dev จริงได้สำหรับ stack เป้าหมาย — Node.js/TypeScript และ Flutter/Dart รวมถึง integrated terminal, test runner, hot reload (Flutter), monorepo awareness, formatting/linting

อ้างอิง spec doc: section 4.4 (Node.js workflow), section 4.5 (Flutter workflow)

## Tasks

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
_(log สำคัญที่เกิดระหว่าง milestone นี้)_

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
