---
milestone: 5
title: AI Power — Inline Edit, Chat, RAG
target_duration: 4-6 weeks
started: TBD
completed: TBD
status: not_started
---

# Milestone 5 — AI Power

## Goal
ขยาย AI ให้ครอบคลุม inline edit (Cmd+K), chat sidebar (multi-turn + @mentions + image paste), และ codebase RAG (semantic + symbol + BM25 hybrid retrieval) ด้วย local embeddings เป็น default

อ้างอิง spec doc: section 5.C (Inline Edit), section 5.D (Chat), section 5.E (RAG)

## Tasks

- [ ] **Inline edit / Cmd+K (5.C)**
  - [ ] Selection range → keybind → prompt input UI
  - [ ] Streaming diff overlay (เห็น code เปลี่ยนสด ๆ)
  - [ ] Three actions: Apply / Reject / Modify
  - [ ] Context: selection + ~500 surrounding lines + diagnostics + imports
  - [ ] Use frontier model (Sonnet, GPT-5)
- [ ] **Chat sidebar (5.D)**
  - [ ] Multi-turn conversation, history per workspace
  - [ ] Markdown rendering + syntax highlight in code blocks
  - [ ] Apply button on code blocks → patch file
  - [ ] Image paste (Flutter UI screenshot critical)
  - [ ] Conversation save/load/export
  - [ ] Branching conversation tree
  - [ ] @ mentions resolver:
    - [ ] `@file:path` — full file
    - [ ] `@folder:path` — tree summary + key files
    - [ ] `@symbol:name` — LSP definition
    - [ ] `@selection` — current selection
    - [ ] `@diagnostics` — workspace errors/warnings
    - [ ] `@terminal` — recent terminal output
    - [ ] `@git:diff` — uncommitted diff
    - [ ] `@git:log` — recent commits
    - [ ] `@web:url` — fetch URL content
    - [ ] `@docs:lib` — library documentation
- [ ] **Codebase RAG (5.E)**
  - [ ] Indexing pipeline
    - [ ] Walk workspace, respect .gitignore + .cursorignore
    - [ ] Chunk by symbol (tree-sitter)
    - [ ] Embed locally (BGE-small-en or Nomic-embed via fastembed/ort)
    - [ ] Store in lancedb
    - [ ] Incremental update on file save (debounced)
  - [ ] Hybrid retrieval
    - [ ] Semantic (embedding)
    - [ ] Symbol (LSP workspace/symbol)
    - [ ] BM25 keyword (ripgrep + custom scorer)
    - [ ] Recency bias
    - [ ] Re-rank before context injection (cross-encoder optional)
  - [ ] Privacy controls
    - [ ] Local embeddings default
    - [ ] Cloud embeddings opt-in toggle
    - [ ] Indicator when data leaves device
  - [ ] Performance: retrieval <100ms (hard limit <300ms)

## Blockers
- (depends on M4 LLM client)

## Notes
_(log สำคัญที่เกิดระหว่าง milestone นี้)_

## Decisions Made
- [ADR-006 — Local Embeddings Default](../docs/adr/adr-006-local-embeddings-default.md)

---

## Claude Code Handoff Prompt

```
You are working on Milestone 5 (AI Power) of a Rust-based code editor.

Context:
- Spec doc: ./DeveloperDocumentation.md — sections 5.C, 5.D, 5.E
- Prerequisites: M2 (LSP), M4 (LLM client) complete
- Crates relevant: ai/chat/, ai/rag/, ai/providers/, syntax/ (tree-sitter chunking), lsp-client/
- Task file: tasks/milestone-5-ai-power.md

Goals:
1. Cmd+K inline edit with streaming diff (5.C)
2. Chat sidebar with @ mentions resolver (5.D) — 10 mention types
3. Codebase RAG with hybrid retrieval (5.E) — semantic + symbol + BM25 + recency
4. Privacy by default: local embeddings, clear indicator when data leaves

Constraints:
- Embeddings: fastembed or ort with BGE-small-en or Nomic-embed
- Vector store: lancedb
- Chunking: by symbol via tree-sitter (NOT by fixed token count)
- Image paste must work for chat (Flutter screenshot → widget edit workflow in M-? — H stack-specific)
- Re-index incrementally; full re-index only on workspace change

Read 5.C + 5.D + 5.E thoroughly. Design @ mention parser first; it touches LSP, git, terminal, web. Update task checkboxes as you go.
```
