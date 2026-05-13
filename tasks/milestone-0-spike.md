---
milestone: 0
title: Spike — Rendering & Text Foundation
target_duration: 1-2 weeks
started: TBD
completed: TBD
status: not_started
---

# Milestone 0 — Spike

## Goal
พิสูจน์ว่า technology stack หลัก (Rust + winit + wgpu + cosmic-text + glyphon) สามารถ render text บนหน้าจอได้พร้อมรองรับ Thai script ถูกต้อง และมี latency baseline ที่ยอมรับได้ — ก่อนลงทุนสร้าง editor จริง เพื่อ de-risk technical choices ที่อยู่ใน ADR-002/003

อ้างอิง spec doc: section 2.3 (Core Dependencies), section 3 (UI Framework Architecture), section 6 — Milestone 0

## Tasks

- [ ] Setup Cargo workspace + verify build บน macOS/Linux/Windows
- [ ] Window + GPU rendering hello-world
  - [ ] winit window initialization
  - [ ] wgpu surface + clear color
  - [ ] Event loop เบื้องต้น (close window works)
- [ ] Text shaping Thai working
  - [ ] Integrate cosmic-text + swash
  - [ ] Render "สวัสดีชาวโลก" ถูกต้องตาม cluster (no broken vowel/tone)
  - [ ] Wire glyphon เพื่อ raster + cache บน GPU
  - [ ] Test matrix: Thai, CJK, Arabic (RTL), emoji ZWJ
- [ ] Baseline latency benchmark
  - [ ] Measure frame time + report P50/P95/P99
  - [ ] Measure cold start time
  - [ ] เทียบกับ performance targets (section 8): startup <100ms, frame 16ms
- [ ] Document findings ใน `docs/research/m0-spike-results.md`

## Blockers
- (none)

## Notes
_(log สำคัญที่เกิดระหว่าง milestone นี้)_

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
