---
adr: 001
title: Language — Rust
date: 2026-05-13
status: accepted
supersedes: null
---

# ADR-001: Language — Rust

## Context
ต้องเลือก systems language สำหรับสร้าง code editor ที่มี requirement หลัก: startup <100ms, keystroke <16ms, memory idle <100MB, รองรับไฟล์ระดับ GB ภาษานี้จะเป็น foundation ทั้ง project — เปลี่ยนภายหลังต้นทุนสูงมาก

ภาษาผู้สมัคร: Rust, C++, Zig

## Decision
ใช้ **Rust** เป็นภาษาหลักของ project

## Alternatives Considered
- **C++**: performance ดีมาก ecosystem กว้าง แต่ memory safety ต้องระวังเอง, build system เก่า, learning curve สำหรับ team
- **Zig**: ใหม่กว่า simpler than C++, manual memory mgmt, ecosystem ยังเล็ก ไม่มี crates สำคัญพร้อม (tree-sitter, ropey, wgpu, cosmic-text)
- **Go**: GC ทำให้ latency คาดเดาไม่ได้ — ไม่เหมาะ editor
- **Swift**: macOS-first, cross-platform support อ่อน

## Consequences
- ผลดี:
  - No GC → latency คาดเดาได้, ไม่มี pause spikes
  - Memory safety เลย class ของ bug ทั้งหมด (use-after-free, data race)
  - Ecosystem พร้อม: `tree-sitter`, `ropey`, `wgpu`, `cosmic-text`, `tokio`, `lsp-types`, `lancedb`
  - Proven: Zed, Helix, Lapce, ruff, biome ทำสำเร็จด้วย Rust
- ผลเสีย:
  - Compile time ช้า (mitigate ด้วย workspace + incremental + sccache)
  - Learning curve borrow checker
- Trade-offs: ยอม compile time แลก runtime + safety

## References
- Spec doc section 2.1
- Zed editor — https://github.com/zed-industries/zed
- Helix editor — https://github.com/helix-editor/helix
- Lapce — https://github.com/lapce/lapce
