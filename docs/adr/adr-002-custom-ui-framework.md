---
adr: 002
title: Custom UI Framework
date: 2026-05-13
status: accepted
supersedes: null
---

# ADR-002: Custom UI Framework

## Context
Code editor ต้องการ render pipeline ที่ควบคุมได้ทุก frame เพื่อ:
- Sustain 120Hz บน hardware ปานกลาง
- Optimize ให้ editor workload (massive text, glyph atlas, dirty region)
- ไม่ติด constraint ของ framework ที่ออกแบบมาสำหรับ generic UI
- เป็นจุดต่างของ product

ทางเลือก UI framework Rust ปัจจุบัน: egui, iced, Floem, GPUI (Zed), Xilem, Druid

## Decision
สร้าง **custom UI framework** จาก scratch (built on top of wgpu + winit)

## Alternatives Considered
- **egui**: immediate mode, ง่าย แต่ไม่เหมาะ editor (re-layout ทุก frame, control flexibility จำกัด)
- **iced**: ELM-style, retained, OK แต่ optimize ยากสำหรับ glyph atlas + dirty region
- **Floem**: ใหม่ (Lapce), reactive — น่าสนใจแต่ยัง early
- **GPUI** (Zed): excellent for editors แต่ tightly coupled กับ Zed, license + reuse questionable
- **Xilem**: experimental, ยังไม่ stable
- **Slint / Dioxus**: UI-toolkit oriented; ไม่ใช่ low-level editor

## Consequences
- ผลดี:
  - ควบคุม render pipeline 100% (dirty region, damage tracking, glyph atlas)
  - Optimize editor workload โดยเฉพาะ (massive text + scroll + selection)
  - ไม่มี framework escape hatch
  - เป็น moat ของ product
- ผลเสีย:
  - ใช้เวลามาก (M0–M1 hard)
  - Reinvent wheel หลายส่วน (widget tree, layout, focus, hit testing)
  - Documentation + community = ตัวเอง
- Trade-offs: ลงทุนเวลาเริ่มต้นแลก long-term ceiling สูงและ differentiation
- Mitigation: incremental approach — build only widgets ที่ editor ต้องใช้จริง

## References
- Spec doc section 2.2, section 3 (UI Framework Architecture)
- GPUI architecture talks (Zed)
- Floem repo (inspiration) — https://github.com/lapce/floem
