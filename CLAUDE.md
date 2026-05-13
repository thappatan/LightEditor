# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project state: pre-implementation

This repo is in the **planning/design phase**. There is no source code yet — `editor/crates/` contains empty directories pre-scaffolded for future crates. Workspace members in `editor/Cargo.toml` are **commented out** on purpose, so `cargo build` does not fail on empty folders.

Before writing code, always:
1. Read `DeveloperDocumentation.md` at repo root — this is the **source of truth** spec doc (sections referenced throughout this file).
2. Read the milestone file for the current work (`tasks/milestone-N-*.md`); each file ends with a "Claude Code Handoff Prompt" that frames its scope.
3. Read any relevant ADR in `docs/adr/` — these encode hard constraints, not suggestions.

## Repository layout

```
/                           project management (NOT code)
├── DeveloperDocumentation.md   spec doc — source of truth
├── docs/adr/                   ADR-001..008 — locked architectural decisions
├── tasks/milestone-N-*.md      8 milestones, M0 (Spike) → M7 (Production)
├── meta/                       changelog, open-questions, tooling cost
└── editor/                     ← all code lives here, isolated from project mgmt
    ├── Cargo.toml              workspace (members commented out by design)
    ├── crates/                 empty crate dirs (see Crate Map in editor/README.md)
    └── languages/              tree-sitter / LSP defaults per language
```

`editor/` is intentionally code-only; do not create planning files inside it. Project management belongs in `tasks/`, `docs/`, `meta/` at repo root.

## Initializing a crate

Crates must be created **one at a time, as the milestone calls for them** — not all at once. Workflow:

```bash
cargo new --lib editor/crates/<name>      # or --bin for editor/crates/app
# then uncomment the matching `members` line in editor/Cargo.toml
cargo build                               # from inside editor/
```

The Crate Map in `editor/README.md` maps each planned crate to its spec-doc section. Shared dependencies should be pinned in `[workspace.dependencies]` (also commented out in `editor/Cargo.toml`) and consumed via `dependencies.<name> = { workspace = true }` in each crate.

## Hard constraints from ADRs

These are decided. Do not relitigate without an explicit user request to revisit the ADR:

- **ADR-001 — Rust only.** No C++/Zig.
- **ADR-002 — Custom UI framework.** Do not introduce egui/iced/Floem/GPUI/Xilem.
- **ADR-003 — GPU-driven rendering via wgpu + glyphon.** No Skia, no software renderer.
- **ADR-004 — `ropey` for the text buffer.** No piece-table, no custom rope.
- **ADR-005 — No web technology in core.** No Electron, no Tauri webview for the editor surface. (A webview tab to display external DevTools/preview is a separate question.)
- **ADR-006 — Local embeddings by default.** Cloud embeddings are opt-in only; code never leaves the machine without explicit user consent.
- **ADR-007 — Use existing protocols: LSP, DAP, MCP.** No NIH protocols.
- **ADR-008 — No plugin system in v1.** Extensibility happens through config + LSP, not WASM/Lua, until v2.
- **ADR-009 — Config format is TOML.** All settings/keybindings/workspace/theme files use TOML; use `toml` for read, `toml_edit` for programmatic write that preserves formatting. Schema is hand-rolled per scope.
- **ADR-010 — No modal editing in v1.** `editor-core` input handling is single-mode (insert). Do not design a pluggable mode abstraction.

## Architecture: the parts that span multiple files

### Layer stack (spec §3.1)

```
Editor logic → Scene graph (retained, dirty-tracked) → Layout
            → Text pipeline ┐
                            ├→ wgpu renderer → winit window/input
            → Shape/image  ─┘
```

Re-rendering is **dirty-region driven** at widget granularity. Target is 120Hz sustained.

### Text pipeline (spec §3.3) — the critical path

`ropey` buffer → `cosmic-text` + `swash` shaping → layout (wrap/tab) → rasterize → glyph atlas on GPU (key: `font, size, glyph_id, weight, hinting`) → `glyphon` draws to the wgpu surface.

**Thai is a first-class correctness test case** (spec §3.4, §1.2 G5). Anything touching text — selection, caret, line break, IME — must operate on **grapheme clusters**, never byte or codepoint offsets. The test matrix is Thai, CJK, Arabic (RTL), Hangul, Devanagari, emoji ZWJ + skin tone. Get Thai shaping correct before adding features; M0's spike is exactly this.

### Workspace model (spec §4.1.5)

Multi-root is **first-class, not bolted on**. The target user opens backend + Flutter + shared package as three roots in one window. Implications that cut across crates:

- **LSP**: one server per `(language × folder root)` — `tsconfig`/`pubspec` differ per root. Send `workspace/didChangeWorkspaceFolders` on add/remove.
- **Settings precedence**: Default → User → Workspace → Folder.
- **Workspace-scoped resources**: settings, keybindings, launch/tasks, `.editorrules` (AI), RAG index, chat history, search scope, terminal cwd default, git repos — all per-workspace (or per-root where noted in the spec table).

When designing anything that holds state, ask: "does this need to be per-root, per-workspace, per-user, or per-installation?"

### AI architecture (spec §5)

Phases A–I in the spec map onto `crates/ai/{providers,completion,chat,agent,rag,mcp}`. Cross-cutting points:

- `LlmProvider` trait (spec §5.A.1) is the abstraction; concrete providers (Anthropic, OpenAI, Google, OpenRouter, Ollama, OpenAI-compat) live behind it. Streaming via SSE, BYOK in OS keychain (`keyring` crate), per-feature model picker.
- **Model tiering by feature** (spec §5.I): tab completion → fast small model (Haiku / GPT-5 mini / local Qwen-Coder); inline edit → mid (Sonnet); agent → frontier. Latency budget for inline completion is P50 <200ms / P95 <500ms — don't route it through a frontier model.
- **RAG retrieval is hybrid** (spec §5.E.2): semantic + LSP symbol + BM25 + recency, re-ranked. Not embedding-only.
- **Agent safety** (spec §5.F.3): approval mode (`auto` / `ask-each` / `read-only`), command allow/blocklist, path sandbox to workspace, per-step checkpoint with rollback.

## Performance budgets (spec §8)

These are the design constraints, not aspirations. Reach for the **target**; the **hard limit** is failure:

| Metric | Target / Hard limit |
|---|---|
| Cold start | <100ms / <250ms |
| Keystroke latency P99 | <16ms / <33ms |
| Memory idle | <100MB / <200MB |
| Open 1GB file | <2s / <5s |
| LSP completion (cached) | <100ms / <300ms |
| AI inline completion P50/P95 | <200ms / <500ms |
| Find in workspace (10k files) | <500ms / <2s |

If a design choice can't hit these, surface it before implementing. Performance is the project's reason to exist (spec §1.2 G1, G2) — it overrides ergonomics in trade-offs.

## Milestone roadmap (spec §6)

| M | Theme | Scope |
|---|---|---|
| 0 | Spike | winit + wgpu + cosmic-text Thai shaping + latency baseline |
| 1 | Editable | Phase 1: buffer, multi-cursor, undo, command palette |
| 2 | Smart | LSP + tree-sitter; TS + Dart |
| 3 | Developable | Node/Flutter workflow, terminal, hot reload |
| 4 | AI baseline | Phase A+B (LLM client, inline completion) |
| 5 | AI power | Phase C+D+E (inline edit, chat, RAG) |
| 6 | Agentic | Phase F+G (agent loop, MCP) |
| 7 | Production | DAP, git, polish, perf, cross-platform |

When picking up work, identify which milestone it belongs to and update that milestone's checkboxes — don't silently expand scope across milestones.

## Open decisions that affect implementation

A handful of decisions in `meta/open-questions.md` are still open and *will* shape code if you hit them before they're resolved. Ask the user rather than picking a default unilaterally:

- License (affects file headers, `Cargo.toml` `license` field — currently `"TBD"`)
- Theme format (VSCode-compat vs custom — affects `crates/theme`)
- Telemetry (lean: none in v1 — but confirm before adding any analytics)
- Update mechanism (affects packaging / code signing)

## Conventions specific to this repo

- The spec doc is bilingual (Thai + English). When updating it, preserve the existing language of each section rather than translating.
- ADRs live in `docs/adr/` as `adr-NNN-kebab-name.md`. When adding one, append it to the resolved list in `meta/open-questions.md` and the ADR section of `DeveloperDocumentation.md` §9.
- `meta/changelog.md` tracks project-management changes (folder structure, milestones, ADRs). Code-level changelog will live at `editor/CHANGELOG.md` once implementation starts — don't conflate the two.

## Git workflow

Read `CONTRIBUTING.md` for the full rules. The non-negotiables:

- **`main` is protected — never push directly.** Every change lands via pull request from a `feat/*` / `fix/*` / `chore/*` / `docs/*` branch.
- **Commit messages are Conventional Commits 1.0** — strict. Type, optional scope, imperative subject, ≤72 chars. Example: `feat(buffer): add multi-cursor selection`. No emoji. No `Co-Authored-By` trailer. No "Generated with…" footer. Write commits as a human engineer would.
- **PR title = squash-merge subject** — also Conventional Commits. The PR body uses `.github/pull_request_template.md`.
- **CI must be green before merge**: `fmt`, `check`, `clippy -D warnings`, `nextest run` (matrix: ubuntu + macos).
- **Releases use `cargo-release`** from `editor/`: `cargo release patch|minor|<version> --execute`. Config in `editor/release.toml`. Tag format `vX.Y.Z` triggers `.github/workflows/release.yml`.

When making a commit on this user's behalf, do not add any AI-attribution trailer. The convention is plain Conventional Commits with no co-author.
