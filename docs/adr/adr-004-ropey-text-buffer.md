---
adr: 004
title: Text Buffer — Ropey
date: 2026-05-13
status: accepted
supersedes: null
---

# ADR-004: Text Buffer — Ropey

## Context
Text buffer เป็น data structure ที่ทุก operation editor ใช้ (insert, delete, undo, slice, line index) ต้อง:
- Insert/delete ใน middle ของไฟล์ใหญ่ที่ O(log n)
- Line index access O(log n)
- Memory efficient สำหรับไฟล์ GB
- Thread-safe sharing สำหรับ async LSP/highlight/search

## Decision
ใช้ **ropey** crate เป็น text buffer

## Alternatives Considered
- **crop**: rope ใหม่กว่า, similar API; less battle-tested
- **piece table**: classic editor data structure (VS Code uses); good for undo แต่ implementation ยุ่ง, line indexing ต้องเสริม
- **Gap buffer**: ง่าย แต่ scale แย่บนไฟล์ใหญ่
- **Custom rope**: control เต็ม แต่ลงทุนสูง

## Consequences
- ผลดี:
  - Battle-tested ใน Helix
  - O(log n) insert/delete/line-access
  - Grapheme-aware iteration (สำคัญสำหรับ Thai, emoji)
  - UTF-8 internal, byte/char/line index ครบ
  - Zero-copy slices
- ผลเสีย:
  - Allocation pattern อาจไม่ optimal สำหรับ append-heavy workload
  - Undo coupling ต้อง implement layer แยก (tree-based undo)
- Trade-offs: ใช้ proven solution แทน custom; รับ trade-off ของ ropey API

## References
- Spec doc section 2.3, ADR section
- ropey — https://docs.rs/ropey
- Helix usage — https://github.com/helix-editor/helix
