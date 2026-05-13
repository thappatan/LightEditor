# Code Editor — Developer Documentation

> **Project**: Custom Code Editor (TBD name)
> **Version**: 0.1.0 (Planning / Design Phase)
> **Last Updated**: 2026-05-13
> **Target Platforms**: macOS, Linux, Windows

---

## 1. Vision & Goals

### 1.1 Vision

สร้าง code editor ที่ **เบาที่สุด เร็วที่สุด** สำหรับการพัฒนา Node.js และ Flutter โดยเฉพาะ — ไม่ใช่ Electron, ไม่ใช่ webview, native ทั้งหมด พร้อม AI ที่เป็น first-class citizen ตั้งแต่วันแรก

### 1.2 Goals

| # | Goal | Why |
|---|------|-----|
| G1 | Performance — startup <100ms, keystroke <16ms | สาเหตุหลักที่หนีจาก VS Code |
| G2 | Memory — idle <100MB, GB-file capable | เครื่อง dev ทั่วไปไม่ใช่ workstation |
| G3 | Node.js + Flutter first-class | โฟกัสเชิงกลยุทธ์ — ไม่เป็น universal |
| G4 | AI-native architecture | AI ไม่ใช่ plugin แต่เป็น core |
| G5 | Thai input ครบสมบูรณ์ | IME + complex script rendering |

### 1.3 Non-Goals

- ไม่พยายามรองรับทุกภาษาตั้งแต่ v1 (JS/TS/Dart มาก่อน)
- ไม่ใช้ web technology ใน core (no Electron, no Tauri webview ส่วน editor)
- ไม่มี remote development ใน v1
- ไม่มี marketplace / extension store ใน v1
- ไม่ทำ collaborative editing (CRDT) ใน v1 — อาจ v2

---

## 2. Technology Stack

### 2.1 ภาษา: Rust

| เหตุผล | รายละเอียด |
|--------|-----------|
| No GC | Latency คาดเดาได้, ไม่มี pause |
| Memory safe | ลด class ของ bug ทั้งหมด |
| Performance | ใกล้เคียง C/C++ |
| Ecosystem | tree-sitter, ropey, wgpu, tokio พร้อมใช้ |
| ผลลัพธ์จริง | Zed, Lapce, Helix ทำสำเร็จมาแล้ว |

### 2.2 ตัดสินใจ: Custom UI Framework

สร้าง UI framework ของตัวเองแทนใช้ existing เพื่อ:
- ควบคุม render pipeline ทุก frame
- Optimize ให้ workload editor โดยเฉพาะ
- ไม่ติด constraint ของ framework คนอื่น
- เป็นจุดต่างของ product

### 2.3 Core Dependencies

| Layer | Crate | หน้าที่ |
|-------|-------|--------|
| Windowing | `winit` | Cross-platform window, event, IME, clipboard |
| GPU | `wgpu` | Vulkan/Metal/DX12/WebGPU abstraction |
| 2D vector | `vello` (optional) | High-quality vector graphics |
| Text shaping | `cosmic-text` + `swash` | Complex script (Thai, Arabic, CJK) |
| GPU text | `glyphon` | Text rendering บน wgpu |
| Text buffer | `ropey` | Rope data structure |
| Syntax | `tree-sitter` | Incremental parsing |
| Async runtime | `tokio` | Async I/O |
| LSP | `lsp-types` + custom client | Protocol types |
| Git | `git2` | libgit2 bindings |
| Terminal | `alacritty_terminal` | Embedded terminal emulator |
| Fuzzy match | `nucleo` | File/symbol picker |
| Code search | `grep` (ripgrep lib) | Find in files |
| HTTP/SSE | `reqwest` + `eventsource-stream` | LLM client |
| Embeddings | `fastembed` หรือ `ort` | Local embedding models |
| Vector store | `lancedb` | RAG codebase index |
| Keychain | `keyring` | Secret storage (API keys) |
| Clipboard | `arboard` | Cross-platform clipboard |

---

## 3. UI Framework Architecture

### 3.1 Layer Stack

```
┌─────────────────────────────────────┐
│  Editor Application Logic           │
├─────────────────────────────────────┤
│  Scene Graph (retained mode)        │
├─────────────────────────────────────┤
│  Layout Engine                      │
├─────────────────────────────────────┤
│  Text Pipeline    │  Shape/Image    │
│  (shape→raster)   │  primitives     │
├─────────────────────────────────────┤
│  GPU Renderer (wgpu)                │
├─────────────────────────────────────┤
│  Window & Input (winit)             │
└─────────────────────────────────────┘
```

### 3.2 Rendering Strategy

- **GPU-driven, retained-mode** scene graph
- **Dirty region tracking** — re-render เฉพาะ region ที่ change
- **Glyph atlas** บน GPU (cache rasterized glyphs)
- **Damage tracking** ระดับ widget
- Target: **120Hz** sustained บน hardware ปานกลาง

### 3.3 Text Pipeline (จุดวิกฤต)

ลำดับการประมวลผล text แต่ละ frame:

1. **Buffer** — `ropey` เก็บ text เป็น rope
2. **Shape** — `cosmic-text` + `swash` แปลง text → glyph cluster
3. **Layout** — คำนวณตำแหน่ง glyph แต่ละบรรทัด (พิจารณา wrap, tab)
4. **Rasterize** — `swash` หรือ GPU rasterize → texture
5. **Cache** — เก็บใน glyph atlas, key = (font, size, glyph_id, weight, hinting)
6. **Render** — `glyphon` วาดลง surface

### 3.4 Thai / Complex Script Considerations

> Thai เป็น test case ที่ดีที่สุดสำหรับความถูกต้องของ text pipeline เพราะมีหลายความท้าทาย

**ต้องจัดการให้ถูกต้อง**:
- **Cluster** — consonant + above/below vowel + tone mark ต้อง shape เป็นกลุ่ม
- **Caret positioning** — ตามขอบ grapheme cluster, ไม่ใช่ byte/codepoint
- **Selection** — ขยาย/หดตาม cluster ไม่ใช่ char
- **IME (Input Method Editor)** — Thai input ผ่าน OS IME หรือ keyboard layout ตรง
- **Preedit display** — แสดง composing text ก่อน commit
- **Line break** — รู้จัก Thai word boundary (อาจใช้ ICU หรือ dictionary-based)

**Testing matrix**: ไทย, จีน (CJK), อาหรับ (RTL), เกาหลี (Hangul shaping), Devanagari, emoji (ZWJ sequences), combined emoji + skin tone

### 3.5 Event System

- Event loop จาก `winit`
- Event bubbling ผ่าน widget tree (capture + bubble phases)
- Focus management + tab order
- Keybinding resolver (รองรับ modal mode optional แบบ Vim/Helix)
- Pointer event hit testing ผ่าน scene graph

---

## 4. Editor Core Features

## Phase 1 — Foundation

### 4.1.1 Text Editing

- [ ] Cursor (insert mode), selection (char/word/line/block)
- [ ] Multi-cursor + multi-selection (Ctrl+D, Alt+Click)
- [ ] Undo/redo (recommend tree-based undo)
- [ ] Clipboard (system + multiple register optional)
- [ ] Auto-indent + smart indent (จาก tree-sitter)
- [ ] Bracket auto-pair + auto-close
- [ ] Surround (Vim-style optional)
- [ ] Column selection (Alt+drag)
- [ ] Soft + hard line wrap
- [ ] Trim trailing whitespace on save
- [ ] EOL detection (LF / CRLF) + preserve

### 4.1.2 Navigation

- [ ] Tab bar + buffer list
- [ ] Split pane (horizontal/vertical, nested)
- [ ] File explorer sidebar
- [ ] Quick file picker (Ctrl+P, fuzzy match)
- [ ] Recent files / sessions
- [ ] Go to line (Ctrl+G)
- [ ] Jump to bracket
- [ ] Navigation history (back/forward)

### 4.1.3 Search

- [ ] Find/Replace in buffer (regex, case, whole word)
- [ ] Find in files (powered by `ripgrep` lib)
- [ ] Search result navigator
- [ ] Highlight all matches

### 4.1.4 Shell

- [ ] Command palette (Ctrl+Shift+P) — fuzzy
- [ ] Integrated terminal (multiple)
- [ ] Configurable keybindings (TOML)
- [ ] Settings UI + reload on edit
- [ ] Notification toaster

### 4.1.5 Workspace Management

> รองรับ **multi-root workspace** เป็น first-class — เปิดได้หลาย folder พร้อมกันใน window เดียว สำคัญมากสำหรับ stack เป้าหมาย (backend Node + frontend Flutter + shared package ใน workspace เดียว)

**Workspace types** (รองรับทั้งสามแบบ):

| Type | Use case |
|------|----------|
| Single folder | เปิด project เดียว แบบง่ายสุด |
| Multi-root (ad-hoc) | เพิ่ม folder เข้า session ปัจจุบัน |
| Workspace file | Save state เป็นไฟล์ เปิดซ้ำได้ |

**Operations**:

- [ ] Open Folder (Ctrl+K, Ctrl+O)
- [ ] **Add Folder to Workspace** (เพิ่ม folder ที่ 2, 3, ...)
- [ ] Remove Folder from Workspace
- [ ] Reorder folders ใน sidebar
- [ ] Save Workspace As… → `.editor-workspace.json` (หรือ TOML)
- [ ] Open Workspace from File → restore folders + settings ทั้งหมด
- [ ] Recent Workspaces list (Ctrl+R)
- [ ] Close Workspace (กลับสู่ welcome screen)
- [ ] Workspace trust prompt (เปิด folder ใหม่ → ถาม trust ก่อน execute task/extension)

**Workspace-scoped resources** (ทุกอย่างนี้ scope ต่อ workspace):

| Resource | Storage |
|----------|---------|
| Settings (override user) | `.editor/settings.json` |
| Keybindings override | `.editor/keybindings.json` |
| Tasks / launch configs | `.editor/launch.json`, `tasks.json` |
| AI rules / system prompt | `.editorrules` (root) |
| RAG index | per workspace fingerprint |
| Chat history | per workspace |
| LSP servers | spawn per workspace root |
| Search scope | all roots (เลือก scope ใน UI ได้) |
| Terminal cwd default | root picker เมื่อมีหลาย folder |
| Git repos | auto-detect per folder root |

**Settings hierarchy** (precedence ต่ำ → สูง):

```
Default → User → Workspace → Folder (multi-root)
```

**Workspace file format** (ตัวอย่าง):

```json
{
  "folders": [
    { "path": "./backend",  "name": "API" },
    { "path": "./mobile",   "name": "Flutter App" },
    { "path": "./shared",   "name": "Types" }
  ],
  "settings": {
    "editor.fontSize": 14,
    "ai.provider": "anthropic"
  },
  "launch": { "configurations": [ ... ] },
  "tasks":  { "tasks": [ ... ] }
}
```

**LSP behavior ใน multi-root**:

- 1 LSP server ต่อ (language × workspace folder) — เพราะ tsconfig/pubspec อาจต่างกัน
- ส่ง `workspace/didChangeWorkspaceFolders` เมื่อ add/remove
- รองรับ multi-root capability ของ LSP spec

**UX details**:

- File explorer แสดง folder roots เป็น top-level nodes แยกกัน
- Each root มี indicator (git branch, has errors, etc.)
- Quick file picker (Ctrl+P) ค้นหาข้าม root ทุก folder
- Find in Files toggle: current folder / all folders / specific folder

---

## Phase 2 — Code Intelligence (LSP)

จุดเปลี่ยน: implement LSP client **ครั้งเดียว → ได้ทุกภาษาฟรี**

### 4.2.1 LSP Features รองรับ

| Capability | Method |
|-----------|--------|
| Completion | `textDocument/completion` + `completionItem/resolve` |
| Hover | `textDocument/hover` |
| Definition | `textDocument/definition` + `declaration` + `typeDefinition` + `implementation` |
| References | `textDocument/references` |
| Document symbols | `textDocument/documentSymbol` |
| Workspace symbols | `workspace/symbol` |
| Rename | `textDocument/rename` + `prepareRename` |
| Code actions | `textDocument/codeAction` + `codeAction/resolve` |
| Formatting | `textDocument/formatting` + `rangeFormatting` + `onTypeFormatting` |
| Diagnostics | `textDocument/publishDiagnostics` |
| Signature help | `textDocument/signatureHelp` |
| Inlay hints | `textDocument/inlayHint` |
| Folding | `textDocument/foldingRange` |
| Highlights | `textDocument/documentHighlight` |
| Semantic tokens | `textDocument/semanticTokens` |
| Call hierarchy | `textDocument/prepareCallHierarchy` |

### 4.2.2 LSP Server Management

- Auto-launch server ตาม file type / workspace
- Crash detection + auto-restart (with backoff)
- Multiple servers per language (e.g. `vtsls` + `eslint`)
- Server status indicator + log viewer
- Custom server config ผ่าน settings

---

## Phase 3 — Debugging (DAP)

implement **DAP client** — เหมือน LSP แต่เล็กกว่า

### 4.3.1 Features

- Breakpoints (line, conditional, log, exception, function)
- Step over / into / out / continue / pause / reverse
- Variables panel + scopes + watch
- Call stack + thread switcher
- Debug console (REPL evaluate in context)
- Run/Debug configurations
  - Compatible กับ `.vscode/launch.json` (ลด friction migration)
- Multiple concurrent debug sessions

---

## Phase 4 — Node.js / TypeScript Workflow

### 4.4.1 Language Server

| Option | Pros | Cons |
|--------|------|------|
| `vtsls` *(แนะนำ)* | เร็ว, feature ครบ, รองรับ TS plugins | ใหม่กว่า |
| `typescript-language-server` | classic, stable | ช้ากว่าในไฟล์ใหญ่ |

**Required parallel servers**:
- ESLint (`vscode-eslint-language-server`)
- Optional: Tailwind CSS LSP, Prisma LSP

### 4.4.2 Project Awareness

- [ ] Auto-detect `package.json` → mark workspace
- [ ] Script runner sidebar (npm / pnpm / yarn / bun)
- [ ] Auto-detect package manager จาก lockfile
- [ ] Monorepo support (pnpm workspaces, npm workspaces, turborepo, nx)
- [ ] `.env` syntax + value resolution + `.env.local` precedence
- [ ] `tsconfig.json` path mapping aware
- [ ] Auto-import preference (relative vs path alias)

### 4.4.3 Debug

- DAP adapter: `vscode-js-debug`
- Auto-attach to spawned Node processes
- Source map support (for TS, esbuild, swc, etc.)
- Browser debug (Chrome/Edge protocol via vscode-js-debug)

### 4.4.4 Testing

- Detect Jest / Vitest / Mocha / node:test
- Gutter icons → run / debug per test / per file
- Test explorer sidebar
- Inline result indicators (pass/fail/skip)
- Watch mode integration
- Coverage display (optional)

### 4.4.5 Formatting / Linting

- Prettier integration (auto-detect config)
- ESLint integration (errors inline + code action fix)
- Format on save toggle
- Organize imports on save

---

## Phase 5 — Flutter / Dart Workflow

### 4.5.1 Language Server

- `dart language-server` — built-in กับ Dart SDK
- Auto-detect Dart SDK path (`which dart`, `FLUTTER_ROOT`)
- ไม่ต้อง config manual

### 4.5.2 Project Awareness

- [ ] `pubspec.yaml` parsing
- [ ] Dependency tree view sidebar
- [ ] Auto `pub get` หลังแก้ pubspec
- [ ] Multi-package workspace (melos)
- [ ] Lock file warning เมื่อ stale

### 4.5.3 Hot Reload — **Killer Feature**

> สำหรับ Flutter dev hot reload คือสิ่งที่ขาดไม่ได้ ต้องทำให้สมูธกว่า official tools

- Trigger keybinding:
  - `r` — hot reload
  - `R` — hot restart
- Toggle: auto-reload on save
- Visual indicator (badge ที่ status bar) แสดง reload status
- Show reload error inline + jump to error
- Reload history log

### 4.5.4 Device Management

- `flutter devices` integration → device picker ใน status bar
- `flutter emulators` → launcher
- Cold device list refresh
- Wireless debug devices
- Multi-target run (web + ios + android พร้อมกัน)

### 4.5.5 Debug

- DAP adapters: `dart debug_adapter`, `flutter debug_adapter`
- DevTools integration
  - Embedded ใน editor (webview) หรือ launch external browser
  - Quick switch to memory / network / performance / inspector
- Widget inspector via DTD (Dart Tooling Daemon) — advanced

### 4.5.6 Code Generation

- `build_runner` integration
  - Status indicator เมื่อ generation needed
  - One-click trigger (`dart run build_runner build`)
- Auto-detect freezed, json_serializable, etc.

---

## Phase 6 — Version Control (Git)

- File status indicator ใน explorer (M, A, D, U, ?)
- Gutter diff (added/modified/deleted lines)
- Inline blame (ลอย ๆ ใน line ปัจจุบัน)
- Stage / unstage hunk / line / file
- Commit UI พร้อม diff preview + amend
- Branch switcher + create / delete / merge
- Merge conflict resolution UI (3-way)
- Stash management
- Push / pull / fetch
- Remote management

**Implementation**: `git2` crate (libgit2) สำหรับ operations + shell ไป `git` CLI สำหรับ feature ที่ libgit2 ขาด

---

## Phase 7 — Polish Features

- [ ] Minimap (scroll overview)
- [ ] Breadcrumbs (path + symbol hierarchy)
- [ ] Bracket pair colorization
- [ ] Bracket pair matching highlight
- [ ] Indent guides
- [ ] Code folding (LSP + tree-sitter)
- [ ] Snippets (LSP snippet syntax)
- [ ] Markdown preview
- [ ] Theme system (TOML, hot reload)
- [ ] Sticky scroll (function header pinned)
- [ ] Word wrap indicator

---

## Phase 8 — Extensibility (v2)

| Option | Description | Verdict |
|--------|------------|---------|
| WASM plugins | Sandboxed, fast, language-agnostic (Zed model) | **แนะนำ** เมื่อทำ |
| Lua scripting | Embed `mlua`, flexible (Neovim model) | ทางเลือกสำหรับ config + macro |
| None | Keep core simple, ขยายผ่าน config + LSP | **v1 strategy** |

**คำแนะนำ**: ไม่ทำ plugin system ใน v1 → รอจน user pain point ชัด ค่อยออกแบบ extension API ที่ถูกต้อง

---

## 5. AI Integration

## Phase A — LLM Client Layer (Foundation)

### 5.A.1 Provider Abstraction

```rust
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn complete(
        &self,
        req: CompletionRequest,
    ) -> Result<BoxStream<'_, TokenChunk>>;

    async fn embed(
        &self,
        texts: Vec<String>,
    ) -> Result<Vec<Embedding>>;

    fn count_tokens(&self, text: &str) -> usize;
    fn capabilities(&self) -> ProviderCapabilities;
}
```

**Providers ที่ต้องรองรับ**:

| Provider | Use case |
|----------|----------|
| `AnthropicProvider` | Claude (default for chat + agent) |
| `OpenAIProvider` | GPT models |
| `GoogleProvider` | Gemini |
| `OpenRouterProvider` | Aggregator (model variety) |
| `OllamaProvider` | Local models |
| `OpenAICompatProvider` | LMStudio, vLLM, llama.cpp server |

### 5.A.2 Core Capabilities

- [ ] Streaming (SSE)
- [ ] Prompt caching (Anthropic ephemeral, OpenAI auto)
- [ ] Token counting + cost estimation
- [ ] BYOK — encrypted in OS keychain
- [ ] Request cancellation (Esc = stop)
- [ ] Retry with exponential backoff + jitter
- [ ] Rate limit awareness (respect headers)
- [ ] Telemetry off by default

---

## Phase B — Inline Completion (Tab / Ghost Text)

### 5.B.1 Behavior

- Ghost text หลัง cursor → Tab to accept
- Partial accept:
  - Word: Ctrl+→
  - Line: Ctrl+End
- Debounce: 100-200ms
- Cancel: on any keystroke that doesn't match prediction
- Auto-trigger: on typing pause (configurable)

### 5.B.2 Context Strategy

| Source | Priority |
|--------|----------|
| Code 100 lines before cursor | High |
| Code 50 lines after cursor | High |
| Top 3 recently focused tabs | Medium |
| Files from import graph | Medium |
| Recent edits this session | Low |
| Project metadata (lang, framework) | Low |

### 5.B.3 Model Choice

ใช้ **fast small model** เท่านั้น ไม่ใช่ frontier:

- Claude Haiku
- GPT-5 mini / nano
- Self-hosted: Qwen2.5-Coder-1.5B / 7B, DeepSeek-Coder

**Latency target**: P50 <200ms, P95 <500ms, P99 <1s

### 5.B.4 Optimization

- Speculative decoding (ถ้า self-host)
- Cache key by (prefix hash, suffix hash, context hash)
- Prefetch on cursor movement หยุด

---

## Phase C — Inline Edit (Cmd+K)

- Select range → keybinding → prompt → streaming diff
- Three actions: Apply / Reject / Modify prompt
- ใช้ frontier model ได้ (Sonnet, GPT-5)
- Context: selection + ~500 surrounding lines + active diagnostics + file imports
- Diff streaming UI — เห็น code เปลี่ยนสด ๆ ขณะ model generate

---

## Phase D — Chat Sidebar

### 5.D.1 Core

- Multi-turn conversation, history per workspace
- Markdown rendering + syntax highlighting ใน code block
- Code block → "Apply" button (patch ลงไฟล์จริง)
- Image paste (สำคัญ — Flutter UI screenshot)
- Conversation save / load / export
- Branching conversation (เหมือน ChatGPT)

### 5.D.2 @ Mentions

| Mention | Resolves to |
|---------|------------|
| `@file:path` | ทั้งไฟล์ |
| `@folder:path` | tree summary + key files |
| `@symbol:name` | symbol definition จาก LSP |
| `@selection` | text ที่ select อยู่ |
| `@diagnostics` | errors/warnings ทั้ง workspace |
| `@terminal` | terminal output ล่าสุด |
| `@git:diff` | uncommitted diff |
| `@git:log` | recent commit history |
| `@web:url` | fetch URL content |
| `@docs:lib` | library documentation |

---

## Phase E — Codebase RAG

### 5.E.1 Indexing Pipeline

1. Walk workspace (respect `.gitignore` + `.cursorignore`)
2. Chunk โดย symbol (function/class) ผ่าน tree-sitter
3. Embed ด้วย local model (BGE-small-en, Nomic-embed-text)
4. Store ใน `lancedb`
5. Incremental update on file save (debounced)

### 5.E.2 Hybrid Retrieval

| Strategy | Strength |
|----------|----------|
| Semantic (embedding) | Conceptual similarity |
| Symbol (LSP) | Exact reference |
| BM25 (keyword) | Literal match |
| Recency | Recently edited |

→ Re-rank ก่อนใส่ context (cross-encoder optional)

### 5.E.3 Privacy

- Local embeddings เป็น default
- Cloud embeddings = opt-in toggle (faster, better quality)
- Never upload code outside without explicit consent
- Indicator ชัดเจนเมื่อ data ออก network

---

## Phase F — Agentic Multi-File Edit

### 5.F.1 Agent Loop

```
plan → tool_call → result → reflect → next_action → ... → finish
```

### 5.F.2 Built-in Tools

| Tool | Purpose |
|------|---------|
| `read_file(path, range?)` | อ่านไฟล์ (พร้อม line range) |
| `edit_file(path, old, new)` | string replace edit |
| `create_file(path, content)` | สร้างไฟล์ใหม่ |
| `delete_file(path)` | ลบไฟล์ (need approval) |
| `list_directory(path)` | tree listing |
| `search_codebase(query)` | semantic + grep hybrid |
| `run_command(cmd, cwd?)` | sandboxed shell |
| `get_diagnostics(path?)` | current LSP errors |
| `run_tests(pattern?)` | execute test suite |
| `git_diff()` | uncommitted changes |
| `web_search(query)` | (optional) external |

### 5.F.3 Safety

| Mechanism | Description |
|-----------|------------|
| Approval mode | `auto` / `ask-each` / `read-only` |
| Command allowlist | Whitelist patterns |
| Command blocklist | `rm -rf /`, `:(){:|:&};:`, etc. |
| Path sandbox | จำกัด `cwd` ใน workspace |
| Checkpoint | Snapshot ก่อนแต่ละ action |
| Rollback | Restore checkpoint ใด ๆ |

### 5.F.4 UX

- Plan preview ก่อนเริ่ม + user approve
- Per-file diff พร้อม hunk-level accept/reject
- Streaming progress + live token count
- Pause / resume / cancel mid-run
- Persistent conversation thread หลัง finish

---

## Phase G — MCP Integration

> **Model Context Protocol** — protocol มาตรฐานสำหรับ tool integration

### 5.G.1 Features

- Implement MCP **client** spec
- User config MCP servers ใน settings (JSON)
- Tool auto-discovery จาก connected servers
- Resources + prompts จาก MCP servers
- Tool available ใน chat context + agent loop

### 5.G.2 Useful MCP Servers (สำหรับ stack เป้าหมาย)

- Figma MCP — ดึง design ตรงเข้า code (โดยเฉพาะ Flutter)
- Linear / Jira MCP — task context
- Database MCP — schema introspection
- Browser MCP — runtime inspection

---

## Phase H — Stack-Specific AI

### 5.H.1 Node.js / TypeScript

- Inject TS diagnostics + type info ก่อน AI request
- Test generation aware ของ Vitest/Jest config
- Type-aware refactoring (rename, extract function)
- Auto-fix on error (inline AI suggestion เมื่อมี TS error)
- Import resolution suggestion

### 5.H.2 Flutter / Dart

- **Screenshot → widget edit** workflow
  - Capture running app frame
  - Send + prompt "make button bigger"
  - Get widget code edit
  - Hot reload หลัง apply
- Widget tree (จาก DTD) เป็น context
- Pattern awareness: Riverpod, Bloc, Provider, freezed
- Dart null safety aware
- Asset path completion

---

## Phase I — AI UX Polish

- Streaming diff apply (เห็น code เปลี่ยนสด ๆ)
- Hunk-level accept/reject (granular)
- Background agent tasks + system notification
- Model picker per feature
  - Tab: Haiku / fast model
  - Inline edit: Sonnet / mid model
  - Agent: Opus / Sonnet
- Cost / token meter ใน status bar
- **Privacy kill switch** — toggle ปิด AI ทั้งหมด
- Conversation search
- Custom system prompt per workspace (`.editorrules`)

---

## 6. Implementation Roadmap

### Milestone 0 — Spike (1-2 สัปดาห์)

- [ ] Window + GPU rendering hello-world
- [ ] Text shaping Thai working
- [ ] wgpu/winit baseline latency benchmark

### Milestone 1 — Editable (4-6 สัปดาห์)

- [ ] Phase 1 complete
- [ ] เปิด/แก้/save ไฟล์ได้
- [ ] Multi-cursor, undo/redo
- [ ] Command palette

### Milestone 2 — Smart (4-6 สัปดาห์)

- [ ] Phase 2 (LSP) + tree-sitter
- [ ] TS + Dart language support
- [ ] Diagnostics, completion, hover, goto-def

### Milestone 3 — Developable (3-4 สัปดาห์)

- [ ] Phase 4 + 5 (Node.js + Flutter workflow)
- [ ] Integrated terminal
- [ ] npm scripts + flutter run + hot reload
- [ ] Test runner

### Milestone 4 — AI Baseline (3-4 สัปดาห์)

- [ ] Phase A + B (LLM client + inline completion)
- [ ] Daily driver ready

### Milestone 5 — AI Power (4-6 สัปดาห์)

- [ ] Phase C + D + E (inline edit + chat + RAG)

### Milestone 6 — Agentic (4-8 สัปดาห์)

- [ ] Phase F + G (agent + MCP)

### Milestone 7 — Production (ongoing)

- [ ] Phase 3 (debug)
- [ ] Phase 6 (git)
- [ ] Phase 7 (polish)
- [ ] Performance tuning
- [ ] Cross-platform QA

**Total estimate**: 6-9 เดือนสำหรับ MVP ใช้งานได้จริง, 12-18 เดือนสำหรับ feature-complete v1

---

## 7. Project Structure

```
editor/
├── Cargo.toml                  # workspace
├── crates/
│   ├── ui/                     # UI framework
│   │   ├── window/             # winit wrapper
│   │   ├── render/             # wgpu pipeline
│   │   ├── text/               # text shaping + rendering
│   │   ├── scene/              # scene graph
│   │   └── widgets/            # widget primitives
│   ├── buffer/                 # text buffer (ropey)
│   ├── editor-core/            # editing operations
│   ├── syntax/                 # tree-sitter
│   ├── lsp-client/             # LSP protocol
│   ├── dap-client/             # DAP protocol
│   ├── git/                    # git integration
│   ├── terminal/               # terminal emulator
│   ├── ai/
│   │   ├── providers/          # LLM providers
│   │   ├── completion/         # inline completion
│   │   ├── chat/
│   │   ├── agent/              # agent loop
│   │   ├── rag/                # codebase indexing
│   │   └── mcp/                # MCP client
│   ├── config/                 # settings + keymaps
│   ├── workspace/              # project management
│   ├── theme/                  # theme engine
│   └── app/                    # main binary
├── languages/                  # built-in language configs
│   ├── typescript/
│   ├── javascript/
│   └── dart/
├── docs/
└── assets/
```

---

## 8. Performance Targets

| Metric | Target | Hard Limit |
|--------|--------|-----------|
| Cold start | <100ms | <250ms |
| Open 1MB file | <50ms | <150ms |
| Open 1GB file | <2s | <5s |
| Keystroke latency (P99) | <16ms | <33ms |
| Scroll FPS | 120Hz | 60Hz min |
| Memory idle | <100MB | <200MB |
| Memory + 10 large files | <500MB | <1GB |
| LSP completion (cached) | <100ms | <300ms |
| AI inline suggestion (P50) | <200ms | <500ms |
| AI inline suggestion (P95) | <500ms | <1s |
| Find in workspace (10k files) | <500ms | <2s |
| RAG retrieval | <100ms | <300ms |

---

## 9. Key Technical Decisions (ADR)

### ADR-001 — Language: Rust

- **Decision**: Rust
- **Alternatives**: C++, Zig
- **Rationale**: Memory safety + ecosystem + community + proven by Zed/Helix

### ADR-002 — Custom UI Framework

- **Decision**: Build from scratch
- **Alternatives**: egui, iced, Floem, GPUI (Zed's), Xilem
- **Rationale**: Full control over render pipeline + optimization สำหรับ editor workload
- **Risk**: ใช้เวลามาก — mitigate ด้วย incremental approach

### ADR-003 — Rendering: GPU-Driven

- **Decision**: wgpu + glyphon
- **Alternatives**: skia-safe, vello, software rendering
- **Rationale**: Cross-platform, modern, future-proof (WebGPU compat)

### ADR-004 — Text Buffer: Ropey

- **Decision**: ropey
- **Alternatives**: crop, custom rope, piece table
- **Rationale**: Battle-tested ใน Helix, Zed; performant

### ADR-005 — No Web Technology

- **Decision**: Native only
- **Alternatives**: Electron, Tauri, webview
- **Rationale**: Performance + memory + cold start เป็น raison d'être

### ADR-006 — AI: Local Embeddings Default

- **Decision**: Local first, cloud opt-in
- **Rationale**: Privacy + cost + offline capability

### ADR-007 — Protocols: LSP + DAP + MCP

- **Decision**: Use existing protocols ทั้งหมด
- **Rationale**: Ecosystem reuse > NIH

### ADR-008 — No Plugin System ใน v1

- **Decision**: Defer
- **Rationale**: Premature abstraction; รอ pain point ชัด

### ADR-009 — Config Format: TOML

- **Decision**: TOML สำหรับ settings/keybindings/workspace file/themes
- **Alternatives**: JSON5, JSON, YAML, RON
- **Rationale**: Comments + Rust ecosystem fit + Cargo familiarity; trade off VSCode-config compatibility

### ADR-010 — No Modal Editing in v1

- **Decision**: Insert mode only (VSCode-style)
- **Alternatives**: Built-in Vim mode, pluggable optional layer
- **Rationale**: ลด complexity ใน `editor-core` + ลด test surface; defer to plugin system (v2) ถ้ามี demand

### ADR-011 — License: Apache 2.0

- **Decision**: Apache License 2.0
- **Alternatives**: MIT, GPL v3, AGPL, dual MIT/Apache-2.0, proprietary
- **Rationale**: Explicit patent grant + Rust ecosystem fit + permissive adoption; trade off copyleft protection

---

## 10. Open Questions

ต้องตัดสินใจก่อนเริ่ม implement หลัก:

- [x] ~~License — open source หรือ proprietary?~~ → **Resolved**: Apache 2.0 ([ADR-011](docs/adr/adr-011-license-apache-2-0.md))
- [ ] Distribution — GitHub release, ผ่าน package manager, หรือทั้งคู่?
- [ ] Plugin system — WASM, Lua, หรือไม่มี? (ตัดสินใจ defer ใน ADR-008)
- [x] ~~Config format — TOML (แนะนำ), JSON, JSON5?~~ → **Resolved**: TOML ([ADR-009](docs/adr/adr-009-config-format-toml.md))
- [x] ~~Modal editing — รองรับ Vim/Helix mode optional?~~ → **Resolved**: ไม่รองรับใน v1 ([ADR-010](docs/adr/adr-010-no-modal-editing.md))
- [ ] Theme format — VSCode-compatible หรือสร้างเอง?
- [ ] Telemetry — opt-in หรือไม่มีเลย?
- [ ] Settings sync (cross-machine, cross-device)?
- [ ] Update mechanism — auto-update, manual, package manager?
- [ ] CRDT collaborative editing — v2 หรือ never?
- [x] ~~Project / workspace concept — single root, multi-root, workspace file?~~ → **Resolved**: รองรับทั้ง 3 แบบ (single folder + multi-root ad-hoc + workspace file) ดู section 4.1.5

---

## 11. References

### Inspiration / Study

| Project | Lesson |
|---------|--------|
| **Zed** | GPUI architecture, performance-first |
| **Helix** | Modal editing, tree-sitter |
| **Lapce** | Rust IDE architecture, Floem |
| **Neovim** | Lua extensibility, LSP client |
| **Cursor** | AI-native UX, composer flow |
| **Cline / Claude Code** | Agent loop patterns |
| **Sublime Text** | UX polish, snappy feeling |

### Specs

- **LSP**: https://microsoft.github.io/language-server-protocol/
- **DAP**: https://microsoft.github.io/debug-adapter-protocol/
- **MCP**: https://modelcontextprotocol.io/
- **DTD (Dart Tooling Daemon)**: Flutter docs

### Key Crates Documentation

- `winit` — https://docs.rs/winit
- `wgpu` — https://docs.rs/wgpu
- `cosmic-text` — https://docs.rs/cosmic-text
- `ropey` — https://docs.rs/ropey
- `tree-sitter` — https://tree-sitter.github.io/

---

## 12. Glossary

| Term | Meaning |
|------|---------|
| LSP | Language Server Protocol |
| DAP | Debug Adapter Protocol |
| MCP | Model Context Protocol |
| RAG | Retrieval-Augmented Generation |
| BYOK | Bring Your Own Key |
| SSE | Server-Sent Events |
| IME | Input Method Editor |
| DTD | Dart Tooling Daemon |
| ADR | Architecture Decision Record |
| TBD | To Be Determined |

---

*End of document*
