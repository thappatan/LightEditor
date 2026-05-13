---
milestone: 6
title: Agentic — Multi-File Edit & MCP
target_duration: 4-8 weeks
started: TBD
completed: TBD
status: not_started
---

# Milestone 6 — Agentic

## Goal
สร้าง agent loop (plan → tool_call → result → reflect → next_action) พร้อม built-in tools, safety mechanisms (approval modes, sandbox, checkpoint/rollback), และ MCP client integration เพื่อใช้ external tools (Figma, Linear, DB, browser MCP)

อ้างอิง spec doc: section 5.F (Agentic Multi-File Edit), section 5.G (MCP Integration), section 5.H (Stack-Specific AI)

## Tasks

- [ ] **Agent loop (5.F.1)**
  - [ ] Core loop: plan → tool_call → result → reflect → next_action → finish
  - [ ] Persistent conversation thread + tool transcript
  - [ ] Pause / resume / cancel mid-run
- [ ] **Built-in tools (5.F.2)**
  - [ ] `read_file(path, range?)`
  - [ ] `edit_file(path, old, new)`
  - [ ] `create_file(path, content)`
  - [ ] `delete_file(path)` (requires approval)
  - [ ] `list_directory(path)`
  - [ ] `search_codebase(query)` (uses M5 RAG)
  - [ ] `run_command(cmd, cwd?)` sandboxed
  - [ ] `get_diagnostics(path?)`
  - [ ] `run_tests(pattern?)`
  - [ ] `git_diff()`
  - [ ] `web_search(query)` optional
- [ ] **Safety (5.F.3)**
  - [ ] Approval mode setting: `auto` / `ask-each` / `read-only`
  - [ ] Command allowlist patterns
  - [ ] Command blocklist (`rm -rf /`, fork bombs)
  - [ ] Path sandbox: cwd restricted to workspace
  - [ ] Checkpoint snapshot before each action
  - [ ] Rollback to any checkpoint
- [ ] **Agent UX (5.F.4)**
  - [ ] Plan preview + user approval before run
  - [ ] Per-file diff with hunk-level accept/reject
  - [ ] Streaming progress + live token count
  - [ ] Background tasks + system notification
- [ ] **MCP integration (5.G)**
  - [ ] MCP client implementation per spec
  - [ ] User config MCP servers in settings (JSON)
  - [ ] Tool auto-discovery from connected servers
  - [ ] Resources + prompts from MCP servers
  - [ ] Tool available in chat context + agent loop
  - [ ] Recommended servers: Figma, Linear/Jira, Database, Browser
- [ ] **Stack-specific AI (5.H)**
  - [ ] TypeScript: inject TS diagnostics + type info
  - [ ] TypeScript: test generation aware of Vitest/Jest
  - [ ] TypeScript: type-aware refactoring
  - [ ] TypeScript: auto-fix on error suggestion
  - [ ] Flutter: screenshot → widget edit workflow
  - [ ] Flutter: widget tree from DTD as context
  - [ ] Flutter: pattern awareness (Riverpod, Bloc, freezed)
  - [ ] Flutter: null safety + asset path completion

## Blockers
- (depends on M5 — chat, RAG, providers)

## Notes
_(log สำคัญที่เกิดระหว่าง milestone นี้)_

## Decisions Made
- [ADR-007 — Protocols: LSP + DAP + MCP](../docs/adr/adr-007-protocols-lsp-dap-mcp.md)

---

## Claude Code Handoff Prompt

```
You are working on Milestone 6 (Agentic) of a Rust-based code editor.

Context:
- Spec doc: ./DeveloperDocumentation.md — sections 5.F (agent), 5.G (MCP), 5.H (stack-specific)
- Prerequisites: M4 (LLM client) + M5 (chat, RAG) complete
- Crates relevant: ai/agent/, ai/mcp/, ai/chat/, plus deep integration with editor-core, lsp-client, git, terminal
- Task file: tasks/milestone-6-agentic.md

Goals:
1. Reliable agent loop that can do multi-file edits safely
2. Approval flow + checkpoint/rollback that users actually trust
3. MCP client compliant with protocol spec
4. Flutter screenshot → widget edit workflow working end-to-end

Constraints:
- Default approval mode: `ask-each` (NOT auto)
- All command execution must go through path sandbox + allowlist/blocklist
- Checkpoint = filesystem snapshot of affected files; rollback = restore
- MCP: implement client spec from https://modelcontextprotocol.io/
- Stack-specific UX (5.H) requires DTD integration for Flutter widget tree

Read 5.F + 5.G + 5.H carefully. Design safety mechanisms BEFORE agent loop — getting agent that runs is easy, getting one that's safe is hard. Update task checkboxes as you go.
```
