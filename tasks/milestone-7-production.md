---
milestone: 7
title: Production — Debug, Git, Polish, AI UX
target_duration: ongoing
started: TBD
completed: TBD
status: not_started
---

# Milestone 7 — Production

## Goal
ทำให้ editor พร้อม ship จริง — เพิ่ม DAP (debugger), Git integration, polish features (minimap, breadcrumbs, bracket colorization, etc.), AI UX polish, performance tuning, และ cross-platform QA

อ้างอิง spec doc: section 4.3 (DAP), section 4.6 (Git), section 4.7 (Polish), section 5.I (AI UX Polish), section 8 (Performance Targets)

## Tasks

- [ ] **Debugging / DAP (4.3)**
  - [ ] DAP client implementation
  - [ ] Breakpoints (line, conditional, log, exception, function)
  - [ ] Step over/into/out/continue/pause
  - [ ] Variables + scopes + watch panel
  - [ ] Call stack + thread switcher
  - [ ] Debug console (REPL evaluate in context)
  - [ ] Run/Debug configurations (compat `.vscode/launch.json`)
  - [ ] Multiple concurrent debug sessions
  - [ ] Adapters: `vscode-js-debug`, `dart debug_adapter`, `flutter debug_adapter`
  - [ ] DevTools embedded (Flutter)
  - [ ] Source map support (TS, esbuild, swc)
  - [ ] Browser debug (Chrome/Edge protocol)
- [ ] **Git (4.6)**
  - [ ] File status in explorer (M/A/D/U/?)
  - [ ] Gutter diff (added/modified/deleted)
  - [ ] Inline blame
  - [ ] Stage/unstage hunk/line/file
  - [ ] Commit UI + diff preview + amend
  - [ ] Branch switcher + create/delete/merge
  - [ ] Merge conflict 3-way resolution UI
  - [ ] Stash management
  - [ ] Push/pull/fetch + remote management
  - [ ] git2 crate + shell fallback
- [ ] **Polish (4.7)**
  - [ ] Minimap
  - [ ] Breadcrumbs (path + symbol)
  - [ ] Bracket pair colorization
  - [ ] Bracket pair matching highlight
  - [ ] Indent guides
  - [ ] Code folding (LSP + tree-sitter)
  - [ ] Snippets (LSP snippet syntax)
  - [ ] Markdown preview
  - [ ] Theme system (TOML, hot reload)
  - [ ] Sticky scroll
  - [ ] Word wrap indicator
- [ ] **AI UX polish (5.I)**
  - [ ] Streaming diff apply
  - [ ] Hunk-level accept/reject
  - [ ] Background agent tasks + notification
  - [ ] Model picker per feature (Tab=Haiku, Edit=Sonnet, Agent=Opus)
  - [ ] Cost / token meter in status bar
  - [ ] Privacy kill switch (toggle ALL AI off)
  - [ ] Conversation search
  - [ ] Custom system prompt per workspace (`.editorrules`)
- [ ] **Performance tuning vs section 8 targets**
  - [ ] Cold start <100ms
  - [ ] Open 1GB file <2s
  - [ ] Keystroke P99 <16ms
  - [ ] Scroll 120Hz
  - [ ] Memory idle <100MB
  - [ ] Find in workspace 10k files <500ms
- [ ] **Cross-platform QA**
  - [ ] macOS (primary)
  - [ ] Linux
  - [ ] Windows
  - [ ] Auto-update mechanism (see open question)

## Blockers
- (none — ongoing after M6)

## Notes
_(log สำคัญที่เกิดระหว่าง milestone นี้)_

## Decisions Made
- _(decisions about plugin system, telemetry, update mechanism may land here)_

---

## Claude Code Handoff Prompt

```
You are working on Milestone 7 (Production) of a Rust-based code editor.

Context:
- Spec doc: ./DeveloperDocumentation.md — sections 4.3, 4.6, 4.7, 5.I, 8
- Prerequisites: M0 through M6 complete (functional, AI-capable editor)
- Crates relevant: dap-client/, git/, ui/widgets/, theme/, ai/* (UX polish)
- Task file: tasks/milestone-7-production.md

Goals:
1. DAP integration so users can actually debug (parity with VS Code core debug)
2. Git workflow that doesn't push users back to terminal for common operations
3. Polish features that make the editor feel premium
4. AI UX that distinguishes from competitors (model picker, cost meter, privacy kill switch)
5. Hit all performance targets in section 8

Constraints:
- DAP client must accept `.vscode/launch.json` to reduce migration friction
- Use git2 crate; fall back to shell `git` only for features git2 lacks
- All polish features must respect 120Hz target — minimap/blame should not drop frames

Read sections 4.3, 4.6, 4.7, 5.I, and 8 thoroughly. This milestone is the longest and most parallelizable — coordinate sub-streams. Update task checkboxes as you go. Open new ADRs for: plugin system (defer-or-not), update mechanism, telemetry policy.
```
