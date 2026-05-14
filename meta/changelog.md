# Project Changelog

> Track project management changes (folder structure, milestones, ADRs)
> Code-level changelog will live in editor/CHANGELOG.md when implementation starts

## 2026-05-14 — Session 5 (M0 Spike)

- **Milestone 0 complete** — technology stack de-risked (ADR-002/003 validated)
- Initialized `crates/app` binary; spike implements winit window + wgpu 29 surface + glyphon 0.11/cosmic-text 0.18 multilingual text rendering
- Centralized M0 dependencies in `editor/Cargo.toml` `[workspace.dependencies]`; spec's draft pins were stale (wgpu 0.20→29, cosmic-text 0.12→0.18, glyphon 0.5→0.11)
- Benchmark: frame time ~8ms (½ of 16ms target), cold start 130-170ms warm / 923ms first-ever — over 100ms target, M1 follow-up needed
- CI fixes: `cache-bin: false` + `prefix-key` bump (stale macOS cargo-bin cache), `--no-tests=pass` (nextest on empty test suite)
- Findings: [docs/research/m0-spike-results.md](../docs/research/m0-spike-results.md)

## 2026-05-14 — Session 4 (License decision)

- Resolved license open question → Apache 2.0 ([ADR-011](../docs/adr/adr-011-license-apache-2-0.md)) ratifying the LICENSE file present at repo root
- Updated `editor/Cargo.toml` workspace license metadata
- First PR through the trunk-based workflow (branch `docs/license-decision`)

## 2026-05-13 — Session 3 (CI + release pipeline)

- Added `CONTRIBUTING.md` — Conventional Commits 1.0, trunk-based branching, PR-only `main`, branch protection rules
- Added `.github/workflows/ci.yml` — fmt / check / clippy / nextest matrix (ubuntu + macos) on PR
- Added `.github/workflows/release.yml` — tag-driven build (aarch64-apple-darwin, x86_64-apple-darwin, x86_64-unknown-linux-gnu) + GitHub Release with artifacts
- Added `.github/pull_request_template.md` — perf-impact section required for render/text/AI paths
- Added `editor/release.toml` — cargo-release config (workspace-shared version, no crates.io publish, push tag)
- Updated `CLAUDE.md` with git workflow section (no AI-attribution trailers in commits)

## 2026-05-13 — Session 2 (Claude Code setup)

- Init git repo + `.gitignore` (Rust + macOS) — pre-implementation phase now under VCS
- Added `.editorconfig` (LF, UTF-8, trim trailing whitespace, 4-space indent default)
- Added CLAUDE.md (project guide for Claude Code instances)
- Installed 15 Claude Code skills (Rust, wgpu, MCP, LSP, tree-sitter, RAG, GPUI patterns, TS, Flutter, a11y, design tokens)
- Expanded `.claude/settings.local.json` permissions for common cargo/git/search commands
- Resolved 2 open questions:
  - Config format → TOML ([ADR-009](../docs/adr/adr-009-config-format-toml.md))
  - Modal editing → not supported in v1 ([ADR-010](../docs/adr/adr-010-no-modal-editing.md))

## 2026-05-13 — Session 1 (Bootstrap)

- 🎬 **Project bootstrap**
- Created folder structure per Cowork project instructions:
  - `docs/` (adr, meetings, research, inspiration, backups)
  - `tasks/` (8 milestone files + weekly/)
  - `assets/` (logos, mockups, screenshots)
  - `meta/` (this file, open-questions, tools-cost)
  - `editor/` (Cargo workspace skeleton — crates/, languages/, assets/)
- Created 8 milestone task files: M0 (Spike) → M7 (Production), all `not_started`
- Logged 8 ADRs from spec doc section 9: ADR-001 → ADR-008
- Wired open-questions.md from spec doc section 10
- Spec doc remains at root as `DeveloperDocumentation.md` (not renamed to align with project instructions path `docs/code-editor-dev-doc.md` — pending user decision)
