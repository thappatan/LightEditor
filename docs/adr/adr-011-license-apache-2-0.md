---
adr: 011
title: License — Apache 2.0
date: 2026-05-14
status: accepted
supersedes: null
---

# ADR-011: License — Apache 2.0

## Context
Project ต้องเลือก license ก่อน first external release. คำถามครอบคลุม: open-source หรือ proprietary, ถ้า OSS เลือก license ไหน. ผลกระทบ:
- Distribution + contribution model
- Commercial strategy ในอนาคต (dual-license, services around it)
- Compatibility กับ dependencies (Rust ecosystem ส่วนใหญ่ MIT/Apache-2.0/dual)
- File header + crate metadata

LICENSE file ที่ root ของ repository ถูก commit ไว้แล้วใน initial commit เป็น Apache 2.0 — ADR นี้ ratify การตัดสินใจดังกล่าวและบันทึก rationale.

## Decision
**Apache License 2.0** สำหรับทั้ง repository.

## Alternatives Considered
- **MIT**: สั้นกว่า + permissive เหมือนกัน แต่ขาด explicit patent grant — ลำบากกว่าในบริบท corporate adoption
- **GPL v3 (copyleft)**: ปกป้อง upstream contribution กลับมา แต่ block commercial fork + ขัดกับ Rust ecosystem norm
- **AGPL**: เหมาะ web service ไม่เหมาะ desktop editor — ไม่มี network use case ที่ต้องบังคับ source disclosure
- **Dual MIT/Apache-2.0**: Rust convention (`Cargo.toml` มัก license = "MIT OR Apache-2.0") — เพิ่ม flexibility แต่ซับซ้อนสำหรับ application repo (vs library crate)
- **Proprietary / source-available (BSL, FSL)**: เปิด commercial control แต่จำกัด community contribution + ขัดต่อ project ethos ที่อ้างอิง Zed/Helix

## Consequences
- ผลดี:
  - Patent grant explicit → ปลอดภัยกว่า MIT สำหรับ patent-aware contributors
  - Compatible กับ Rust ecosystem ส่วนใหญ่ (MIT, Apache-2.0, BSD)
  - Permissive → encourage adoption, forking, commercial use
  - Standard, well-understood by lawyers + companies
- ผลเสีย:
  - ไม่มี copyleft → forks เชิงพาณิชย์ไม่ต้อง contribute upstream กลับ
  - File-header attribution requirements (Apache §4) — เพิ่ม boilerplate ใน source files (mitigate ด้วย NOTICE file)
- Trade-offs: ยอมเสีย copyleft protection แลก ecosystem fit + adoption ease

## Implementation Notes
- `editor/Cargo.toml` workspace.package.license = "Apache-2.0"
- เพิ่ม `NOTICE` file ที่ root (ถ้ามี third-party attribution ในอนาคต)
- File headers ไม่บังคับใน v1 — Cargo.toml metadata เพียงพอ (สามารถ add ผ่าน tooling ภายหลัง)
- Dependency licenses ต้อง compatible — ตรวจด้วย `cargo deny` หรือ `cargo about` ใน CI ภายหลัง

## References
- LICENSE file ที่ root ของ repo (Apache 2.0 full text)
- Spec doc §10 (Open Questions) — License
- Resolves open question in [meta/open-questions.md](../../meta/open-questions.md) → "License"
- Apache 2.0 — https://www.apache.org/licenses/LICENSE-2.0
- Rust dual-license convention — https://rust-lang.github.io/api-guidelines/necessities.html#crate-and-its-dependencies-have-a-permissive-license-c-permissive
