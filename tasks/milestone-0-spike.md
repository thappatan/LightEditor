---
milestone: 0
title: Spike — Rendering & Text Foundation
target_duration: 1-2 weeks
started: 2026-05-14
completed: 2026-05-14
status: complete
---

# Milestone 0 — Spike

## Goal
พิสูจน์ว่า technology stack หลัก (Rust + winit + wgpu + cosmic-text + glyphon) สามารถ render text บนหน้าจอได้พร้อมรองรับ Thai script ถูกต้อง และมี latency baseline ที่ยอมรับได้ — ก่อนลงทุนสร้าง editor จริง เพื่อ de-risk technical choices ที่อยู่ใน ADR-002/003

อ้างอิง spec doc: section 2.3 (Core Dependencies), section 3 (UI Framework Architecture), section 6 — Milestone 0

## Tasks

- [x] Setup Cargo workspace + verify build บน macOS/Linux/Windows
  - macOS: built + ran. Linux/Windows: CI confirms compile on ubuntu+macos; Windows ยังไม่ทดสอบ
- [x] Window + GPU rendering hello-world
  - [x] winit window initialization (1280x720, ApplicationHandler API)
  - [x] wgpu surface + clear color
  - [x] Event loop เบื้องต้น (close window works)
- [x] Text shaping Thai working
  - [x] Integrate cosmic-text + swash
  - [x] Render "สวัสดีชาวโลก" ถูกต้องตาม cluster (no broken vowel/tone)
  - [x] Wire glyphon เพื่อ raster + cache บน GPU
  - [x] Test matrix: Thai, CJK, Arabic (RTL), emoji ZWJ — + Hangul, Devanagari
- [x] Baseline latency benchmark
  - [~] Measure frame time — logged 1-second rolling avg (~8ms); proper P50/P95/P99 deferred to M1
  - [x] Measure cold start time — warm 130-170ms, first-ever 923ms
  - [x] เทียบกับ performance targets (section 8): frame ✅ well under 16ms; cold start ⚠️ over 100ms target
- [x] Document findings ใน `docs/research/m0-spike-results.md`

## Blockers
- (none)

## Notes
- Stack de-risked — ADR-002/003 hold up. Full results: [docs/research/m0-spike-results.md](../docs/research/m0-spike-results.md)
- Frame time ~8ms (½ of 16ms target) — comfortable headroom
- Cold start is the one concern: 130-170ms warm (over 100ms target, under 250ms hard limit), 923ms on a first-ever cold GPU cache. M1 follow-up: flamegraph `App::new()`, suspect eager `FontSystem::new()` font enumeration
- Spec's draft dependency pins were stale — actual: wgpu 29, cosmic-text 0.18, glyphon 0.11, winit 0.30 (centralized in `editor/Cargo.toml`)
- P50/P95/P99 percentile capture + automated grapheme-cluster correctness tests pushed to M1 (need a `criterion` harness)

## Decisions Made
- [ADR-001 — Rust](../docs/adr/adr-001-rust-language.md)
- [ADR-002 — Custom UI Framework](../docs/adr/adr-002-custom-ui-framework.md)
- [ADR-003 — GPU-Driven Rendering](../docs/adr/adr-003-gpu-driven-rendering.md)

---

## Claude Code Handoff Prompt

> Paste prompt นี้เข้า Claude Code เมื่อพร้อมเริ่ม milestone นี้

```
You are working on Milestone 0 (Spike) of a Rust-based code editor project.

Context:
- Spec doc: ./DeveloperDocumentation.md — read it before doing anything, especially sections 2 (Tech Stack), 3 (UI Framework Architecture), 6 (Roadmap M0), 8 (Performance Targets)
- ADRs that constrain this work: docs/adr/adr-001 through adr-005
- Task file: tasks/milestone-0-spike.md — update task checkboxes as you complete them
- Code lives under editor/ as a Cargo workspace; crates are pre-scaffolded but empty

Goals for this milestone:
1. Get a winit window with wgpu rendering a clear color
2. Render "สวัสดีชาวโลก" correctly using cosmic-text + swash + glyphon (Thai cluster shaping must be visually correct)
3. Measure baseline latency (cold start, frame time) and write findings to docs/research/m0-spike-results.md

Constraints:
- No web technology, no Electron, no webview (ADR-005)
- Must build on macOS first; Linux + Windows nice to have for spike
- Keep it minimal — this is a spike, not the real editor. Single binary or 1-2 crates is fine.

Begin by reading the spec doc and proposing a 3-5 step plan. Then implement.
```
