---
milestone: 4
title: AI Baseline — LLM Client & Inline Completion
target_duration: 3-4 weeks
started: TBD
completed: TBD
status: not_started
---

# Milestone 4 — AI Baseline

## Goal
ติดตั้ง AI capabilities ขั้นพื้นฐาน: LLM provider abstraction (multi-provider) + inline completion (ghost text + Tab to accept) เพื่อให้ editor พร้อมเป็น daily driver ที่มี AI ตั้งแต่ก่อน chat/agent มาเต็มตัว

อ้างอิง spec doc: section 5.A (Phase A — LLM Client), section 5.B (Phase B — Inline Completion)

## Tasks

- [ ] **LLM client layer (5.A)**
  - [ ] `LlmProvider` trait (complete/embed/count_tokens/capabilities)
  - [ ] Providers:
    - [ ] AnthropicProvider
    - [ ] OpenAIProvider
    - [ ] GoogleProvider
    - [ ] OpenRouterProvider
    - [ ] OllamaProvider
    - [ ] OpenAICompatProvider (LMStudio/vLLM/llama.cpp)
  - [ ] Streaming via SSE (`eventsource-stream`)
  - [ ] Prompt caching (Anthropic ephemeral, OpenAI auto)
  - [ ] Token counting + cost estimation per request
  - [ ] BYOK — API key in OS keychain (via `keyring` crate)
  - [ ] Request cancellation (Esc = stop)
  - [ ] Retry with exponential backoff + jitter
  - [ ] Rate limit awareness (respect response headers)
  - [ ] Telemetry off by default
- [ ] **Inline completion / ghost text (5.B)**
  - [ ] Ghost text overlay UI in editor
  - [ ] Tab to accept full, Ctrl+→ word, Ctrl+End line
  - [ ] Debounce 100-200ms (configurable)
  - [ ] Cancel on non-matching keystroke
  - [ ] Auto-trigger toggle on typing pause
  - [ ] Context window strategy (5.B.2):
    - [ ] 100 lines before + 50 lines after
    - [ ] Top 3 recently focused tabs
    - [ ] Import-graph files (from LSP)
    - [ ] Recent edits log
    - [ ] Project metadata (lang, framework)
  - [ ] Cache key: (prefix hash, suffix hash, context hash)
  - [ ] Default fast model (Claude Haiku / GPT-5 mini / Qwen2.5-Coder-1.5B local)
  - [ ] Latency target P50 <200ms, P95 <500ms, P99 <1s
  - [ ] Prefetch on cursor pause

## Blockers
- (none — independent of agent / RAG; can run after M2)

## Notes
_(log สำคัญที่เกิดระหว่าง milestone นี้)_

## Decisions Made
- [ADR-006 — Local Embeddings Default](../docs/adr/adr-006-local-embeddings-default.md) *(applies to M5 mainly; cited here for AI architecture context)*

---

## Claude Code Handoff Prompt

```
You are working on Milestone 4 (AI Baseline) of a Rust-based code editor.

Context:
- Spec doc: ./DeveloperDocumentation.md — sections 5.A and 5.B
- Prerequisites: M1 + M2 complete (text editing + LSP for import graph)
- Crates relevant: ai/providers/, ai/completion/, lsp-client/ (for context), editor-core/
- Task file: tasks/milestone-4-ai-baseline.md

Goals:
1. Provider abstraction works for at least Anthropic + OpenAI + Ollama at launch
2. Inline completion feels instant (P50 <200ms) — match or beat Copilot
3. BYOK with keychain; never write API keys to disk in plaintext
4. Telemetry OFF by default

Constraints:
- Use Anthropic Messages API + OpenAI Chat Completions API + Ollama native API
- Use `reqwest` + `eventsource-stream` for SSE
- Use `keyring` crate for secrets
- Model defaults: chat/edit can be frontier; ghost text MUST be fast small model
- Context gathering: use LSP import graph (textDocument/definition + workspace symbol)

Read spec doc 5.A + 5.B before designing the provider trait. Plan cache strategy for ghost text — prefix/suffix hashing is critical. Update task checkboxes as you go.
```
