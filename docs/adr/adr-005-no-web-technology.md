---
adr: 005
title: No Web Technology in Core
date: 2026-05-13
status: accepted
supersedes: null
---

# ADR-005: No Web Technology in Core

## Context
ส่วนใหญ่ของ modern code editor สร้างบน web technology (Electron = VS Code, Atom; Tauri webview; CEF) เพราะ:
- ecosystem UI rich + familiar
- cross-platform ง่าย
- iteration เร็ว

แต่ web technology มี cost ที่ขัดกับ vision ของ project นี้ — startup ช้า, memory bloat, latency variable

## Decision
**ไม่ใช้ web technology ใน core editor** — native ทั้งหมด (Rust + wgpu + winit)
WebView อาจใช้ในส่วน auxiliary เช่น embedded DevTools (Flutter) หรือ markdown preview แต่ไม่ใช่ editor surface

## Alternatives Considered
- **Electron**: vast ecosystem แต่ memory 500MB+, startup 1s+, latency unpredictable
- **Tauri** (webview): smaller than Electron แต่ยังมี webview overhead + platform inconsistency
- **CEF embed**: similar trade-offs ถึง Electron
- **Hybrid (native + webview for some panels)**: complexity + 2 worlds to maintain

## Consequences
- ผลดี:
  - Startup <100ms achievable
  - Memory idle <100MB achievable
  - Latency predictable (no V8/Blink event loop competing)
  - Single language + binary
  - เป็น raison d'être ของ product
- ผลเสีย:
  - UI ecosystem = ต้องสร้างเอง (ADR-002)
  - Markdown preview, web docs viewer ต้อง integrate webview พิเศษ
  - Recruiting harder (Rust + GPU dev fewer than web dev)
- Trade-offs: ต้นทุน development สูงขึ้น แลก performance ที่เป็นจุดต่าง

## References
- Spec doc section 2.2, section 1.3 (Non-Goals)
- Performance targets section 8
