# Editor — Rust Workspace

Rust workspace for the code editor itself. Mirrors structure in spec doc section 7.

> Project management (milestones, ADRs, research notes) lives in the **parent folder** — `../docs/`, `../tasks/`, `../meta/`. This `editor/` directory is **code only**.

## Quickstart for Claude Code

1. Read `../DeveloperDocumentation.md` (spec doc — source of truth)
2. Read the milestone file for current work: `../tasks/milestone-N-*.md`
3. Read relevant ADRs in `../docs/adr/`
4. Initialize crates as needed:

   ```bash
   cargo new --lib crates/buffer
   cargo new --lib crates/ui/window
   # ...
   cargo new crates/app          # binary
   ```

5. Uncomment the matching `members` line in `Cargo.toml`
6. Build: `cargo build`

## Crate Map (planned)

| Path | Purpose | Spec section |
|------|---------|--------------|
| `crates/ui/window` | winit wrapper, event loop, IME, clipboard | 3.5 |
| `crates/ui/render` | wgpu pipeline, scene rasterization | 3.2 |
| `crates/ui/text` | cosmic-text + swash + glyphon glue | 3.3 |
| `crates/ui/scene` | retained-mode scene graph + dirty tracking | 3.2 |
| `crates/ui/widgets` | primitive widgets (button, list, panel, editor surface) | 3 |
| `crates/buffer` | ropey-backed text buffer + selections | 4.1.1 |
| `crates/editor-core` | editing ops, multi-cursor, undo/redo, indent | 4.1.1 |
| `crates/syntax` | tree-sitter parsers + highlight queries | 4.2, 4.7 |
| `crates/lsp-client` | LSP client per spec | 4.2 |
| `crates/dap-client` | DAP client | 4.3 |
| `crates/git` | git2 wrapper + status/diff/blame | 4.6 |
| `crates/terminal` | alacritty_terminal embedded | 4.1.4 |
| `crates/ai/providers` | LlmProvider trait + Anthropic/OpenAI/Google/OR/Ollama/OAI-compat | 5.A |
| `crates/ai/completion` | inline completion / ghost text | 5.B |
| `crates/ai/chat` | chat sidebar + @ mentions | 5.D |
| `crates/ai/agent` | agent loop, tool dispatch, checkpoint | 5.F |
| `crates/ai/rag` | embedding + lancedb + hybrid retrieval | 5.E |
| `crates/ai/mcp` | MCP client | 5.G |
| `crates/config` | TOML settings + keybindings resolver | 4.1.4 |
| `crates/workspace` | single/multi-root/workspace-file management | 4.1.5 |
| `crates/theme` | TOML themes + hot reload | 4.7 |
| `crates/app` | main binary; wires everything | 7 |

## Languages directory

`languages/{typescript,javascript,dart}/` — built-in language configs (tree-sitter grammars, LSP defaults, file associations). Initialized by M2.
