---
milestone: 2
title: Smart — LSP & Syntax
target_duration: 4-6 weeks
started: TBD
completed: TBD
status: not_started
---

# Milestone 2 — Smart

## Goal
เพิ่ม code intelligence ผ่าน tree-sitter (syntax highlighting + structural awareness) และ LSP client (completion, hover, goto definition, diagnostics, rename, code actions, etc.) โดย bootstrap ภาษาเป้าหมาย TypeScript/JavaScript + Dart ก่อน

อ้างอิง spec doc: section 4.2 (Phase 2 — Code Intelligence)

## Tasks

- [ ] **Tree-sitter integration**
  - [ ] Embed grammars: ts, tsx, js, dart, json, toml, markdown
  - [ ] Syntax highlighting via theme palette
  - [ ] Folding ranges
  - [ ] Smart indent
- [ ] **LSP client (4.2.1)** — implement complete LSP protocol
  - [ ] Completion + resolve
  - [ ] Hover
  - [ ] Definition, declaration, typeDefinition, implementation
  - [ ] References
  - [ ] Document + workspace symbols
  - [ ] Rename + prepareRename
  - [ ] Code actions + resolve
  - [ ] Formatting (full, range, onType)
  - [ ] Diagnostics (publishDiagnostics)
  - [ ] Signature help
  - [ ] Inlay hints
  - [ ] Folding range (LSP-provided)
  - [ ] Document highlight
  - [ ] Semantic tokens
  - [ ] Call hierarchy
- [ ] **LSP server management (4.2.2)**
  - [ ] Auto-launch per file type / workspace
  - [ ] Crash detection + auto-restart with exponential backoff
  - [ ] Multiple servers per language support (vtsls + eslint)
  - [ ] Server status indicator + log viewer
  - [ ] Custom server config via settings
  - [ ] Multi-root: 1 server per (language × workspace folder)
  - [ ] Send workspace/didChangeWorkspaceFolders on add/remove
- [ ] Wire vtsls + dart language-server as built-in defaults
- [ ] Performance: completion (cached) <100ms

## Blockers
- (none)

## Notes
_(log สำคัญที่เกิดระหว่าง milestone นี้)_

## Decisions Made
- [ADR-007 — Protocols: LSP + DAP + MCP](../docs/adr/adr-007-protocols-lsp-dap-mcp.md)

---

## Claude Code Handoff Prompt

```
You are working on Milestone 2 (Smart — LSP) of a Rust-based code editor.

Context:
- Spec doc: ./DeveloperDocumentation.md — focus section 4.2
- Prerequisites: M0 + M1 complete (window, text editing, multi-root workspace)
- Crates relevant: syntax/ (tree-sitter), lsp-client/, editor-core/, ui/widgets/
- Task file: tasks/milestone-2-smart.md

Goals:
1. Tree-sitter integration with incremental parsing
2. Full LSP client per section 4.2.1 capability table
3. Auto-launch + crash recovery per 4.2.2
4. Multi-root LSP behavior: 1 server per (language × folder root), tsconfig/pubspec may differ

Constraints:
- Use lsp-types crate for protocol types, write custom client
- Bootstrap languages: TypeScript, JavaScript, Dart (full LSP), Tailwind/Prisma optional
- Performance target: completion latency (cached) <100ms, hard limit <300ms

Read spec doc + ADR-007 first. Plan LSP message routing carefully — async/tokio. Update task checkboxes as you progress.
```
