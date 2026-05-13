# Open Questions

> รายการสิ่งที่ต้องตัดสินใจก่อน lock decision ใน ADR
> Sync จาก spec doc section 10 + เพิ่มเองได้ระหว่างทาง

## Active

- [ ] **Distribution** — GitHub release, package manager (brew/scoop/apt), หรือทั้งคู่?
  - Impact: update mechanism, code signing requirements
- [ ] **Theme format** — VSCode-compatible (reuse ecosystem) หรือ custom (TOML)?
  - Trade-off: ecosystem vs design control
- [ ] **Telemetry** — opt-in, opt-out, หรือ ไม่มีเลย?
  - Lean: ไม่มีเลยใน v1 (privacy-first); add opt-in for crash reports later
- [ ] **Settings sync** — cross-machine via cloud, file-based (git), หรือไม่มี?
  - Defer to v1.5+
- [ ] **Update mechanism** — auto-update (Sparkle/Squirrel), manual download, package manager?
- [ ] **CRDT collaborative editing** — v2 หรือ never?
  - Lean: v2 candidate; not core
- [ ] **Plugin system technology** — WASM, Lua, both, neither? (Resolved: defer in [ADR-008](../docs/adr/adr-008-no-plugin-system-v1.md))

## Resolved

- [x] **Project/workspace concept** — single root, multi-root, workspace file?
  - **Decision**: รองรับทั้ง 3 แบบ (single + multi-root ad-hoc + workspace file)
  - Reference: spec doc section 4.1.5
- [x] **Language**: Rust → [ADR-001](../docs/adr/adr-001-rust-language.md)
- [x] **UI framework**: Custom → [ADR-002](../docs/adr/adr-002-custom-ui-framework.md)
- [x] **Rendering**: wgpu + glyphon → [ADR-003](../docs/adr/adr-003-gpu-driven-rendering.md)
- [x] **Text buffer**: ropey → [ADR-004](../docs/adr/adr-004-ropey-text-buffer.md)
- [x] **Web tech in core**: None → [ADR-005](../docs/adr/adr-005-no-web-technology.md)
- [x] **AI embeddings**: Local default → [ADR-006](../docs/adr/adr-006-local-embeddings-default.md)
- [x] **Protocols**: LSP + DAP + MCP → [ADR-007](../docs/adr/adr-007-protocols-lsp-dap-mcp.md)
- [x] **Plugin system v1**: Defer → [ADR-008](../docs/adr/adr-008-no-plugin-system-v1.md)
- [x] **Config format**: TOML → [ADR-009](../docs/adr/adr-009-config-format-toml.md)
- [x] **Modal editing**: ไม่รองรับใน v1 → [ADR-010](../docs/adr/adr-010-no-modal-editing.md)
- [x] **License**: Apache 2.0 → [ADR-011](../docs/adr/adr-011-license-apache-2-0.md)
