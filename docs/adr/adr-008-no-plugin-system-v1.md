---
adr: 008
title: No Plugin System in v1 (Defer)
date: 2026-05-13
status: accepted
supersedes: null
---

# ADR-008: No Plugin System in v1

## Context
Plugin system เป็น expectation ของ modern editor (VS Code marketplace, Neovim Lua, Zed WASM) แต่:
- Plugin API ที่ดีต้อง design after pain point ชัด — premature abstraction เสี่ยงผูก editor กับ shape ผิด
- Plugin system = surface area ใหญ่ (security, sandbox, versioning, distribution, store UI)
- v1 ต้อง prove core editor + AI ก่อน

## Decision
**ไม่ทำ plugin system ใน v1** — ขยายผ่าน config (TOML), LSP servers, MCP servers, themes แทน
รอจน v2 ค่อยตัดสินใจ plugin technology (WASM vs Lua vs custom)

## Alternatives Considered
- **WASM plugins (Zed-style)**: sandboxed, fast, language-agnostic — **แนะนำเมื่อทำ** (v2)
- **Lua scripting (Neovim-style)**: flexible, config + macro — **ทางเลือกที่ 2**
- **None ตลอด**: limit ceiling ของ product; eventually พลาด long-tail features

## Consequences
- ผลดี:
  - v1 focus กับ core differentiation (performance + AI)
  - ไม่ต้อง maintain plugin API surface, marketplace, security review
  - Design space เปิด — เก็บข้อมูลจาก user pain point ก่อนเลือก
- ผลเสีย:
  - User extensibility จำกัด — config + theme + LSP + MCP เท่านั้น
  - Migration จาก VS Code อาจขาด feature ที่ user เคยใช้ผ่าน extension
  - Competitor (Cursor, Zed) มี plugin system อยู่แล้ว
- Trade-offs: ยอม ceiling จำกัดใน v1 แลก time-to-MVP เร็ว + design ถูกในภายหลัง

## References
- Spec doc section 4.8 (Phase 8 — Extensibility), section 1.3 (Non-Goals)
- Zed extension model — https://zed.dev/docs/extensions
- Neovim Lua docs — https://neovim.io/doc/user/lua.html
