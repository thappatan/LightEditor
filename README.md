<div align="center">

# LightEditor

**A native, GPU-rendered code editor built for speed — Node.js & Flutter first, AI-native from day one.**

[![CI](https://github.com/thappatan/LightEditor/actions/workflows/ci.yml/badge.svg)](https://github.com/thappatan/LightEditor/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](editor/Cargo.toml)
[![Status](https://img.shields.io/badge/status-early%20development-yellow.svg)](#project-status)
[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey.svg)](#building)

</div>

---

## Why

VS Code is heavy. For a daily Node.js + Flutter workflow, startup latency,
keystroke latency, and memory footprint add up. LightEditor is an attempt to
build the editor that workflow deserves — no Electron, no webview, no
JavaScript runtime in the core. Just native Rust, a GPU rendering pipeline,
and AI treated as a first-class part of the architecture rather than a plugin.

It is **not** trying to be a universal editor. It is deliberately focused on
the Node.js/TypeScript and Flutter/Dart stacks, with complete Thai text input
and shaping as a first-class correctness requirement.

## Goals

| # | Goal | Target |
|---|------|--------|
| G1 | Performance | startup <100 ms, keystroke <16 ms |
| G2 | Memory | idle <100 MB, GB-file capable |
| G3 | Node.js + Flutter first-class | not universal — focused by design |
| G4 | AI-native architecture | AI in the core, not a plugin |
| G5 | Complete Thai input | IME + complex-script shaping |

Full rationale, performance budgets, and architecture live in the
[Developer Documentation](DeveloperDocumentation.md).

## Project status

**Early development — pre-1.0, but already a working editor for its
target stacks.**

The project is built milestone by milestone. M0–M2 are complete and
M3 (the Node.js / Flutter workflow) is largely shipped.

| Milestone | Scope | Status |
|-----------|-------|--------|
| **M0** — Spike | winit + wgpu + Thai text shaping + latency baseline | ✅ Complete |
| **M1** — Editable | Buffer, multi-cursor, undo/redo, command palette | ✅ Complete |
| **M2** — Smart | LSP + tree-sitter; TypeScript + Dart | ✅ Complete |
| **M3** — Developable | Node/Flutter workflow, terminal, hot reload | 🟡 Largely shipped |
| M4 — AI Baseline | LLM client + inline completion | Planned |
| M5 — AI Power | Inline edit, chat, codebase RAG | Planned |
| M6 — Agentic | Agent loop + MCP | Planned |
| M7 — Production | Debugging (DAP), git, polish, cross-platform QA | Planned |

**Working today:** multi-cursor editing with cursor-restoring undo,
tree-sitter syntax for 17 languages, LSP diagnostics / hover /
go-to-definition / completion (Rust + TypeScript), a file-tree
sidebar (keyboard nav, fs-watcher refresh, drag-resize, git-status
decorations), git gutter, find-in-files, an embedded ANSI-colour
terminal, npm/pnpm/yarn/bun script running and Flutter
run / hot-reload / device-picker from the command palette,
TOML + VSCode-JSON theming with hot-reload, and
layout-independent keyboard shortcuts. See
[`editor/README.md`](editor/README.md) for the full feature list.

**Not yet built:** DAP debugging (M7), the AI stack (M4–M6),
multi-root workspace UX, and split panes.

M0 results — including the frame-time and cold-start baseline — are written
up in [`docs/research/m0-spike-results.md`](docs/research/m0-spike-results.md).

## Architecture at a glance

```
Editor logic → Scene graph (retained, dirty-tracked) → Layout
            → Text pipeline ┐
                            ├→ wgpu renderer → winit window/input
            → Shape/image  ─┘
```

The UI framework is **custom** — not egui/iced/GPUI — because controlling the
render pipeline frame by frame is the product's main differentiator
([ADR-002](docs/adr/adr-002-custom-ui-framework.md)). The text pipeline
(`ropey` → `cosmic-text` + `swash` → glyph atlas → `glyphon`) is the critical
path, and Thai is its correctness oracle.

### Core stack

| Layer | Crate |
|-------|-------|
| Windowing / input | [`winit`](https://docs.rs/winit) |
| GPU | [`wgpu`](https://docs.rs/wgpu) (Vulkan / Metal / DX12) |
| Text shaping | [`cosmic-text`](https://docs.rs/cosmic-text) + [`swash`](https://docs.rs/swash) |
| GPU text | [`glyphon`](https://docs.rs/glyphon) |
| Text buffer | [`ropey`](https://docs.rs/ropey) |
| Syntax | [`tree-sitter`](https://tree-sitter.github.io/) (17 grammars) |
| Git | [`git2`](https://docs.rs/git2) (libgit2) |
| Terminal | [`alacritty_terminal`](https://docs.rs/alacritty_terminal) (PTY + VT parser) |
| Protocols | LSP · DAP · MCP |

Every major technical decision is recorded as an
[Architecture Decision Record](docs/adr/).

## Repository layout

```
.
├── DeveloperDocumentation.md   # the spec — source of truth
├── README.md                   # you are here
├── CONTRIBUTING.md              # workflow, commit convention, release process
├── docs/
│   ├── adr/                     # ADR-001..011 — locked decisions
│   └── research/                # spike results, investigations
├── tasks/                       # milestone roadmap M0..M7
├── meta/                        # changelog, open questions
└── editor/                      # the Cargo workspace — all code
    ├── crates/                  # editor crates (see editor/README.md)
    └── languages/               # built-in language configs
```

## Building

### Prerequisites

- A stable **Rust** toolchain (workspace MSRV: see `rust-version` in
  [`editor/Cargo.toml`](editor/Cargo.toml))
- **macOS** — Xcode Command Line Tools (Metal backend)
- **Linux** — X11/Wayland development libraries (`libxkbcommon`, `libwayland`,
  X11 headers)
- **Windows** — a recent MSVC toolchain

### Build & run

```bash
cd editor
cargo build --workspace
cargo run --release --bin app                 # empty editor
cargo run --release --bin app -- path/to/file # open a file
cargo run --release --bin app -- path/to/dir  # open a folder as the workspace
```

### Development checks

These mirror what CI enforces on every pull request:

```bash
cd editor
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

## Documentation

| Document | What it is |
|----------|------------|
| [DeveloperDocumentation.md](DeveloperDocumentation.md) | Full spec — vision, architecture, features, performance budgets |
| [docs/adr/](docs/adr/) | Architecture Decision Records (locked decisions) |
| [tasks/](tasks/) | Milestone roadmap, M0 → M7 |
| [CONTRIBUTING.md](CONTRIBUTING.md) | Branching, Conventional Commits, release process |
| [editor/README.md](editor/README.md) | Cargo workspace crate map |
| [meta/changelog.md](meta/changelog.md) | Project-management changelog |

## Contributing

Contributions follow a trunk-based workflow with PR-only `main`,
[Conventional Commits](https://www.conventionalcommits.org/), and a green CI
gate. See [CONTRIBUTING.md](CONTRIBUTING.md) for the full process.

## License

Licensed under the [Apache License 2.0](LICENSE)
([ADR-011](docs/adr/adr-011-license-apache-2-0.md)).
