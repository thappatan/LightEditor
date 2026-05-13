---
adr: 007
title: Protocols — LSP + DAP + MCP
date: 2026-05-13
status: accepted
supersedes: null
---

# ADR-007: Protocols — LSP + DAP + MCP

## Context
Code editor ต้อง integrate กับ:
- Language tooling (completion, diagnostics, refactor) → standardized by **LSP**
- Debugger → standardized by **DAP**
- AI tool integration → standardized by **MCP**

ทั้งสาม protocol เป็น JSON-RPC + capabilities exchange, mature, ecosystem กว้าง

## Decision
ใช้ **LSP + DAP + MCP** ทั้งหมดตาม spec; implement เป็น client (ไม่เขียน server เอง)

## Alternatives Considered
- **Roll own protocol** สำหรับ language/debug/tool integration: NIH (not invented here) — ตัดทันที, ขาด ecosystem
- **LSP/DAP only, skip MCP**: ตัด AI tool extensibility — competitor (Cursor, Claude Code) มี MCP จะตามไม่ทัน
- **Tree-sitter only (no LSP)** สำหรับ syntax: ดี for highlight แต่ขาด semantic (rename, goto def, type info)

## Consequences
- ผลดี:
  - Ecosystem reuse: free TS server (vtsls), Dart language-server, ESLint, etc.
  - Free debug adapters: vscode-js-debug, dart/flutter debug_adapter
  - MCP servers ecosystem โต (Figma, Linear, DB, browser)
  - User สามารถ config server เอง
- ผลเสีย:
  - JSON-RPC overhead (mitigate ด้วย batching + tokio)
  - LSP spec ใหญ่ — implement client ครบใช้เวลา
  - MCP spec ยัง evolving (early 2025-2026) — อาจมี breaking changes
- Trade-offs: ยอม implementation effort แลก ecosystem leverage มหาศาล

## References
- Spec doc section 4.2, 4.3, 5.G
- LSP — https://microsoft.github.io/language-server-protocol/
- DAP — https://microsoft.github.io/debug-adapter-protocol/
- MCP — https://modelcontextprotocol.io/
- lsp-types crate — https://docs.rs/lsp-types
