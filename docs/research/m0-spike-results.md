# M0 Spike — Results

> Milestone: [M0 — Spike](../../tasks/milestone-0-spike.md)
> Date: 2026-05-14
> Status: complete — stack de-risked, proceed to M1

## Goal recap

Prove that the core stack (Rust + winit + wgpu + cosmic-text + swash + glyphon)
can put correctly-shaped multilingual text on screen with acceptable latency,
before committing to build the real editor on it. This validates the
technical bets in ADR-002 (custom UI framework) and ADR-003 (GPU-driven
rendering).

## What was built

A single binary, `editor/crates/app` (`crates/app/src/main.rs`, ~330 lines):

- 1280x720 winit window via the 0.30 `ApplicationHandler` API
- wgpu 29 surface — `Bgra8UnormSrgb`, FIFO present mode, cleared each frame
- glyphon 0.11 + cosmic-text 0.18 + swash rendering a multilingual sample with
  `Shaping::Advanced`
- Per-frame timing: cold-start logged on first frame, then a rolling 1-second
  window logs fps + last frame time

Everything else — scene graph, retained-mode widgets, dirty-region tracking,
input handling — was intentionally **not** built. That is M1+ work.

## Test environment

| | |
|---|---|
| Machine | Apple M4, macOS 26.3.1 |
| Display | Built-in Liquid Retina XDR, 3024x1964, 120Hz |
| Build | `cargo build --release` (opt-level 3, LTO fat, stripped) |
| Binary size | 5.9 MB |

Linux + Windows were not benchmarked. CI (`ci.yml`) confirms the workspace
*compiles* on `ubuntu-latest` and `macos-latest`; runtime benchmarking on
those platforms is deferred.

## Results

### Frame time / throughput — PASS, comfortably

| Metric | Measured | Spec §8 target | Spec §8 hard limit |
|---|---|---|---|
| Steady-state frame time | ~8.0–8.6 ms | <16 ms | <33 ms |
| Sustained fps | 115–120 (vsync-capped) | 120 Hz | 60 Hz min |

Frame time sits at roughly **half** the 16 ms target, on a trivial workload
(one text buffer, full-frame redraw, no dirty-region culling). fps is pinned
to the 120 Hz display by FIFO present mode — the ~8 ms frame time means there
is meaningful headroom before the real editor's per-frame cost (syntax
highlight spans, multiple panes, cursor blink, minimap) eats into it.

### Cold start — OVER target, under hard limit (warm); fails hard limit (first-ever)

| Run | First frame presented |
|---|---|
| 1st ever (clean GPU/shader state) | 923 ms |
| 2nd (warm) | 168 ms |
| 3rd (warm) | 129 ms |

Warm cold-start lands at **130–170 ms** — over the 100 ms target but inside
the 250 ms hard limit. The first-ever launch (923 ms) blows past the hard
limit; that run pays one-time costs the OS/driver cache absorbs afterwards
(GPU pipeline creation, shader compilation).

The measurement starts inside `App::new()` (in `main`), so a small,
unmeasured prelude exists before it — the real number is slightly worse.

Dominant suspected costs, to be confirmed with a flamegraph in M1:

- `FontSystem::new()` enumerates and loads **all** system fonts eagerly. On a
  Mac with a large font book this is the prime suspect.
- First wgpu render pipeline + shader module compilation.
- Window + surface creation handshake.

P50/P95/P99 frame-time percentiles were not computed — the spike logs a
1-second rolling average instead. Proper percentile capture is an M1 task
once a benchmark harness (`criterion` / `cargo flamegraph`) is wired up.

### Text shaping correctness — PASS (visual)

The sample block exercises the full spec §3.4 testing matrix in one buffer:

```
สวัสดีชาวโลก  ·  你好,世界  ·  مرحبا بالعالم
안녕하세요 세계  ·  नमस्ते दुनिया
🇹🇭 🌏 🚀 👨‍👩‍👧‍👦
```

Verified visually on the M4:

- **Thai** — consonant + vowel + tone-mark clusters stack correctly; no
  broken/floating diacritics
- **CJK** — Han glyphs render at correct advance widths
- **Arabic** — RTL run reorders and joins correctly
- **Devanagari** — conjuncts and matras shape correctly
- **Emoji ZWJ** — the family sequence renders as a single glyph; flag emoji
  renders as a flag, not two letters

`Shaping::Advanced` is doing its job. This is a visual smoke test only —
automated cluster/caret correctness tests are M1 work, and they matter
because the editor's selection/caret logic must operate on grapheme
clusters (spec §3.4, §4.1.1).

## De-risking verdict

The stack is **viable**. ADR-002 and ADR-003 hold up:

- ✅ winit + wgpu + glyphon compose cleanly; no fighting the libraries
- ✅ Frame budget is comfortably met with room to spare
- ✅ Complex-script shaping is correct out of the box with `Shaping::Advanced`
- ⚠️ Cold start is the one real concern — over target warm, badly over on a
  cold GPU cache. Not a stack-viability problem (it's startup work, not
  per-frame work), but it needs deliberate attention, not incidental fixing.

## Follow-ups for M1+

1. **Profile cold start** — flamegraph `App::new()`; confirm whether
   `FontSystem::new()` is the bottleneck. If so, switch to lazy / on-demand
   font loading instead of eager enumeration.
2. **Pipeline/shader caching** — investigate `wgpu` pipeline caching to shrink
   the first-ever-launch penalty.
3. **Real frame-time percentiles** — wire up `criterion` benches + capture
   P50/P95/P99 under a realistic workload, not a 1-second average.
4. **Automated text correctness tests** — grapheme-cluster boundary, caret
   advance, and selection-extent assertions across the §3.4 matrix.
5. **Linux/Windows runtime benchmark** — CI proves compilation; actual
   latency numbers on those platforms are still unknown.
6. **Measure from process entry** — move the cold-start clock to the true
   start of `main` (or earlier) so the number isn't flattering.

## Version notes

The spec's draft dependency pins were stale. Actual versions used:

| Crate | Spec draft | Used | Reason |
|---|---|---|---|
| wgpu | 0.20 | 29 | glyphon 0.11 requires it |
| cosmic-text | 0.12 | 0.18 | matches glyphon 0.11 |
| glyphon | 0.5 | 0.11 | latest |
| winit | 0.30 | 0.30 | unchanged |

All pinned in `editor/Cargo.toml` `[workspace.dependencies]`.
