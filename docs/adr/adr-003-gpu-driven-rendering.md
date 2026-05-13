---
adr: 003
title: Rendering — GPU-Driven (wgpu + glyphon)
date: 2026-05-13
status: accepted
supersedes: null
---

# ADR-003: GPU-Driven Rendering

## Context
ต้อง render text + UI ที่ 120Hz, รองรับไฟล์ใหญ่ (scroll + selection ใหญ่), และ cross-platform (macOS Metal, Linux Vulkan, Windows DX12)

## Decision
ใช้ **wgpu** เป็น GPU abstraction + **glyphon** สำหรับ text rendering บน wgpu, มี **vello** เป็น optional path สำหรับ vector graphics

Strategy: GPU-driven retained-mode scene graph + dirty region tracking + glyph atlas บน GPU

## Alternatives Considered
- **skia-safe**: mature, used by Chrome/Flutter — แต่ binding to C++ heavy, build ยุ่ง
- **Software rendering** (tiny-skia, raqote): ง่าย portable แต่ไม่ 120Hz บนหน้าจอใหญ่
- **Direct Metal/Vulkan/DX12 ต่อ platform**: best perf แต่ต้องเขียน 3 backend → ต้นทุนสูง
- **Vello only**: high-quality vector แต่ overkill สำหรับ text-heavy editor + experimental

## Consequences
- ผลดี:
  - Cross-platform single API (Vulkan/Metal/DX12/WebGPU)
  - Future-proof (WebGPU compatibility ถ้าจะทำ web build)
  - wgpu mature เพียงพอ (used by Bevy, Zed indirectly via blade)
  - glyphon handles glyph atlas + caching แล้ว
- ผลเสีย:
  - wgpu abstraction มี overhead เล็กน้อยเทียบ native API
  - Driver bugs ระหว่าง backend ต้อง workaround
- Trade-offs: ยอม overhead เล็กน้อยแลก portability

## References
- Spec doc section 3.2 (Rendering Strategy)
- wgpu — https://docs.rs/wgpu
- glyphon — https://github.com/grovesNL/glyphon
- vello — https://github.com/linebender/vello
