---
milestone: 2
title: Smart — LSP & Syntax
target_duration: 4-6 weeks
started: 2026-05-16
completed: 2026-05-17
status: partial_complete
---

# Milestone 2 — Smart

## Goal
เพิ่ม code intelligence ผ่าน tree-sitter (syntax highlighting + structural awareness) และ LSP client (completion, hover, goto definition, diagnostics, rename, code actions, etc.) โดย bootstrap ภาษาเป้าหมาย TypeScript/JavaScript + Dart ก่อน

อ้างอิง spec doc: section 4.2 (Phase 2 — Code Intelligence)

## Tasks

- [x] **Tree-sitter integration**
  - [x] Embed grammars: rust, ts, tsx, js, json, python, go, c, markdown, toml, yaml, dart, bash, lua, ruby (15 total)
  - [x] Syntax highlighting via theme palette
  - [x] Incremental parsing via `tree.edit()` + `BufferDelta` deltas (`editor-core::PendingEdits`)
  - [x] Highlight cache keyed on editor `revision` — skip reparse on tab switch
  - [x] Per-language context-sensitive classifiers (function names, macros, lifetimes, fields)
  - [ ] Folding ranges — deferred (tree-sitter side; LSP side too)
  - [ ] Smart indent (language-aware) — partial; only generic auto-indent shipped
- [x] **LSP client (4.2.1)** — minimum-viable subset shipped
  - [x] Hover (`textDocument/hover`) — Cmd-I popup
  - [x] Definition (`textDocument/definition`) — F12 + Cmd-click
  - [x] Diagnostics (`textDocument/publishDiagnostics`) — gutter dots + status-bar counts
  - [ ] Completion + resolve — biggest follow-up
  - [ ] Declaration / typeDefinition / implementation — deferred
  - [ ] References — deferred
  - [ ] Document + workspace symbols — deferred
  - [ ] Rename + prepareRename — deferred
  - [ ] Code actions + resolve — deferred
  - [ ] Formatting (full, range, onType) — deferred
  - [ ] Signature help — deferred
  - [ ] Inlay hints — deferred
  - [ ] Folding range (LSP) — deferred
  - [ ] Document highlight — deferred
  - [ ] Semantic tokens — deferred
  - [ ] Call hierarchy — deferred
- [x] **LSP server management (4.2.2)** — basics; resiliency + multi-root deferred
  - [x] Auto-launch per file type (lazy spawn on first matching file)
  - [x] JSON-RPC framing on a dedicated writer thread + reader thread
  - [x] Workspace-root walk (finds topmost Cargo.toml / package.json / tsconfig.json / .git)
  - [x] didChange debounce (100 ms) so analyser isn't trashed on every keystroke
  - [x] Graceful disable when server binary missing (logs, no crash)
  - [ ] Crash detection + auto-restart with exponential backoff — only detection shipped
  - [ ] Multiple servers per language (vtsls + eslint) — deferred
  - [ ] Server status indicator + log viewer — deferred
  - [ ] Custom server config via settings — deferred
  - [ ] Multi-root: 1 server per (language × workspace folder) — single root only
  - [ ] Send workspace/didChangeWorkspaceFolders — deferred
- [x] Wire rust-analyzer (Rust) and typescript-language-server (TS/TSX/JS) as built-in defaults
- [ ] Wire vtsls (preferred per spec §4.4.1) — uses typescript-language-server instead for now
- [ ] Wire dart-language-server — deferred to M3 alongside Flutter workflow
- [x] **Performance pass** — see [Notes](#notes)
  - [x] Keystroke latency P99 < 33 ms on a 4000-line buffer (was 280 ms before perf pass)
  - [ ] LSP completion (cached) < 100 ms — n/a until completion lands

## Blockers
- (none)

## Notes

### Shipped scope

M2 closed with the **diagnostics + hover + goto-definition** vertical
slice through real language servers (rust-analyzer; typescript-language-
server). That makes the editor genuinely "smart" on Rust and TS/JS
files even though most LSP capabilities are still pending — the
extension surface is wired and adding the rest is incremental.

### Performance journey

The biggest finding was that cosmic-text's `set_rich_text` reshapes
the entire buffer on every keystroke. On a 4000-line file (this very
project's `crates/app/src/main.rs`) that cost ~120 ms per keystroke
on its own; with rust-analyzer competing for CPU during indexing it
spiked to 280 ms. Fixed by:

1. Adding a per-line diff (`BufferLine::set_text` skips lines whose
   text/attrs match the existing entry) — handled in-line edits.
2. Switching the diff to **prefix + suffix LCS** — handled
   Enter/delete-line, where every tail line shifts index but keeps
   content.
3. Routing plain `set_content` through the same diff — fixed the
   gutter, which would otherwise full-reshape its 4000-line digit
   buffer on every line-count change.
4. Moving LSP stdin writes to a dedicated writer thread so a
   100-KB-payload `didChange` doesn't block render.

After all four: keystroke P99 27 ms / P50 ~10 ms on the same file
while rust-analyzer is indexing. Target (16 ms) hit most frames;
33 ms hard limit hit every frame.

### Deferred items

The unticked boxes above are real follow-ups, not bugs. The big ones:

- **Completion** — the most-used LSP feature; not yet implemented.
- **Multi-root LSP** — spec §4.1.5 calls for one server per
  (language × workspace folder); we run one per language only.
- **Auto-restart on crash** — we detect server exit (`ServerExited`
  event) and log it, but don't relaunch.
- **Server-config in settings** — currently the server command is
  hard-coded per `ServerKind`.

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
