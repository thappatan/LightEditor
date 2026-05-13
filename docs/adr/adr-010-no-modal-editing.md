---
adr: 010
title: No Modal Editing (Vim/Helix Mode) in v1
date: 2026-05-13
status: accepted
supersedes: null
---

# ADR-010: No Modal Editing in v1

## Context
Modal editing (Vim, Helix) มีฐาน user แข็งแกร่งและช่วย editing speed สำหรับ user ที่เชี่ยวชาญ. คำถาม: ต้องรองรับ modal mode เป็น optional layer ใน v1 หรือไม่?

ผลกระทบถ้าทำ:
- `editor-core` ต้อง design input handling ให้ pluggable (state machine ต่อ mode) ตั้งแต่ M1
- Keybinding resolver ซับซ้อนขึ้น (modal context)
- Cursor rendering ต้องรู้จัก mode (block vs line cursor)
- Test surface เพิ่มเป็น 2-3 เท่า

## Decision
**ไม่รองรับ modal editing ใน v1** — default = insert mode (VSCode-style) เท่านั้น.

`editor-core` ออกแบบ input handling แบบ flat (single-mode) — ไม่ลงทุน abstraction สำหรับ multi-mode จนกว่าจะมี user demand ชัด.

## Alternatives Considered
- **Built-in Vim mode**: รองรับใน core — ต้นทุน abstraction สูงตั้งแต่ M1, เพิ่ม maintenance burden ตลอดอายุ project
- **Optional layer (pluggable)**: ออกแบบ input handling ให้ pluggable แต่ default off — ยังต้องจ่ายต้นทุน abstraction + ความเสี่ยงว่า abstraction ผิด (modal Vim มี edge case ที่ pluggable abstraction มักจับไม่ครบ)
- **Defer plugin-based Vim emulation**: รอ plugin system (ADR-008 = v2) แล้วให้ Vim mode เป็น extension — สอดคล้อง strategic direction มากกว่า

## Consequences
- ผลดี:
  - `editor-core` ง่ายขึ้น — input flow เป็น linear, easier to optimize latency (G1 target keystroke <16ms)
  - Test surface เล็กลง — focus กับ correctness ของ insert mode (Thai IME, multi-cursor)
  - Time-to-MVP เร็วขึ้น
- ผลเสีย:
  - User Vim/Helix จะไม่ migrate มา — เสีย segment user power
  - Marketing ขาด "supports Vim mode" bullet point
  - User feedback อาจ push request features ตลอด v1 lifecycle
- Trade-offs: ยอมเสีย Vim user segment ใน v1 แลก editor-core ที่ leaner + ลด risk M1 จาก over-engineering

## When to Revisit
- มี user demand จริงจาก beta feedback (>20% ขอ)
- Plugin system พร้อม (v2) — implement เป็น extension แทน core feature
- มี contributor พร้อม own Vim emulation crate

## References
- Spec doc §3.5 (Event System), §4.1.1 (Text Editing)
- Resolves open question in [meta/open-questions.md](../../meta/open-questions.md) → "Modal editing"
- Helix architecture (reference) — https://docs.helix-editor.com/
