---
adr: 009
title: Config Format — TOML
date: 2026-05-13
status: accepted
supersedes: null
---

# ADR-009: Config Format — TOML

## Context
Editor ต้องการ config format สำหรับ settings, keybindings, workspace file, launch/tasks, themes — ทั้งระดับ user, workspace, และ folder (spec §4.1.5). เลือก format ครั้งเดียวกระทบ:
- Parser library + binary size
- User authoring experience (comments, schema clarity)
- Compatibility กับ ecosystem ที่ user มาจาก (VSCode = JSON)
- Tooling (LSP สำหรับ config file editing ภายใน editor เอง)

## Decision
**ใช้ TOML** เป็น primary config format สำหรับทุก scope: `settings.toml`, `keybindings.toml`, `tasks.toml`, `launch.toml`, themes.

Workspace file (`.editor-workspace`) ก็ใช้ TOML — ตัด extension `.json` ที่เห็นใน spec §4.1.5 ตัวอย่างออก (ตัวอย่างนั้นใช้ JSON เฉพาะตอนยังไม่ตัดสินใจ).

## Alternatives Considered
- **JSON5**: comments + trailing commas, ใกล้ VSCode — แต่ ecosystem Rust ไม่นิยม, parser ใหญ่กว่า
- **JSON (no comments)**: universal แต่ user comment ไม่ได้ — ไม่เหมาะกับ config ที่ user แก้บ่อย
- **YAML**: human-readable แต่ indent-sensitive bugs + ambiguous types (norway problem)
- **RON**: Rust-native แต่ user ไม่คุ้น, learning curve

## Consequences
- ผลดี:
  - Consistent กับ `Cargo.toml` — user/contributor Rust คุ้นเคยอยู่แล้ว
  - Comments ทำได้ — สำคัญสำหรับ keybindings ที่ user override บ่อย
  - Parser `toml` / `toml_edit` ใน Rust ecosystem mature + เล็ก
  - Schema-friendly (sections + key-value ตรงไปตรงมา)
- ผลเสีย:
  - User ที่มาจาก VSCode ต้องเรียนรู้ syntax ใหม่ — ลดด้วย good defaults + docs + migration helper
  - Nested config ลึกๆ TOML อ่านลำบาก — design schema ให้ flat ที่สุด
  - Workspace file ไม่ JSON-compatible กับ VSCode's `.code-workspace` — ไม่เป็น drop-in migration
- Trade-offs: ยอมเสีย VSCode-config compatibility แลก authoring quality + ecosystem fit

## Implementation Notes
- Parser: `toml` crate (read) + `toml_edit` (preserve formatting/comments เวลา programmatic edit)
- Schema validation: hand-rolled per scope (avoid heavy serde-validation crate ใน v1)
- Settings precedence ตาม spec §4.1.5: Default → User → Workspace → Folder
- Hot reload on edit (spec §4.1.4) — watch file → re-parse → diff → apply

## References
- Spec doc §4.1.4 (Settings UI), §4.1.5 (Workspace-scoped resources)
- Resolves open question in [meta/open-questions.md](../../meta/open-questions.md) → "Config format"
- `toml` crate — https://docs.rs/toml
- `toml_edit` crate — https://docs.rs/toml_edit
