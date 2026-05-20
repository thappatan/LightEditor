// Light Editor — application entry point.
//
// An editable text surface: keyboard and mouse input drive editor-core, the
// editor state is turned into a scene-graph each change, and the scene is
// rendered — selection highlights + caret quads via QuadRenderer, buffer text
// via TextStack. The viewport scrolls (wheel, caret-follow, drag-past-edge),
// multiple documents are stacked behind a tab strip, and everything is sized
// in physical pixels scaled by the window's DPI.
//
// Still missing for a "real" editor: split panes, a proper widget tree,
// find/replace, the file tree. Those are later M1 / M2 work.

mod document;
mod file_tree;
mod find;
mod find_in_files;
mod flutter;
mod git;
mod lsp;
mod palette;
mod scripts;
mod terminal;
mod terminal_palette;

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use document::Document;
use editor_config::{parse_hex_color, Settings, Theme};
use editor_core::{LineEnding, Position, Selection};
use editor_syntax::{Highlight, HighlightCategory, Language};
use editor_ui_render::{GpuContext, QuadRenderer};
use editor_ui_scene::{Color as SceneColor, Point, Rect, Scene, SceneNode};
use editor_ui_text::glyphon::{
    Attrs, Color, Family, FontSystem, Resolution, SwashCache, TextArea, TextBounds,
};
use editor_ui_text::{TextGpu, TextStack};
use file_tree::{ClickResult, FileTree, NodeKind};
use find::{FindBar, FindFocus};
use find_in_files::FindInFiles;
use lsp::{LspEvent, LspState};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use palette::{CommandEntry, CommandId, CommandPalette, BUILTIN_COMMAND_IDS};
use wgpu::{
    LoadOp, Operations, RenderPassColorAttachment, RenderPassDescriptor, StoreOp,
    TextureViewDescriptor,
};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
use winit::window::{Window, WindowId};

/// Cross-thread events posted into the winit event loop from background
/// helpers — file watcher, flash-clear timer, caret-blink timer.
#[derive(Debug, Clone)]
enum AppEvent {
    /// `settings.toml` (user or workspace) changed on disk; reload and reapply.
    SettingsChanged,
    /// `theme.toml` changed on disk; reload and reapply colors.
    ThemeChanged,
    /// `FLASH_DURATION` has elapsed since a transient status-bar message was
    /// set — clear it.
    ClearFlash,
    /// Caret-blink heartbeat. Fires every `CARET_BLINK_INTERVAL`; the
    /// handler decides whether to flip visibility (skips when recent
    /// interaction).
    CaretTick,
    /// Periodic poke to drain LSP server stdout queues. Without this,
    /// publishDiagnostics arriving during an idle window would not surface
    /// until the next user interaction.
    LspPoll,
    /// The embedded terminal's grid changed (output arrived, title
    /// updated, child exited). Triggers a redraw — the actual grid
    /// read happens during `render()` while holding the term lock.
    TerminalWakeup,
    /// Something under the workspace root touched the filesystem since
    /// the last reload. The watcher debounces a burst into a single
    /// event; the handler does a `reload_preserving_expansion()` and
    /// refreshes the shaped sidebar text.
    FileTreeChanged,
    /// `flutter devices --machine` finished in the background; the
    /// payload is the parsed device list (empty on any failure so
    /// the host's "no devices" branch fires).
    FlutterDevicesRefreshed(Vec<flutter::FlutterDevice>),
}

/// How long a "settings reloaded" flash stays on the status bar.
const FLASH_DURATION: Duration = Duration::from_millis(2000);

/// Half-cycle of the caret blink — solid for this long, then hidden for the
/// same. 530ms matches VSCode's smooth-blink cadence.
const CARET_BLINK_INTERVAL: Duration = Duration::from_millis(530);
/// Quiet period the file-tree watcher waits for before firing a reload.
/// `notify` emits dozens of events for a single git checkout or `npm
/// install`; the debounce coalesces them into one reload at the end.
const FILE_TREE_DEBOUNCE: Duration = Duration::from_millis(200);
/// One in-flight hover popup. `anchor_char` lets the popup follow the
/// caret through scrolls without reshaping. The shaped body lives on the
/// `hover_text` TextStack — this struct holds only the anchor.
struct HoverPopup {
    anchor_char: usize,
}

/// Completion popup state. Stays open while the user is typing inside
/// the word that triggered it; each keystroke refines `prefix` and
/// re-filters `items`. The shaped item list lives on the
/// `completion_text` TextStack.
struct CompletionPopup {
    /// The full item set the server returned. Server-side filtering is
    /// not always exhaustive (rust-analyzer in particular returns broad
    /// lists), so we filter locally as the user types.
    items: Vec<editor_lsp_client::lsp_types::CompletionItem>,
    /// Indices into [`items`] that match `prefix`, in display order
    /// (best match first per a simple case-insensitive prefix score).
    filtered: Vec<usize>,
    /// Caret char index when the popup opened — the start of the prefix.
    anchor_char: usize,
    /// What the user has typed since the anchor. The popup dismisses
    /// when this stops matching anything.
    prefix: String,
    /// Currently selected row, indexing into [`filtered`].
    selected: usize,
    /// First visible row when [`filtered.len()`] exceeds the visible
    /// row count — supports keyboard scrolling through long lists.
    scroll: usize,
}

impl CompletionPopup {
    /// Rebuild [`filtered`] from [`prefix`]. Empty prefix shows every
    /// item; otherwise items are kept when their `filter_text` (or
    /// `label`, the spec fallback) starts with the prefix
    /// case-insensitively. Resets selection + scroll.
    fn refilter(&mut self) {
        self.filtered.clear();
        if self.prefix.is_empty() {
            self.filtered.extend(0..self.items.len());
        } else {
            let needle = self.prefix.to_ascii_lowercase();
            for (i, item) in self.items.iter().enumerate() {
                let hay = item
                    .filter_text
                    .as_deref()
                    .unwrap_or(item.label.as_str())
                    .to_ascii_lowercase();
                if hay.starts_with(&needle) {
                    self.filtered.push(i);
                }
            }
            // Stable sort so prefix-matched items keep server ordering,
            // which usually reflects relevance.
        }
        self.selected = 0;
        self.scroll = 0;
    }

    /// Keep `selected` inside the visible window of `COMPLETION_MAX_ROWS`
    /// rows. Called after every up/down navigation.
    fn adjust_scroll(&mut self) {
        let visible = COMPLETION_MAX_ROWS;
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + visible {
            self.scroll = self.selected + 1 - visible;
        }
    }
}

/// Whether `c` belongs to an identifier — used to anchor the completion
/// popup and decide when the user has typed out of it.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Walk backward from `caret` (a `char` index) over identifier chars and
/// return the first index where the word starts. `text` is the buffer's
/// full content. Returns `caret` when the caret is not adjacent to a
/// word character — the popup then anchors with an empty prefix.
fn word_start_before(text: &str, caret: usize) -> usize {
    let chars: Vec<char> = text.chars().take(caret).collect();
    let mut i = chars.len();
    while i > 0 && is_word_char(chars[i - 1]) {
        i -= 1;
    }
    i
}

/// Per-phase render timing, captured as elapsed-since-frame-start at each
/// checkpoint. Logged only when a frame overruns the hard latency limit so
/// the steady-state log stays quiet.
///
/// Each field is `Duration::ZERO` until that checkpoint is reached, which
/// happens when the phase actually runs (e.g. `text_materialize` stays
/// zero when `text_dirty == false`).
#[derive(Default)]
struct FrameTimings {
    text_materialize: Duration,
    syntax_parse: Duration,
    build_spans: Duration,
    reshape: Duration,
    lsp_send: Duration,
    scene: Duration,
    quads_prepare: Duration,
    text_prepare: Duration,
}

impl FrameTimings {
    fn log(&self, total: Duration) {
        // Report as cumulative milliseconds at each checkpoint. The
        // reader can take deltas between adjacent entries to spot the
        // expensive phase. Zero entries are phases that didn't run.
        let ms = |d: Duration| d.as_secs_f32() * 1000.0;
        log::info!(
            "slow frame breakdown (cumulative ms): \
             text_materialize={:.1} syntax={:.1} spans={:.1} reshape={:.1} \
             lsp_send={:.1} scene={:.1} quads={:.1} text_prepare={:.1} total={:.1}",
            ms(self.text_materialize),
            ms(self.syntax_parse),
            ms(self.build_spans),
            ms(self.reshape),
            ms(self.lsp_send),
            ms(self.scene),
            ms(self.quads_prepare),
            ms(self.text_prepare),
            ms(total),
        );
    }
}

/// How often the LSP polling thread fires. 100 ms is fast enough that
/// diagnostics appear immediately to the eye, and slow enough that idle
/// CPU stays near zero.
const LSP_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// After any interaction (keystroke / click), the caret stays solid for at
/// least this long so it never blinks during active typing.
const CARET_BLINK_PAUSE: Duration = Duration::from_millis(500);

/// Time window inside which two successive clicks count as a double / triple
/// click. macOS default is ~500ms; matching it.
const MULTI_CLICK_INTERVAL: Duration = Duration::from_millis(500);
/// Pointer-movement budget between successive clicks before the count resets
/// to one, in physical pixels squared.
const MULTI_CLICK_DIST_SQ: f32 = 16.0;

/// Initial window size, in logical pixels.
const WINDOW_WIDTH: u32 = 1280;
const WINDOW_HEIGHT: u32 = 720;

/// Horizontal inset of the text block from the window's left edge, in logical
/// pixels. Multiplied by the window scale factor to get physical pixels.
const TEXT_INSET_DIP: f32 = 28.0;
/// Gap between the bottom of the tab strip and the top of the editor text,
/// in logical pixels.
const TEXT_TOP_GAP_DIP: f32 = 8.0;
/// Caret width, in logical pixels.
const CARET_WIDTH_DIP: f32 = 2.0;

/// Gutter dimensions, in logical pixels. The gutter sits between the
/// window's left edge and the editor text, showing right-aligned line
/// numbers. Fixed digit width for v1 — files over 9999 lines spill past
/// the reserved area but stay readable.
const GUTTER_DIGITS: usize = 4;
/// Margin between the window's left edge and the first digit, in logical pixels.
const GUTTER_PAD_LEFT_DIP: f32 = 12.0;
/// Gap between the gutter and the start of the editor text, in logical pixels.
const GUTTER_PAD_RIGHT_DIP: f32 = 12.0;
/// Monospace digit advance as a fraction of font size. Tuned for the
/// `Family::Monospace` fallback our platform stack picks — close enough that
/// the right-aligned numbers line up under each other.
const MONOSPACE_CHAR_FACTOR: f32 = 0.6;

/// Status-bar dimensions, in logical pixels. The bar sits at the very bottom
/// of the window and is opaque, so the editor viewport stops above it.
const STATUS_BAR_HEIGHT_DIP: f32 = 22.0;
/// Horizontal padding inside the status bar (left edge → text start), in
/// logical pixels.
const STATUS_PAD_X_DIP: f32 = 10.0;

/// Tab strip dimensions, in logical pixels.
const TAB_BAR_HEIGHT_DIP: f32 = 30.0;
const TAB_WIDTH_DIP: f32 = 180.0;
/// Padding inside one tab slot (left edge → start of label), in logical pixels.
const TAB_PAD_X_DIP: f32 = 10.0;
/// Width/height of the close-button "×" hit area at the right edge of each
/// tab slot, in logical pixels.
const TAB_CLOSE_W_DIP: f32 = 18.0;
/// Padding between the close button and the slot's right edge, in logical pixels.
const TAB_CLOSE_PAD_DIP: f32 = 6.0;
/// Approximate number of monospace characters that fit in one tab slot's
/// labelled area. Tuned at `font_size_pt = 16` and `TAB_WIDTH_DIP = 180`;
/// `Family::Monospace` puts ASCII at ~9.6 dip wide, so a 160 dip label
/// region (slot minus the two padding strips) holds ~16 chars. The figure is
/// used to pad each label so the next one lines up with the next slot — it is
/// approximate by design, complex-script fallback fonts won't always honour
/// the cell.
const TAB_LABEL_CHARS: usize = 16;

/// Command-palette overlay dimensions, in logical pixels.
const PALETTE_WIDTH_DIP: f32 = 600.0;
/// Distance from the window's top edge to the palette's top, in logical pixels.
const PALETTE_TOP_DIP: f32 = 80.0;
/// Padding inside the palette panel, in logical pixels.
const PALETTE_PAD_DIP: f32 = 12.0;
/// Maximum number of command rows rendered at once. Beyond this, the
/// palette scrolls under arrow-key navigation. Picked so the popup
/// stays comfortably above the editor's mid-screen on a typical
/// 1080p / 1440p window.
const PALETTE_VISIBLE_ROWS: usize = 12;

/// Find-bar overlay dimensions (single-row), in logical pixels.
const FIND_WIDTH_DIP: f32 = 480.0;
const FIND_TOP_DIP: f32 = 16.0;
const FIND_PAD_DIP: f32 = 8.0;

/// Hover-popup overlay (LSP) — wraps the server's reply at this width.
const HOVER_WIDTH_DIP: f32 = 480.0;
const HOVER_PAD_DIP: f32 = 8.0;
/// Maximum rendered height. Long hovers from rust-analyzer are clipped
/// rather than scrolled — scrolling is a follow-up.
const HOVER_MAX_HEIGHT_DIP: f32 = 240.0;

/// Completion-popup overlay (LSP) — list of suggestions hanging under
/// the caret. Width is fixed at this many dips; the list scrolls when
/// the filtered set exceeds the visible row count.
const COMPLETION_WIDTH_DIP: f32 = 360.0;
const COMPLETION_PAD_DIP: f32 = 6.0;
/// Maximum visible rows in the popup before it scrolls.
const COMPLETION_MAX_ROWS: usize = 10;

/// Width of the diagnostic indicator drawn in the gutter, in logical px.
const DIAG_DOT_DIP: f32 = 6.0;

/// File-tree sidebar (M3) — fixed width for v1, resizable drag-handle
/// is a follow-up. Per-depth indent is rendered as leading spaces in
/// the shaped text (see `refresh_file_tree_text`).
/// Initial width of the file-tree sidebar, in DIP. Drag-resize is
/// driven against this default; the user-chosen width lives on
/// `State::sidebar_width_dip` from then on.
const SIDEBAR_DEFAULT_WIDTH_DIP: f32 = 240.0;
/// Inclusive clamp range for the drag-resize handle.
const SIDEBAR_MIN_WIDTH_DIP: f32 = 120.0;
const SIDEBAR_MAX_WIDTH_DIP: f32 = 600.0;
/// Hit-test thickness for the drag-resize strip on the sidebar's
/// right edge — half on either side of the edge so the cursor can
/// grab from either direction. 14 dip ≈ 28 physical px on Retina,
/// wide enough to grab by feel without the cursor-icon polish.
const SIDEBAR_RESIZE_HANDLE_DIP: f32 = 14.0;
/// Width of the visible vertical divider between the sidebar and
/// the gutter, in DIP. Without this both regions paint the same
/// `gutter_bg` colour and the user can't see where to grab.
const SIDEBAR_DIVIDER_DIP: f32 = 1.0;
/// Wrap width for sidebar / chrome text stacks where we explicitly
/// don't want soft-wrap. Cosmic-text's `set_size` expects a width so
/// we hand it one a long filename won't reach; the visible bounds
/// rectangle still clips anything that runs off the panel.
/// Wrapping in the file tree would break the `row_index × line_h`
/// hit-test the click and keyboard-selection paths rely on.
const NO_WRAP_WIDTH_PX: f32 = 100_000.0;
const SIDEBAR_PAD_X_DIP: f32 = 8.0;

/// Embedded terminal pane (M3) — bottom-anchored pane, fixed height
/// for v1 with drag-to-resize a follow-up.
const TERMINAL_HEIGHT_DIP: f32 = 260.0;
/// Number of default rows the shell starts with — the renderer
/// re-derives true cell count from pixel height once cells are
/// measured, but the shell needs *some* answer at spawn time.
const TERMINAL_INITIAL_ROWS: u16 = 12;
const TERMINAL_INITIAL_COLS: u16 = 100;

/// Find-in-files panel (M3) — large centred overlay with the input
/// row at the top and matched lines below.
const FIND_FILES_WIDTH_DIP: f32 = 760.0;
const FIND_FILES_HEIGHT_DIP: f32 = 500.0;
const FIND_FILES_PAD_DIP: f32 = 12.0;
/// Result rows offset within the panel — input row + status row + a
/// blank divider row leaves results starting at row 3.
const FIND_FILES_HEADER_ROWS: usize = 3;

/// Subdirectory under the user's XDG config dir that holds settings.toml.
const CONFIG_SUBDIR: &str = "lighteditor";
const CONFIG_FILENAME: &str = "settings.toml";
/// Theme file name in the same directory.
const THEME_FILENAME: &str = "theme.toml";

// Bundled themes — TOML embedded at compile time so the palette picker
// works without the example files being on disk.
const BUNDLED_SOLARIZED_DARK: &str = include_str!("../../../examples/themes/solarized-dark.toml");
const BUNDLED_SOLARIZED_LIGHT: &str = include_str!("../../../examples/themes/solarized-light.toml");
const BUNDLED_MONOKAI: &str = include_str!("../../../examples/themes/monokai.toml");
const BUNDLED_GRUVBOX_DARK: &str = include_str!("../../../examples/themes/gruvbox-dark.toml");
const BUNDLED_NORD: &str = include_str!("../../../examples/themes/nord.toml");
const BUNDLED_TOKYO_NIGHT: &str = include_str!("../../../examples/themes/tokyo-night.toml");
/// Subdirectory under the current working directory that may hold a
/// workspace-scoped settings override (spec §4.1.5 — Workspace ranks above
/// User in the precedence Default → User → Workspace).
const WORKSPACE_CONFIG_SUBDIR: &str = ".lighteditor";

/// Whether the platform's "primary" modifier (Cmd on macOS, Ctrl on
/// Linux/Windows) is held. Used to gate shortcuts like Cmd-S.
fn is_cmd_or_ctrl(mods: ModifiersState) -> bool {
    mods.super_key() || mods.control_key()
}

/// `true` when `event` matches the QWERTY letter `target` (e.g. `'s'`
/// for Cmd-S). Checks the layout-independent
/// [`PhysicalKey`](winit::keyboard::PhysicalKey) first so non-Latin
/// keyboard layouts (Thai, Arabic, Russian, …) still fire shortcuts —
/// `logical_key` on those layouts reports the localised character
/// (`"ฆ"` instead of `"s"`) and an `eq_ignore_ascii_case` check would
/// miss. Falls back to the logical key for completeness on layouts /
/// platforms where `physical_key` reports `Unidentified`.
fn shortcut_letter(event: &KeyEvent, target: char) -> bool {
    shortcut_letter_of(event).is_some_and(|c| c.eq_ignore_ascii_case(&target.to_string()))
}

/// Lower-case form of the letter/digit/symbol the user pressed,
/// derived layout-independently from `event.physical_key`. Falls back
/// to `event.logical_key` for keys whose physical code we haven't
/// listed (e.g. punctuation specific to a national layout). Returns
/// `None` when neither path produces a single character. Used to
/// drive the Cmd-letter shortcut match on layouts where
/// `logical_key` would otherwise report the localised glyph.
fn shortcut_letter_of(event: &KeyEvent) -> Option<String> {
    if let PhysicalKey::Code(code) = event.physical_key {
        let ch: Option<char> = match code {
            KeyCode::KeyA => Some('a'),
            KeyCode::KeyB => Some('b'),
            KeyCode::KeyC => Some('c'),
            KeyCode::KeyD => Some('d'),
            KeyCode::KeyE => Some('e'),
            KeyCode::KeyF => Some('f'),
            KeyCode::KeyG => Some('g'),
            KeyCode::KeyH => Some('h'),
            KeyCode::KeyI => Some('i'),
            KeyCode::KeyJ => Some('j'),
            KeyCode::KeyK => Some('k'),
            KeyCode::KeyL => Some('l'),
            KeyCode::KeyM => Some('m'),
            KeyCode::KeyN => Some('n'),
            KeyCode::KeyO => Some('o'),
            KeyCode::KeyP => Some('p'),
            KeyCode::KeyQ => Some('q'),
            KeyCode::KeyR => Some('r'),
            KeyCode::KeyS => Some('s'),
            KeyCode::KeyT => Some('t'),
            KeyCode::KeyU => Some('u'),
            KeyCode::KeyV => Some('v'),
            KeyCode::KeyW => Some('w'),
            KeyCode::KeyX => Some('x'),
            KeyCode::KeyY => Some('y'),
            KeyCode::KeyZ => Some('z'),
            KeyCode::Digit0 => Some('0'),
            KeyCode::Digit1 => Some('1'),
            KeyCode::Digit2 => Some('2'),
            KeyCode::Digit3 => Some('3'),
            KeyCode::Digit4 => Some('4'),
            KeyCode::Digit5 => Some('5'),
            KeyCode::Digit6 => Some('6'),
            KeyCode::Digit7 => Some('7'),
            KeyCode::Digit8 => Some('8'),
            KeyCode::Digit9 => Some('9'),
            KeyCode::Slash => Some('/'),
            KeyCode::Period => Some('.'),
            _ => None,
        };
        if let Some(c) = ch {
            return Some(c.to_string());
        }
    }
    if let Key::Character(c) = &event.logical_key {
        return Some(c.to_lowercase());
    }
    None
}

/// A short language label for the status bar, guessed from `path`'s file
/// extension. Covers the languages M1 is targeting plus a few staples; an
/// unknown extension or no path falls through to "Plain Text". A proper
/// language registry is a later (syntax-highlighting) concern.
fn language_for(path: Option<&Path>) -> &'static str {
    let Some(ext) = path.and_then(|p| p.extension()).and_then(|e| e.to_str()) else {
        return "Plain Text";
    };
    match ext.to_ascii_lowercase().as_str() {
        "rs" => "Rust",
        "ts" | "tsx" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "dart" => "Dart",
        "py" => "Python",
        "go" => "Go",
        "c" | "h" => "C",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => "C++",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "swift" => "Swift",
        "rb" => "Ruby",
        "md" | "markdown" => "Markdown",
        "toml" => "TOML",
        "json" => "JSON",
        "yaml" | "yml" => "YAML",
        "html" | "htm" => "HTML",
        "css" => "CSS",
        "scss" | "sass" => "SCSS",
        "sh" | "bash" | "zsh" => "Shell",
        "sql" => "SQL",
        _ => "Plain Text",
    }
}

/// Line-comment prefix to use for a file path. Falls back to `//` for
/// anything outside the small whitelist (covers Rust / TS / Dart / JS / Java
/// / C / Go style files and a few `#` / `--` languages). A proper
/// language-config table is a follow-up alongside syntax highlighting.
fn comment_prefix_for(path: Option<&Path>) -> &'static str {
    let Some(ext) = path.and_then(|p| p.extension()).and_then(|e| e.to_str()) else {
        return "//";
    };
    match ext.to_ascii_lowercase().as_str() {
        "py" | "sh" | "bash" | "zsh" | "toml" | "yaml" | "yml" | "rb" => "#",
        "sql" | "hs" | "elm" | "lua" => "--",
        _ => "//",
    }
}

/// `"LF"` / `"CRLF"` for the buffer's dominant line-ending convention.
fn line_ending_label(le: LineEnding) -> &'static str {
    match le {
        LineEnding::Lf => "LF",
        LineEnding::CrLf => "CRLF",
    }
}

/// Find the next char-range in `haystack` where `needle` appears, starting
/// at or after `from`. Wraps to the beginning if nothing matches past
/// `from`. Returns `None` for an empty needle or when no occurrence exists.
fn find_next_occurrence(
    haystack: &str,
    needle: &str,
    from: usize,
) -> Option<std::ops::Range<usize>> {
    if needle.is_empty() {
        return None;
    }
    let hay: Vec<char> = haystack.chars().collect();
    let nee: Vec<char> = needle.chars().collect();
    if nee.len() > hay.len() {
        return None;
    }
    let from = from.min(hay.len());
    let mut i = from;
    while i + nee.len() <= hay.len() {
        if hay[i..i + nee.len()] == nee[..] {
            return Some(i..i + nee.len());
        }
        i += 1;
    }
    let mut j = 0;
    while j + nee.len() <= from {
        if hay[j..j + nee.len()] == nee[..] {
            return Some(j..j + nee.len());
        }
        j += 1;
    }
    None
}

/// Resolve a theme color string (e.g. `"#cd82e9ff"`) to a quad-renderer
/// `SceneColor`. Falls back to neutral gray when the string is malformed
/// rather than blowing up on a typo'd hex literal.
fn quad_color(hex: &str) -> SceneColor {
    let [r, g, b, a] = parse_hex_color(hex).unwrap_or([0x80, 0x80, 0x88, 0xff]);
    SceneColor::rgba(r, g, b, a)
}

/// Compute a scrollbar-thumb rect for a scrolling popup list, or
/// `None` when every item fits (no scroll, no bar). `track` is the
/// vertical band the rows occupy; `total` / `visible` / `scroll` are
/// item counts and the current top-row offset. The thumb is a thin
/// bar pinned to the track's right edge, its height proportional to
/// the visible fraction (floored so it stays grabbable), its position
/// proportional to how far the list is scrolled.
fn scrollbar_thumb(
    track: Rect,
    total: usize,
    visible: usize,
    scroll: usize,
    scale: f32,
) -> Option<Rect> {
    if total <= visible || visible == 0 {
        return None;
    }
    let width = (3.0 * scale).max(2.0);
    let x = track.max_x() - width;
    let track_h = track.size.height;
    let thumb_h = (track_h * visible as f32 / total as f32).max(16.0 * scale);
    let max_scroll = (total - visible) as f32;
    let frac = if max_scroll > 0.0 {
        (scroll as f32 / max_scroll).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let y = track.min_y() + frac * (track_h - thumb_h);
    Some(Rect::new(x, y, width, thumb_h))
}

/// Resolve a theme color string for cosmic-text. Alpha is dropped — glyphs
/// are either drawn or not; per-glyph transparency isn't useful at the
/// text layer.
fn text_color(hex: &str) -> Color {
    let [r, g, b, _] = parse_hex_color(hex).unwrap_or([0xee, 0xee, 0xee, 0xff]);
    Color::rgb(r, g, b)
}

/// Resolve a theme color string into the `wgpu::Color` the surface
/// clear uses. The surface format is `Bgra8UnormSrgb`, so wgpu treats
/// the values handed to `LoadOp::Clear` as *linear* and gamma-encodes
/// them to sRGB on its way to the framebuffer. Hex codes are sRGB
/// bytes by convention, so we apply the sRGB EOTF before handing them
/// off — otherwise `#1f1f1f` (intended ≈ 31/255 grey) lands closer to
/// 110/255 visible grey on screen. Same fix
/// [`Color::to_f32_array`](editor_ui_scene::Color::to_f32_array) does
/// for the quad path.
fn clear_color(hex: &str) -> wgpu::Color {
    let [r, g, b, _] = parse_hex_color(hex).unwrap_or([5, 5, 8, 0xff]);
    wgpu::Color {
        r: editor_ui_scene::srgb_byte_to_linear(r) as f64,
        g: editor_ui_scene::srgb_byte_to_linear(g) as f64,
        b: editor_ui_scene::srgb_byte_to_linear(b) as f64,
        a: 1.0,
    }
}

/// Theme color for a syntax category. Reads from the active document's
/// theme so user-overridden colors land in the right token type.
fn syntax_color(theme: &editor_config::SyntaxTheme, category: HighlightCategory) -> Color {
    let hex = match category {
        HighlightCategory::Keyword => &theme.keyword,
        HighlightCategory::StringLit => &theme.string,
        HighlightCategory::Number => &theme.number,
        HighlightCategory::Comment => &theme.comment,
        HighlightCategory::Type => &theme.type_,
        HighlightCategory::Function => &theme.function,
        HighlightCategory::Punctuation => &theme.punctuation,
    };
    text_color(hex)
}

/// Build `(slice, attrs)` spans from `text` and `highlights` for
/// `TextStack::set_content_rich`. The highlights must be in char-range
/// order and non-overlapping; the syntax crate's leaf-only emission
/// guarantees this. Concatenating every span exactly reconstructs `text`.
fn build_highlight_spans<'a>(
    text: &'a str,
    highlights: &'a [Highlight],
    default_color: Color,
    syntax: &editor_config::SyntaxTheme,
) -> Vec<(&'a str, Attrs<'a>)> {
    let total_chars = text.chars().count();
    // Walk char_indices once to get a char→byte map: position[char_idx] = byte.
    let mut char_to_byte: Vec<usize> = Vec::with_capacity(total_chars + 1);
    for (b, _) in text.char_indices() {
        char_to_byte.push(b);
    }
    char_to_byte.push(text.len());

    let default = || Attrs::new().color(default_color);
    let mut spans: Vec<(&str, Attrs)> = Vec::with_capacity(highlights.len() * 2 + 1);
    let mut cursor: usize = 0;
    for hl in highlights {
        let s = hl.range.start.min(total_chars);
        let e = hl.range.end.min(total_chars);
        if s < cursor || s >= e {
            continue;
        }
        if s > cursor {
            spans.push((&text[char_to_byte[cursor]..char_to_byte[s]], default()));
        }
        spans.push((
            &text[char_to_byte[s]..char_to_byte[e]],
            Attrs::new().color(syntax_color(syntax, hl.category)),
        ));
        cursor = e;
    }
    if cursor < total_chars {
        spans.push((&text[char_to_byte[cursor]..], default()));
    }
    spans
}

/// Build a copy of `text` with every space replaced by a middle dot and
/// every tab by a rightwards-arrow. Both replacements are single chars, so
/// char indices in the buffer line up exactly with char indices in the
/// shaped display text — selections and caret positions still work.
fn substitute_whitespace(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            ' ' => '·',
            '\t' => '→',
            other => other,
        })
        .collect()
}

/// Write `text` to the OS clipboard. Logs and swallows errors — clipboard
/// access can fail on headless Linux or when another app holds the
/// pasteboard lock; better to no-op than abort the editor.
fn clipboard_set(text: &str) {
    match arboard::Clipboard::new() {
        Ok(mut cb) => {
            if let Err(e) = cb.set_text(text) {
                log::warn!("clipboard write failed: {e}");
            }
        }
        Err(e) => log::warn!("clipboard unavailable: {e}"),
    }
}

/// Read the OS clipboard. Returns `None` when the clipboard is
/// unavailable or doesn't currently hold text.
fn clipboard_get() -> Option<String> {
    arboard::Clipboard::new()
        .ok()
        .and_then(|mut cb| cb.get_text().ok())
}

/// Closing partner inserted when the user types an opener. Returns the full
/// `"opener+closer"` pair so the caller can `insert` it in one go and then
/// move the caret one back; returns `None` for any other input.
fn auto_pair(input: &str) -> Option<&'static str> {
    match input {
        "(" => Some("()"),
        "[" => Some("[]"),
        "{" => Some("{}"),
        "\"" => Some("\"\""),
        "'" => Some("''"),
        "`" => Some("``"),
        _ => None,
    }
}

/// Matching closer for a single opener character. Quotes mirror to
/// themselves and are used both for auto-pair and wrap.
fn matching_closer(c: char) -> Option<char> {
    match c {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '"' => Some('"'),
        '\'' => Some('\''),
        '`' => Some('`'),
        _ => None,
    }
}

/// Short label suitable for a status-bar flash — just the filename (with
/// extension), falling back to the full path's stringified form for weird
/// edge cases where there is no terminal component.
fn filename_for_flash(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// Flatten an LSP `HoverContents` (one of three shapes — string, marked
/// string, or marked-string array) into a single plain-text body the
/// hover popup can render. Markdown is *not* rendered yet; it shows as
/// raw markup.
fn hover_contents_to_string(c: &lsp_types::HoverContents) -> String {
    use lsp_types::{HoverContents, MarkedString};
    let to_text = |m: &MarkedString| match m {
        MarkedString::String(s) => s.clone(),
        MarkedString::LanguageString(ls) => ls.value.clone(),
    };
    match c {
        HoverContents::Scalar(s) => to_text(s),
        HoverContents::Array(items) => items.iter().map(to_text).collect::<Vec<_>>().join("\n\n"),
        HoverContents::Markup(m) => m.value.clone(),
    }
}

/// Rank a `DiagnosticSeverity` so the "highest" (most severe) one can be
/// picked when several diagnostics share a line. Lower number wins.
fn severity_rank(s: lsp_types::DiagnosticSeverity) -> u8 {
    match s {
        lsp_types::DiagnosticSeverity::ERROR => 0,
        lsp_types::DiagnosticSeverity::WARNING => 1,
        lsp_types::DiagnosticSeverity::INFORMATION => 2,
        lsp_types::DiagnosticSeverity::HINT => 3,
        _ => 4,
    }
}

/// Adjust the find-in-files panel's scroll so `selected` is inside
/// the `visible`-row window. Handles both directions: selection past
/// the bottom edge pulls scroll forward; selection above the top
/// edge pulls it back.
fn scroll_into_view(f: &mut FindInFiles, visible: usize) {
    if f.selected < f.scroll {
        f.scroll = f.selected;
    } else if f.selected >= f.scroll + visible {
        f.scroll = f.selected + 1 - visible;
    }
}

/// Build the per-frame terminal palette context from the current
/// theme. The 16 ANSI slots come from `theme.terminal.palette`,
/// padded with [`DEFAULT_ANSI_16`] for any missing entries; the
/// `Foreground` / `Background` / `Cursor` sentinels default to the
/// editor's own text / background / caret colours when the theme
/// leaves them empty, so the pane stays visually continuous with
/// the chrome by default.
fn build_terminal_palette(theme: &Theme) -> terminal_palette::PaletteContext {
    use terminal_palette::{PaletteColor, DEFAULT_ANSI_16};
    let unpack = |hex: &str, fallback: PaletteColor| -> PaletteColor {
        if hex.is_empty() {
            return fallback;
        }
        match parse_hex_color(hex) {
            Some([r, g, b, _]) => PaletteColor::new(r, g, b),
            None => fallback,
        }
    };
    let editor_fg = unpack(&theme.editor.text_fg, PaletteColor::new(0xEE, 0xEE, 0xEE));
    let editor_bg = unpack(
        &theme.editor.background,
        PaletteColor::new(0x12, 0x12, 0x16),
    );
    let editor_caret = unpack(&theme.editor.caret, PaletteColor::new(0xEE, 0xEE, 0xEE));

    let mut ansi_16 = DEFAULT_ANSI_16;
    for (i, slot) in ansi_16.iter_mut().enumerate() {
        if let Some(hex) = theme.terminal.palette.get(i) {
            *slot = unpack(hex, *slot);
        }
    }

    terminal_palette::PaletteContext {
        ansi_16,
        default_fg: unpack(&theme.terminal.foreground, editor_fg),
        default_bg: unpack(&theme.terminal.background, editor_bg),
        default_cursor: unpack(&theme.terminal.cursor, editor_caret),
    }
}

/// One run of consecutive terminal cells in a single grid row that
/// share the same background colour. Stored on `State` between the
/// grid walk in `refresh_terminal_text` and the pixel-rect emission
/// in `rebuild_scene`, so the renderer doesn't re-walk the grid.
#[derive(Debug, Clone)]
struct TerminalBgRun {
    row: usize,
    col_start: usize,
    col_end: usize,
    color: terminal_palette::PaletteColor,
}

/// Pick a quad colour for one git-gutter line status. Conventions match
/// VS Code: green added / blue modified / red deletion wedge. Themable
/// later via a dedicated section in the theme TOML; hardcoded for v1.
fn git_marker_color(status: git::GitLineStatus) -> SceneColor {
    let hex = match status {
        git::GitLineStatus::Added => "#3fb950",
        git::GitLineStatus::Modified => "#388bfd",
        git::GitLineStatus::Deleted => "#f85149",
    };
    quad_color(hex)
}

/// Pick the text colour for a file-tree status decoration. Uses the
/// same green / blue / red palette as the gutter so the user reads
/// "this file is modified" from either side at the same glance.
/// `Added` reuses green; `Untracked` is yellow so a brand-new file
/// reads as "needs attention but not yet a change vs HEAD".
/// `Conflicted` is the same red as `Deleted` for v1.
fn file_git_status_color(status: git::FileGitStatus) -> Color {
    let hex = match status {
        git::FileGitStatus::Modified => "#388bfd",
        git::FileGitStatus::Added => "#3fb950",
        git::FileGitStatus::Untracked => "#d29922",
        git::FileGitStatus::Deleted => "#f85149",
        git::FileGitStatus::Conflicted => "#f85149",
    };
    text_color(hex)
}

/// Single-character suffix shown after the filename on the tree row.
/// Mirrors `git status --porcelain` codes so the indicator reads
/// the same as the user already knows from the CLI.
fn file_git_status_label(status: git::FileGitStatus) -> &'static str {
    match status {
        git::FileGitStatus::Modified => "M",
        git::FileGitStatus::Added => "A",
        git::FileGitStatus::Untracked => "?",
        git::FileGitStatus::Deleted => "D",
        git::FileGitStatus::Conflicted => "U",
    }
}

/// Pick a quad colour for one diagnostic severity. Uses the theme's syntax
/// strings so themes can override the palette later; for now, fall back
/// to fixed hexes if any are missing.
fn diagnostic_color(severity: Option<lsp_types::DiagnosticSeverity>) -> SceneColor {
    let hex = match severity {
        Some(lsp_types::DiagnosticSeverity::ERROR) => "#f44747",
        Some(lsp_types::DiagnosticSeverity::WARNING) => "#cca700",
        Some(lsp_types::DiagnosticSeverity::INFORMATION) => "#3794ff",
        Some(lsp_types::DiagnosticSeverity::HINT) => "#a0a0a0",
        _ => "#a0a0a0",
    };
    quad_color(hex)
}

/// Width of the text shaped into `stack` in physical pixels — the maximum
/// `x + width` over every glyph in every layout run. Used to right-align
/// short captions whose width depends on the content (e.g. "Ln L, Col C").
fn shaped_width(stack: &TextStack) -> f32 {
    stack
        .buffer
        .layout_runs()
        .flat_map(|run| run.glyphs.iter().map(|g| g.x + g.w))
        .fold(0.0_f32, f32::max)
}

/// Total gutter width in physical pixels, including the outer paddings.
/// Approximated from `font_size_pt × scale × MONOSPACE_CHAR_FACTOR`, which
/// is close enough to the real digit advance that the right-aligned numbers
/// line up under each other for ASCII content.
fn gutter_outer_width(font_size_pt: f32, scale: f32) -> f32 {
    let char_w = font_size_pt * MONOSPACE_CHAR_FACTOR * scale;
    (GUTTER_PAD_LEFT_DIP + GUTTER_PAD_RIGHT_DIP) * scale + GUTTER_DIGITS as f32 * char_w
}

/// A window title showing the file name when one is open, or the welcome
/// label otherwise.
fn window_title(path: Option<&Path>) -> String {
    match path {
        Some(p) => format!(
            "LightEditor — {}",
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string())
        ),
        None => "LightEditor — editable surface".to_string(),
    }
}

/// Initial buffer content — the spec §3.4 multilingual matrix doubles as a
/// smoke test that editing keeps complex scripts intact.
const WELCOME_TEXT: &str = "\
LightEditor — editable surface\n\
\n\
Type to edit. Arrows move; Shift+Arrows select.\n\
Click to place the caret; drag to select; wheel to scroll.\n\
\n\
สวัสดีชาวโลก  ·  你好,世界  ·  مرحبا بالعالم\n\
안녕하세요 세계  ·  नमस्ते दुनिया\n\
🇹🇭 🌏 🚀 👨‍👩‍👧‍👦\n\
";

struct State {
    window: Arc<Window>,
    gpu: GpuContext,
    /// Quads for the editor + chrome layer (selection / carets / gutter
    /// / tab strip / sidebar / status bar). Drawn first.
    quads: QuadRenderer,
    /// Quads for the floating-overlay layer (find bar / hover popup /
    /// completion popup / command palette). Drawn after the main layer
    /// AND after the main text, so popup backgrounds correctly occlude
    /// the editor text behind them.
    overlay_quads: QuadRenderer,
    /// Shared across every `TextStack` (editor, palette, find, tabs, close,
    /// status). `FontSystem::new()` walks the OS font directory once —
    /// ~80ms cold — so building one and lending it out is much cheaper than
    /// one per stack.
    font_system: FontSystem,
    /// Shared swash glyph cache.
    swash_cache: SwashCache,
    /// Shared GPU text resources — one viewport, one atlas, one renderer.
    /// Every per-frame TextArea (editor, tabs, close ×s, status, find,
    /// palette) goes into a single `prepare` + `render` call, so adding a
    /// stack is now cheap.
    text_gpu: TextGpu,
    /// TextStack for the *active* document. Reshape happens on switch and on
    /// edit (`text_dirty`).
    text: TextStack,

    /// Open documents, oldest-first. There is always at least one — closing
    /// the last document opens a fresh scratch one.
    docs: Vec<Document>,
    /// Index of the active document into `docs`. Always valid.
    active: usize,
    /// TextStack dedicated to the tab strip labels. Rebuilt whenever the
    /// tab list, a label, or a dirty flag changes — *not* on every keystroke.
    tabs_text: TextStack,
    /// TextStack holding a single "×" glyph. Rendered once per tab via
    /// separate TextAreas at each slot's close-button position; the buffer
    /// content never changes.
    close_text: TextStack,
    /// Left half of the status bar — `path · language · LE`.
    status_left: TextStack,
    /// Right half of the status bar — `Ln L, Col C · N lines`. Positioned
    /// via a measurement of its shaped width so the right edge sits one
    /// `STATUS_PAD_X_DIP` from the window edge.
    status_right: TextStack,
    /// Gutter TextStack — right-aligned line numbers, one per buffer line.
    /// Reshape happens lazily: only when the line count changes or the
    /// active document switches.
    gutter_text: TextStack,
    /// Line count the gutter was last reshaped for. Tracking this avoids
    /// reshaping N digits/newlines on every keystroke.
    gutter_lines: usize,

    /// The scene rebuilt from `editor` whenever it changes — everything
    /// in the editor / chrome layer.
    scene: Scene,
    /// Scene for the overlay layer (popups + find bar). Rebuilt
    /// alongside `scene` so the two stay in sync.
    overlay_scene: Scene,

    /// Window scale factor; physical = logical * scale.
    scale: f32,
    /// Left edge of the editor text in physical pixels. Includes the
    /// gutter's outer width — text starts to its right.
    text_inset_x: f32,
    /// `(TAB_BAR_HEIGHT_DIP + TEXT_TOP_GAP_DIP) × scale` — top of the text
    /// block, accounting for the tab strip.
    text_inset_y: f32,
    /// Total horizontal padding (left + right), in physical pixels.
    text_padding: f32,
    /// Caret width, in physical pixels.
    caret_width: f32,

    /// Latched keyboard modifiers (Shift for selection-extending movement).
    modifiers: ModifiersState,
    /// Last known pointer position in physical pixels — `None` until the first
    /// `CursorMoved`, so a click before any move is ignored rather than
    /// landing at (0, 0).
    mouse_pos: Option<(f32, f32)>,
    /// While a left-button drag is in progress, the `char` index where it
    /// started (the selection's anchor).
    drag_anchor: Option<usize>,
    /// Set when the change that dirtied the scene moved the caret (an edit,
    /// arrow key, or click) — the next rebuild scrolls it into view. Wheel
    /// scrolling, drag-scrolling, and tab switches leave this `false` so they
    /// aren't undone.
    follow_caret: bool,
    /// The buffer text changed — TextStack needs a reshape before the next frame.
    text_dirty: bool,
    /// The editor state changed — the scene needs rebuilding before the next frame.
    scene_dirty: bool,

    /// Command-palette state when the popup is open.
    palette: Option<CommandPalette>,
    /// Second TextStack dedicated to the palette overlay so it can shape
    /// independently of the buffer.
    palette_text: TextStack,

    /// Third TextStack for the find-bar caption ("Find: query   3/12"). The
    /// `FindBar` itself lives on the active document.
    find_text: TextStack,

    /// Spaces inserted by the Tab key (pre-built from `settings.editor.tab_size`).
    tab_spaces: String,

    /// Active theme — colors for every surface and syntax category. Hot-
    /// reloaded from `theme.toml` via the same watcher pattern as settings.
    theme: Theme,

    /// Transient overlay for the status bar's left half — `Some((msg, t))`
    /// while a flash is active. Cleared via `AppEvent::ClearFlash` after
    /// [`FLASH_DURATION`]; the `Instant` is also used to gate the clear so
    /// a later flash isn't wiped early by an earlier flash's timer.
    status_flash: Option<(String, Instant)>,

    /// Most recent left-button press — drives double/triple-click detection
    /// (select word / select line). Resets to count = 1 when the press is
    /// outside the interval or moves further than the threshold.
    last_click: Option<(Instant, (f32, f32), u32)>,

    /// When `true` the editor text wraps at the viewport width. Off mode
    /// uses an unbounded wrap so long lines extend off-screen — toggled by
    /// Cmd-Alt-Z.
    word_wrap: bool,
    /// When `true`, spaces are drawn as `·` and tabs as `→`. The underlying
    /// buffer keeps real spaces/tabs; this only affects the shaped display
    /// text. Toggled by Cmd-Alt-W (when the find bar is closed).
    visible_whitespace: bool,
    /// Tab whose close "×" is under the pointer. Brightens that "×" on
    /// hover so the affordance is discoverable.
    hovered_close: Option<usize>,

    /// Whether the caret quad is currently drawn. Flipped by the periodic
    /// `CaretTick` event when no interaction has happened recently.
    caret_visible: bool,
    /// Stamp of the last keystroke / mouse press — `tick_caret` keeps the
    /// caret solid while this is within `CARET_BLINK_PAUSE`.
    last_interaction: Instant,
    /// Used by `set_status_flash` to schedule its own `ClearFlash` event
    /// from a detached sleeper thread. Clone-cheap.
    flash_proxy: EventLoopProxy<AppEvent>,

    /// Document version per-file for LSP didChange — LSP requires a
    /// monotonically-increasing counter. Bumped each time we send a
    /// didChange to a server.
    lsp_doc_version: HashMap<PathBuf, i32>,

    /// Language Server Protocol state — per-server connections, pending
    /// requests, and the diagnostics map (spec §3.5, §4.2). Empty until a
    /// file in a supported language is opened; then the matching server is
    /// spawned lazily.
    lsp: LspState,
    /// Hover popup state when a server's hover response is visible. The
    /// String is the rendered markup; the (line, col) is the caret-anchor
    /// position the popup should hang under.
    hover_popup: Option<HoverPopup>,
    /// Dedicated TextStack for the hover overlay's body.
    hover_text: TextStack,
    /// Completion popup state — shown when the user invokes Ctrl-Space
    /// and the active LSP server returns matching items.
    completion: Option<CompletionPopup>,
    /// Dedicated TextStack for the completion popup's item list.
    completion_text: TextStack,

    /// File-tree sidebar (M3). Holds the loaded node list + scroll
    /// position; visibility lives on the struct itself so toggling on
    /// and off is instant once it's been opened once.
    file_tree: FileTree,
    /// Dedicated TextStack for the sidebar's row labels.
    file_tree_text: TextStack,
    /// Sidebar width in DIP. User-resizable via the drag handle on
    /// the right edge; seeded to [`SIDEBAR_DEFAULT_WIDTH_DIP`] and
    /// clamped to `[SIDEBAR_MIN_WIDTH_DIP, SIDEBAR_MAX_WIDTH_DIP]`.
    /// Stored in logical units so it survives DPI changes (a window
    /// moved to a Retina display keeps the same visual size).
    sidebar_width_dip: f32,
    /// `Some(grab_offset_px)` while the user is dragging the resize
    /// handle. The grab offset is the distance, in physical pixels,
    /// between the mouse-down point and the sidebar's right edge —
    /// so the edge stays glued to the cursor instead of snapping to
    /// it on the first drag move.
    sidebar_resize_drag: Option<f32>,
    /// Recursive filesystem watcher rooted at the sidebar's root.
    /// Fires `AppEvent::FileTreeChanged` after a 200 ms quiet period;
    /// the handler reloads with expansion preserved. `None` when
    /// `notify` setup failed — the tree still works, just without
    /// auto-refresh on external changes.
    _file_tree_watcher: Option<RecommendedWatcher>,
    /// Workspace-wide git status: absolute path → `M`/`A`/`?`/`D`/`U`
    /// marker shown next to the filename in the sidebar. Refreshed on
    /// tab switch, save, and every file-tree reload. Empty when the
    /// workspace isn't a git repo, which silently hides the markers.
    workspace_git_status: HashMap<PathBuf, git::FileGitStatus>,
    /// Per-directory aggregate of [`workspace_git_status`], used to
    /// decorate parent rows in the sidebar with the highest-priority
    /// status anywhere in their subtree. Computed from the file map
    /// in the same refresh pass.
    workspace_git_dir_status: HashMap<PathBuf, git::FileGitStatus>,
    /// `package.json` scripts detected in the workspace root, used to
    /// populate the command palette with `Run script: <name>`
    /// entries. Refreshed on every `FileTreeChanged` (which fires for
    /// any edit to package.json too, since the watcher covers the
    /// whole root). Empty when no manifest is present.
    npm_scripts: Vec<scripts::NpmScript>,
    /// Flutter project (pubspec.yaml with `flutter:` SDK dependency)
    /// detected in the workspace root. `Some` ⇒ the palette gets
    /// `Flutter: Run / Hot Reload / Hot Restart / Stop` entries and
    /// the dispatch handlers know which CLI to drive.
    flutter_project: Option<flutter::FlutterProject>,
    /// `true` between `Flutter: Run` and `Flutter: Stop` — i.e. when
    /// the editor thinks a `flutter run` session is alive in the
    /// terminal pane. Drives save-triggered hot reload and the
    /// status-bar "Flutter: running" indicator. We don't introspect
    /// the terminal output to confirm; the flag tracks the user's
    /// last *explicit* action, which is right ~95% of the time and
    /// recoverable by another palette command.
    flutter_session_active: bool,
    /// Cached list of `flutter devices --machine` output, populated
    /// by a background thread on startup and refreshed after each
    /// Flutter palette command. Drives the per-device picker in the
    /// command palette. Empty when flutter isn't installed, no
    /// devices are connected, or the refresh is still in flight on
    /// first launch.
    flutter_devices: Vec<flutter::FlutterDevice>,
    /// Find-in-files overlay (M3). `Some` while the panel is open;
    /// `None` once the user dismisses it. Re-opening builds a fresh
    /// state — there's no value in remembering an old query.
    find_in_files: Option<FindInFiles>,
    /// Dedicated TextStack for the find-in-files panel (input row +
    /// status row + matched lines).
    find_in_files_text: TextStack,
    /// Embedded terminal pane (M3) — `Some` while a shell is running.
    /// Visibility toggles via `Cmd-J`; the shell stays alive across
    /// hide/show so scrollback survives.
    terminal: Option<terminal::TerminalPane>,
    /// Dedicated TextStack for the terminal pane's grid contents.
    terminal_text: TextStack,
    /// Per-cell background runs for the terminal pane, collected
    /// during [`refresh_terminal_text`] and converted to pixel
    /// rects at scene-rebuild time. Consecutive cells in the same
    /// row with the same bg colour share one run; the default-bg
    /// sentinel is skipped (the pane fills that already).
    terminal_bg_runs: Vec<TerminalBgRun>,

    /// When the last unhandled key press happened, for keystroke-latency timing.
    pending_keystroke: Option<Instant>,
    /// Rolling 1-second frame-time window (spec §8).
    frame_count: u64,
    last_report: Instant,
    last_frame_us: u128,
    cold_start: Option<Instant>,
}

impl State {
    #[allow(clippy::too_many_arguments)]
    fn new(
        window: Arc<Window>,
        cold_start: Instant,
        initial_text: &str,
        file_path: Option<PathBuf>,
        settings: &Settings,
        theme: Theme,
        flash_proxy: EventLoopProxy<AppEvent>,
    ) -> Self {
        let scale = window.scale_factor() as f32;
        let size = window.inner_size();
        let gpu = GpuContext::new(window.clone());
        let quads = QuadRenderer::new(&gpu.device, gpu.format());
        let overlay_quads = QuadRenderer::new(&gpu.device, gpu.format());

        let font_size_pt = settings.editor.font_size;
        let line_height_pt = settings.editor.line_height;
        let tab_spaces = " ".repeat(settings.editor.tab_size);

        // One `FontSystem` shared by every `TextStack` — the OS font scan
        // takes ~80ms cold, so paying it once and lending out a `&mut` keeps
        // the editor under the 250ms hard limit.
        let mut font_system = editor_ui_text::new_font_system();
        let swash_cache = editor_ui_text::new_swash_cache();

        // One shared TextGpu — every stack below reuses these GPU resources
        // through the single `prepare`/`render` call in `Self::render`.
        let text_gpu = TextGpu::new(&gpu.device, &gpu.queue, gpu.format());

        // The gutter occupies the editor's left inset; the text's effective
        // left padding is the gutter's outer width.
        let gutter_width = gutter_outer_width(font_size_pt, scale);
        let right_pad = TEXT_INSET_DIP * scale;
        let text_padding = gutter_width + right_pad;
        let text = TextStack::new(
            &mut font_system,
            size.width as f32 - text_padding,
            font_size_pt,
            line_height_pt,
            scale,
            initial_text,
        );

        let doc = match file_path {
            Some(p) => Document::from_file(p, initial_text),
            None => Document::new_scratch(initial_text),
        };

        // Palette rows must not soft-wrap — a long entry (e.g.
        // `Flutter: Run on iPhone · ios (<long-uuid>)`) breaking
        // onto a second visible line would split one logical row
        // into two, and the selection highlight + index → row math
        // would disagree by one. The panel's `TextBounds` clips the
        // overflow horizontally; the user sees a truncated label.
        let palette_text = TextStack::new(
            &mut font_system,
            NO_WRAP_WIDTH_PX,
            font_size_pt,
            line_height_pt,
            scale,
            "",
        );

        let find_width = (FIND_WIDTH_DIP - 2.0 * FIND_PAD_DIP) * scale;
        let find_text = TextStack::new(
            &mut font_system,
            find_width,
            font_size_pt,
            line_height_pt,
            scale,
            "",
        );

        // Tab strip TextStack — one long single line of labels. Width spans
        // the whole window; no wrap.
        let tabs_text = TextStack::new(
            &mut font_system,
            size.width as f32,
            font_size_pt,
            line_height_pt,
            scale,
            "",
        );

        // Close-button TextStack — shapes the "×" glyph once and is then
        // drawn at every tab's close-button position via N TextAreas.
        let close_text = TextStack::new(
            &mut font_system,
            TAB_CLOSE_W_DIP * scale,
            font_size_pt,
            line_height_pt,
            scale,
            "×",
        );

        // Status-bar TextStacks — one for each side. Both span the whole
        // strip width so a long left caption can still measure correctly
        // before we decide whether to truncate.
        let status_width = size.width as f32 - 2.0 * STATUS_PAD_X_DIP * scale;
        let status_left = TextStack::new(
            &mut font_system,
            status_width,
            font_size_pt,
            line_height_pt,
            scale,
            "",
        );
        let status_right = TextStack::new(
            &mut font_system,
            status_width,
            font_size_pt,
            line_height_pt,
            scale,
            "",
        );

        // Gutter TextStack — single column, no wrap.
        let gutter_text = TextStack::new(
            &mut font_system,
            gutter_width,
            font_size_pt,
            line_height_pt,
            scale,
            "",
        );

        let scene = Scene::new(SceneNode::group(Rect::new(
            0.0,
            0.0,
            size.width as f32,
            size.height as f32,
        )));
        let overlay_scene = Scene::new(SceneNode::group(Rect::new(
            0.0,
            0.0,
            size.width as f32,
            size.height as f32,
        )));

        // Hover popup text — same width budget as the find bar for a
        // similar "tight inline tooltip" feel.
        let hover_text = TextStack::new(
            &mut font_system,
            (HOVER_WIDTH_DIP - 2.0 * HOVER_PAD_DIP) * scale,
            font_size_pt,
            line_height_pt,
            scale,
            "",
        );

        // Completion-popup rows don't soft-wrap. A long completion
        // label (`MyLongClassName.factoryConstructorFromParts`)
        // breaking onto a second visible row would split one logical
        // entry into two rendered rows, and the selection
        // highlight / Enter-fires-this-entry math (both keyed on
        // row index × line_height) would disagree. Clip on the
        // panel's right edge instead.
        let completion_text = TextStack::new(
            &mut font_system,
            NO_WRAP_WIDTH_PX,
            font_size_pt,
            line_height_pt,
            scale,
            "",
        );

        // Tree rows don't soft-wrap — a long filename clips at the
        // sidebar's right edge instead of breaking onto a second
        // visible row. Keeps `idx × line_h` accurate for hit-test
        // and the keyboard-selection highlight.
        let file_tree_text = TextStack::new(
            &mut font_system,
            NO_WRAP_WIDTH_PX,
            font_size_pt,
            line_height_pt,
            scale,
            "",
        );

        // Result rows in the find-in-files panel hold long file paths
        // and match snippets — they MUST stay one row per result so
        // `idx × line_height` continues to identify the chosen
        // result on Enter. Clip on the right edge; the user can
        // resize their window or move along the row to inspect
        // long paths.
        let find_in_files_text = TextStack::new(
            &mut font_system,
            NO_WRAP_WIDTH_PX,
            font_size_pt,
            line_height_pt,
            scale,
            "",
        );

        let terminal_text = TextStack::new(
            &mut font_system,
            size.width as f32,
            font_size_pt,
            line_height_pt,
            scale,
            "",
        );

        // Derive the sidebar's root from the active doc's workspace
        // (the same find_project_root walk the LSP layer uses), falling
        // back to CWD when no doc has a path yet.
        let tree_root = doc
            .file_path
            .as_deref()
            .and_then(lsp::find_project_root)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let hidden_dirs = settings.file_tree.hidden_dirs.clone();
        let file_tree = FileTree::new(tree_root, hidden_dirs.clone());
        // The watcher snapshots its own copy of the hidden-dirs list at
        // spawn time. A live settings reload won't change which paths
        // the watcher drops until the editor restarts — kept simple
        // because the list rarely changes in practice; if it ever
        // matters, swap the snapshot for an `Arc<RwLock<Vec<String>>>`
        // and update it from the `SettingsChanged` handler.
        let file_tree_watcher =
            spawn_file_tree_watcher(&file_tree.root, hidden_dirs, flash_proxy.clone());
        let workspace_git_status = git::compute_workspace_status(&file_tree.root);
        let workspace_git_dir_status = git::aggregate_dirs(&workspace_git_status, &file_tree.root);
        let npm_scripts = scripts::read_scripts(&file_tree.root);
        let flutter_project = flutter::detect_flutter(&file_tree.root);

        let mut state = Self {
            window,
            gpu,
            quads,
            overlay_quads,
            font_system,
            swash_cache,
            text_gpu,
            text,
            docs: vec![doc],
            active: 0,
            tabs_text,
            close_text,
            status_left,
            status_right,
            gutter_text,
            gutter_lines: 0,
            scene,
            overlay_scene,
            scale,
            text_inset_x: gutter_width,
            text_inset_y: (TAB_BAR_HEIGHT_DIP + TEXT_TOP_GAP_DIP) * scale,
            text_padding,
            caret_width: CARET_WIDTH_DIP * scale,
            modifiers: ModifiersState::empty(),
            mouse_pos: None,
            drag_anchor: None,
            follow_caret: false,
            text_dirty: false,
            scene_dirty: true,
            palette: None,
            palette_text,
            find_text,
            status_flash: None,
            last_click: None,
            word_wrap: true,
            visible_whitespace: false,
            hovered_close: None,
            caret_visible: true,
            last_interaction: Instant::now(),
            flash_proxy,
            lsp_doc_version: HashMap::new(),
            lsp: LspState::new(),
            hover_popup: None,
            hover_text,
            completion: None,
            completion_text,
            file_tree,
            file_tree_text,
            sidebar_width_dip: SIDEBAR_DEFAULT_WIDTH_DIP,
            sidebar_resize_drag: None,
            _file_tree_watcher: file_tree_watcher,
            workspace_git_status,
            workspace_git_dir_status,
            npm_scripts,
            flutter_project,
            flutter_session_active: false,
            flutter_devices: Vec::new(),
            find_in_files: None,
            find_in_files_text,
            terminal: None,
            terminal_text,
            terminal_bg_runs: Vec::new(),
            tab_spaces,
            theme,
            pending_keystroke: None,
            frame_count: 0,
            last_report: Instant::now(),
            last_frame_us: 0,
            cold_start: Some(cold_start),
        };
        state.refresh_tabs_text();
        state.rebuild_scene();
        // Introduce the initial document to its LSP server (no-op when
        // there is no file path or no server for its language).
        state.lsp_did_open_doc(0);
        // If this is a Flutter workspace, kick off a background
        // `flutter devices --machine` so the per-device entries are
        // ready by the time the user opens the palette. The fetch
        // takes ~1–3 s; doing it in a thread keeps startup snappy.
        if state.flutter_project.is_some() {
            state.refresh_flutter_devices_async();
        }
        state
    }

    /// Borrow the active document.
    fn doc(&self) -> &Document {
        &self.docs[self.active]
    }

    /// Mutably borrow the active document.
    fn doc_mut(&mut self) -> &mut Document {
        &mut self.docs[self.active]
    }

    /// Line height in physical pixels — the single unit carets, highlights,
    /// and scroll math all work in. Sourced from TextStack so it can never
    /// drift from the actual font metrics.
    fn line_height(&self) -> f32 {
        self.text.line_height()
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        self.gpu.resize(size.width, size.height);
        self.text
            .set_width(&mut self.font_system, size.width as f32 - self.text_padding);
        self.tabs_text
            .set_width(&mut self.font_system, size.width as f32);
        let status_width = size.width as f32 - 2.0 * STATUS_PAD_X_DIP * self.scale;
        self.status_left
            .set_width(&mut self.font_system, status_width);
        self.status_right
            .set_width(&mut self.font_system, status_width);
        self.terminal_text
            .set_width(&mut self.font_system, size.width as f32);
        // Propagate the new pane size to the PTY so programs that look
        // at `$COLUMNS` / `tput cols` see the right answer.
        self.resync_terminal_cells();
        // Palette / find widths are fixed; nothing to update on a window resize.
        self.text_dirty = true;
        self.scene_dirty = true;
        // ControlFlow::Wait won't redraw on its own — a resize must ask.
        self.window.request_redraw();
    }

    /// Recompute the terminal's cell-grid dimensions from the pane's
    /// current pixel size and push them through to the PTY. Called on
    /// every window resize and once at spawn.
    fn resync_terminal_cells(&mut self) {
        let Some(pane_rect) = self.terminal.as_ref().map(|_| self.terminal_pane_rect()) else {
            return;
        };
        // Terminal text uses the editor's monospace metrics. We need the
        // *actual* glyph advance from cosmic-text here, not the 0.6 ×
        // font_size approximation — they drift enough that the PTY's
        // column count and the rendered text disagree on where each
        // column lands. And we have to measure from `terminal_text`
        // specifically, not the chrome's buffer, because the two can
        // pick different monospace faces even when both request
        // `Family::Monospace`.
        let cell_w = self.terminal_measured_char_width();
        let cell_h = self.terminal_measured_line_height();
        if cell_w <= 0.0 || cell_h <= 0.0 {
            return;
        }
        let pad = 6.0 * self.scale;
        let usable_w = (pane_rect.size.width - 2.0 * pad).max(cell_w);
        let usable_h = (pane_rect.size.height - 2.0 * pad).max(cell_h);
        let cols = ((usable_w / cell_w).floor() as u16).max(1);
        let rows = ((usable_h / cell_h).floor() as u16).max(1);
        if let Some(term) = self.terminal.as_mut() {
            term.resize(cols, rows, cell_w, cell_h);
        }
    }

    /// Re-size everything for a new window scale factor (the window moved to a
    /// display with different DPI).
    fn apply_scale(&mut self, scale: f32) {
        if scale <= 0.0 || scale == self.scale {
            return;
        }
        self.scale = scale;
        let font_size_pt = self.text.font_size_pt();
        let gutter_width = gutter_outer_width(font_size_pt, scale);
        self.text_inset_x = gutter_width;
        self.text_inset_y = (TAB_BAR_HEIGHT_DIP + TEXT_TOP_GAP_DIP) * scale;
        self.text_padding = gutter_width + TEXT_INSET_DIP * scale;
        self.caret_width = CARET_WIDTH_DIP * scale;
        let surface_w = self.gpu.surface_config.width as f32;
        let fs = &mut self.font_system;
        self.text.set_scale(fs, scale);
        self.text.set_width(fs, surface_w - self.text_padding);
        self.palette_text.set_scale(fs, scale);
        // No wrap — see palette_text construction comment.
        self.palette_text.set_width(fs, NO_WRAP_WIDTH_PX);
        self.find_text.set_scale(fs, scale);
        self.find_text
            .set_width(fs, (FIND_WIDTH_DIP - 2.0 * FIND_PAD_DIP) * scale);
        self.tabs_text.set_scale(fs, scale);
        self.tabs_text.set_width(fs, surface_w);
        self.close_text.set_scale(fs, scale);
        self.close_text.set_width(fs, TAB_CLOSE_W_DIP * scale);
        let status_width = surface_w - 2.0 * STATUS_PAD_X_DIP * scale;
        self.status_left.set_scale(fs, scale);
        self.status_left.set_width(fs, status_width);
        self.status_right.set_scale(fs, scale);
        self.status_right.set_width(fs, status_width);
        self.gutter_text.set_scale(fs, scale);
        self.gutter_text.set_width(fs, gutter_width);
        // Gutter line count is fine; the cached `gutter_lines` is still
        // valid (we're just rescaling existing shaped content).
        self.text_dirty = true;
        self.scene_dirty = true;
        self.window.request_redraw();
    }

    /// Pick up a fresh `Theme`: swap colors and ask for a redraw. No
    /// reshaping needed — only the colors of existing glyphs/quads change.
    fn reload_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.text_dirty = true; // syntax colors come from rich shaping
        self.scene_dirty = true;
        self.set_status_flash("theme reloaded".to_string());
        self.window.request_redraw();
    }

    /// Pick up a fresh `Settings`: reapply font metrics to every TextStack
    /// and rebuild the Tab-key spaces. Called from the settings file watcher.
    fn reload_settings(&mut self, settings: &Settings) {
        let fs = &mut self.font_system;
        let font = settings.editor.font_size;
        let lh = settings.editor.line_height;
        self.text.set_font_size(fs, font, lh);
        self.palette_text.set_font_size(fs, font, lh);
        self.find_text.set_font_size(fs, font, lh);
        self.tabs_text.set_font_size(fs, font, lh);
        self.close_text.set_font_size(fs, font, lh);
        self.status_left.set_font_size(fs, font, lh);
        self.status_right.set_font_size(fs, font, lh);
        self.gutter_text.set_font_size(fs, font, lh);
        // Gutter width depends on font size — recompute and reflow the
        // editor's wrap width accordingly.
        let gutter_width = gutter_outer_width(font, self.scale);
        self.text_inset_x = gutter_width;
        self.text_padding = gutter_width + TEXT_INSET_DIP * self.scale;
        let surface_w = self.gpu.surface_config.width as f32;
        self.text.set_width(fs, surface_w - self.text_padding);
        self.gutter_text.set_width(fs, gutter_width);
        self.tab_spaces = " ".repeat(settings.editor.tab_size);
        self.set_status_flash(format!(
            "settings reloaded · font {font} · line {lh} · tab {}",
            settings.editor.tab_size
        ));
        self.text_dirty = true;
        self.scene_dirty = true;
        log::info!(
            "settings reloaded: font_size={font} line_height={lh} tab_size={}",
            settings.editor.tab_size
        );
        self.window.request_redraw();
    }

    /// Show `msg` on the status bar's left half and schedule its own
    /// `AppEvent::ClearFlash` after [`FLASH_DURATION`].
    ///
    /// Each call spawns a detached sleeper thread; if `msg` overwrites a
    /// previous flash, both timers will fire, but `clear_status_flash`
    /// gates on the stored timestamp so the *older* timer is a no-op once
    /// the newer message has refreshed the deadline.
    fn set_status_flash(&mut self, msg: String) {
        self.status_flash = Some((msg, Instant::now()));
        self.scene_dirty = true;
        let proxy = self.flash_proxy.clone();
        std::thread::spawn(move || {
            std::thread::sleep(FLASH_DURATION);
            let _ = proxy.send_event(AppEvent::ClearFlash);
        });
        self.window.request_redraw();
    }

    /// Clear the flash if (and only if) it is genuinely expired — the timer
    /// thread fires this; a stale fire from an earlier flash is dropped.
    fn clear_status_flash(&mut self) {
        let expired = self
            .status_flash
            .as_ref()
            .is_some_and(|(_, t)| t.elapsed() >= FLASH_DURATION);
        if expired {
            self.status_flash = None;
            self.scene_dirty = true;
            self.window.request_redraw();
        }
    }

    /// Route a key press into the active document or the open overlay.
    fn handle_key(&mut self, event: KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        self.note_activity();

        // Cmd-Shift-P toggles the palette regardless of whether it is open,
        // so it stays a single muscle-memory key combo.
        if is_cmd_or_ctrl(self.modifiers)
            && self.modifiers.shift_key()
            && shortcut_letter(&event, 'p')
        {
            if self.palette.is_some() {
                self.close_palette();
            } else {
                self.open_palette();
            }
            return;
        }

        // When the palette is open it captures every other key.
        if self.palette.is_some() {
            self.handle_palette_key(event);
            return;
        }

        // Cmd/Ctrl shortcuts — checked before regular key handling so that
        // e.g. Cmd-S doesn't also try to insert "s".
        if is_cmd_or_ctrl(self.modifiers) {
            // Ctrl-Tab / Ctrl-Shift-Tab cycle tabs. Cmd-Tab on macOS is the
            // OS-level app switcher; Ctrl-Tab is the conventional in-app one.
            if let Key::Named(NamedKey::Tab) = &event.logical_key {
                if self.modifiers.shift_key() {
                    self.prev_tab();
                } else {
                    self.next_tab();
                }
                return;
            }
            // Cmd-Alt-Up / Cmd-Alt-Down add a caret one line above / below
            // the primary, at the same column. Matches VSCode.
            if self.modifiers.alt_key() {
                match &event.logical_key {
                    Key::Named(NamedKey::ArrowUp) => {
                        self.add_cursor_above();
                        return;
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        self.add_cursor_below();
                        return;
                    }
                    _ => {}
                }
            }
        }
        // Alt-Shift-↑/↓ swaps the line under (or selected by) the primary
        // with the line above / below. Checked outside the Cmd block since
        // it doesn't involve Cmd / Ctrl.
        if self.modifiers.alt_key() && self.modifiers.shift_key() {
            match &event.logical_key {
                Key::Named(NamedKey::ArrowUp) => {
                    self.doc_mut().editor.move_lines_up();
                    self.text_dirty = true;
                    self.scene_dirty = true;
                    self.follow_caret = true;
                    self.mark_dirty_if_clean();
                    self.window.request_redraw();
                    return;
                }
                Key::Named(NamedKey::ArrowDown) => {
                    self.doc_mut().editor.move_lines_down();
                    self.text_dirty = true;
                    self.scene_dirty = true;
                    self.follow_caret = true;
                    self.mark_dirty_if_clean();
                    self.window.request_redraw();
                    return;
                }
                _ => {}
            }
        }
        // Open dialogs claim Cmd-V before anything else so the user's
        // paste goes to whichever input they're currently typing in.
        // Order: palette (already captured above) > find bar >
        // find-in-files > terminal > editor.
        if self.doc().find.is_some()
            && is_cmd_or_ctrl(self.modifiers)
            && shortcut_letter(&event, 'v')
        {
            self.handle_find_key(event);
            return;
        }
        if self.find_in_files.is_some()
            && is_cmd_or_ctrl(self.modifiers)
            && shortcut_letter(&event, 'v')
        {
            if let Some(text) = clipboard_get() {
                if let Some(f) = self.find_in_files.as_mut() {
                    if f.input_focused {
                        f.query.push_str(&text);
                    }
                }
                self.refresh_find_in_files_text();
                self.scene_dirty = true;
                self.window.request_redraw();
            }
            return;
        }
        // Cmd-V routes to the embedded terminal whenever the pane is
        // *visible*, even without keyboard focus — the user's mental
        // model is "I copied text, I want to push it into the
        // terminal" and they shouldn't have to re-click the pane
        // first. Has to short-circuit *before* the editor's
        // Cmd-letter match below, otherwise the editor's `"v"` arm
        // would fire `paste_clipboard()` against the editor buffer.
        // Editor paste while the terminal is open requires hiding
        // the pane (Cmd-J) or a future explicit "Paste into editor"
        // command.
        if is_cmd_or_ctrl(self.modifiers)
            && shortcut_letter(&event, 'v')
            && self.terminal.as_ref().is_some_and(|t| t.visible)
        {
            if let Some(text) = clipboard_get() {
                if let Some(t) = self.terminal.as_ref() {
                    t.write(text.into_bytes());
                }
            }
            self.set_status_flash("pasted into terminal".to_string());
            return;
        }
        if is_cmd_or_ctrl(self.modifiers) {
            if let Some(lower) = shortcut_letter_of(&event) {
                let alt = self.modifiers.alt_key();
                match lower.as_str() {
                    "f" => {
                        // Cmd-Shift-F → project-wide search; Cmd-F →
                        // find in the current buffer.
                        if self.modifiers.shift_key() {
                            self.toggle_find_in_files();
                        } else if self.doc().find.is_some() {
                            self.close_find();
                        } else {
                            self.open_find();
                        }
                        return;
                    }
                    "s" => {
                        if alt {
                            self.save_all();
                        } else {
                            self.save_to_file();
                        }
                        return;
                    }
                    "o" => {
                        self.open_file_dialog();
                        return;
                    }
                    "n" => {
                        self.new_file();
                        return;
                    }
                    "t" => {
                        self.open_new_tab();
                        return;
                    }
                    "w" if alt => {
                        self.toggle_visible_whitespace();
                        return;
                    }
                    "w" => {
                        self.close_active_tab();
                        return;
                    }
                    "z" if alt => {
                        self.toggle_word_wrap();
                        return;
                    }
                    "d" => {
                        if self.modifiers.shift_key() {
                            self.skip_to_next_occurrence();
                        } else {
                            self.add_next_occurrence();
                        }
                        return;
                    }
                    "k" => {
                        if self.modifiers.shift_key() {
                            self.doc_mut().editor.delete_line();
                            self.text_dirty = true;
                            self.scene_dirty = true;
                            self.follow_caret = true;
                            self.mark_dirty_if_clean();
                            self.window.request_redraw();
                        } else {
                            self.collapse_selection_to_primary();
                        }
                        return;
                    }
                    "a" => {
                        self.select_all();
                        return;
                    }
                    "b" => {
                        self.toggle_file_tree();
                        return;
                    }
                    "j" => {
                        // Cmd-J toggles the embedded terminal pane,
                        // matching VS Code's "Toggle Panel" muscle
                        // memory. Spawns the shell on first open.
                        self.toggle_terminal();
                        return;
                    }
                    "i" if !alt => {
                        // Cmd-I requests an LSP hover at the caret. The
                        // response paints a popup once it arrives via
                        // `poll_lsp`. No-op when no server is wired for
                        // the active document's language.
                        self.request_hover();
                        return;
                    }
                    "." => {
                        // Cmd-. is the macOS-friendly trigger for LSP
                        // completion. Ctrl-Space is the cross-platform
                        // default, but macOS users with multiple input
                        // sources have it bound to Input Source Switcher
                        // before our app ever sees it.
                        self.request_completion();
                        return;
                    }
                    "c" => {
                        self.copy_selection();
                        return;
                    }
                    "v" => {
                        self.paste_clipboard();
                        return;
                    }
                    "x" => {
                        self.cut_selection();
                        return;
                    }
                    "z" if self.modifiers.shift_key() => {
                        self.redo();
                        return;
                    }
                    "z" if !alt => {
                        // alt-z already matched above for toggle_word_wrap
                        self.undo();
                        return;
                    }
                    "g" => {
                        // Cmd-G / Cmd-Shift-G repeat the find — works only
                        // while the find bar is open (its query is the only
                        // place a search term currently lives).
                        if self.doc().find.is_some() {
                            let shift = self.modifiers.shift_key();
                            if let Some(f) = self.doc_mut().find.as_mut() {
                                if shift {
                                    f.prev_match();
                                } else {
                                    f.next_match();
                                }
                            }
                            self.refresh_find_text();
                            self.select_current_match();
                            self.scene_dirty = true;
                            self.window.request_redraw();
                        }
                        return;
                    }
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" => {
                        if let Ok(n) = lower.parse::<usize>() {
                            self.switch_tab(n.saturating_sub(1));
                        }
                        return;
                    }
                    "/" => {
                        let prefix = comment_prefix_for(self.doc().file_path.as_deref());
                        self.doc_mut().editor.toggle_comment_lines(prefix);
                        self.text_dirty = true;
                        self.scene_dirty = true;
                        self.follow_caret = true;
                        self.mark_dirty_if_clean();
                        self.window.request_redraw();
                        return;
                    }
                    _ => {}
                }
            }
            // Cmd-Enter (with optional Shift) inserts a blank line below /
            // above. Handled here so the regular Enter handler's auto-indent
            // doesn't fire instead.
            if let Key::Named(NamedKey::Enter) = &event.logical_key {
                if self.modifiers.shift_key() {
                    self.insert_blank_line_above();
                } else {
                    self.insert_blank_line_below();
                }
                return;
            }
        }

        // When the find bar is open it captures every other key.
        if self.doc().find.is_some() {
            self.handle_find_key(event);
            return;
        }

        let shift = self.modifiers.shift_key();
        let alt = self.modifiers.alt_key();
        let cmd = is_cmd_or_ctrl(self.modifiers);

        // Embedded terminal claims keys while it has focus — including
        // Cmd-V (paste into the PTY). Other Cmd / Alt shortcuts that
        // the host owns (Cmd-S, Cmd-J, Cmd-Shift-P, …) bounce back
        // through the function returning `false`. `route_key_to_terminal`
        // is the single source of truth for which terminal-side
        // shortcuts exist.
        let terminal_active = self
            .terminal
            .as_ref()
            .is_some_and(|t| t.visible && t.focused);
        if terminal_active && !alt && self.route_key_to_terminal(&event) {
            return;
        }

        // Find-in-files panel intercepts every key while open — typing
        // edits the query, Enter runs / opens, ↑/↓ navigate, Tab flips
        // focus, Esc dismisses.
        if self.find_in_files.is_some() {
            // Cmd-V pastes the clipboard into the query input. Done
            // before the match so the localised "V" doesn't fall
            // through and get appended as a literal character on
            // non-Latin layouts.
            if cmd && shortcut_letter(&event, 'v') {
                if let Some(text) = clipboard_get() {
                    if let Some(f) = self.find_in_files.as_mut() {
                        if f.input_focused {
                            f.query.push_str(&text);
                            self.refresh_find_in_files_text();
                            self.scene_dirty = true;
                            self.window.request_redraw();
                        }
                    }
                }
                return;
            }
            match &event.logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.find_in_files = None;
                    self.scene_dirty = true;
                    self.window.request_redraw();
                    return;
                }
                Key::Named(NamedKey::Enter) => {
                    let input_focused =
                        self.find_in_files.as_ref().is_some_and(|f| f.input_focused);
                    if input_focused {
                        self.run_find_in_files();
                    } else {
                        self.open_selected_find_result();
                    }
                    return;
                }
                Key::Named(NamedKey::Tab) => {
                    if let Some(f) = self.find_in_files.as_mut() {
                        f.input_focused = !f.input_focused;
                    }
                    self.refresh_find_in_files_text();
                    self.scene_dirty = true;
                    self.window.request_redraw();
                    return;
                }
                Key::Named(NamedKey::ArrowDown) => {
                    let visible = self.find_in_files_visible_rows();
                    if let Some(f) = self.find_in_files.as_mut() {
                        f.input_focused = false;
                        f.select_next();
                        scroll_into_view(f, visible);
                    }
                    self.refresh_find_in_files_text();
                    self.scene_dirty = true;
                    self.window.request_redraw();
                    return;
                }
                Key::Named(NamedKey::ArrowUp) => {
                    let visible = self.find_in_files_visible_rows();
                    if let Some(f) = self.find_in_files.as_mut() {
                        f.input_focused = false;
                        f.select_prev();
                        scroll_into_view(f, visible);
                    }
                    self.refresh_find_in_files_text();
                    self.scene_dirty = true;
                    self.window.request_redraw();
                    return;
                }
                Key::Named(NamedKey::Backspace) => {
                    if let Some(f) = self.find_in_files.as_mut() {
                        if f.input_focused {
                            f.query.pop();
                        }
                    }
                    self.refresh_find_in_files_text();
                    self.scene_dirty = true;
                    self.window.request_redraw();
                    return;
                }
                _ => {
                    // Printable input → append to query when input is
                    // focused. Modifiers (cmd/ctrl/alt) other than
                    // shift fall through so Cmd-Shift-F can re-toggle.
                    if cmd || alt {
                        // Let the outer matcher handle it.
                    } else if let Some(text) = event.text.as_deref() {
                        if let Some(f) = self.find_in_files.as_mut() {
                            if f.input_focused && !text.is_empty() {
                                f.query.push_str(text);
                                self.refresh_find_in_files_text();
                                self.scene_dirty = true;
                                self.window.request_redraw();
                                return;
                            }
                        }
                    }
                }
            }
        }

        // File-tree sidebar grabs the navigation keys while it has
        // focus. Other keystrokes (Cmd-S, find shortcuts, typing into
        // a focused editor) fall through so the sidebar stays a
        // *soft* focus — it doesn't steal everything just by being
        // visible. Esc returns focus to the editor without hiding
        // the panel; Cmd-B still toggles visibility from anywhere.
        if self.file_tree.focused && self.file_tree.visible && !cmd && !alt {
            match &event.logical_key {
                Key::Named(NamedKey::ArrowDown) => {
                    self.file_tree.select_next();
                    self.scroll_selected_into_view();
                    self.scene_dirty = true;
                    self.window.request_redraw();
                    return;
                }
                Key::Named(NamedKey::ArrowUp) => {
                    self.file_tree.select_prev();
                    self.scroll_selected_into_view();
                    self.scene_dirty = true;
                    self.window.request_redraw();
                    return;
                }
                Key::Named(NamedKey::Enter) => {
                    match self.file_tree.activate_selected() {
                        ClickResult::Nothing => {
                            self.refresh_file_tree_text();
                            self.scroll_selected_into_view();
                            self.scene_dirty = true;
                            self.window.request_redraw();
                        }
                        ClickResult::OpenFile(path) => {
                            self.open_path(path);
                        }
                    }
                    return;
                }
                Key::Named(NamedKey::Escape) => {
                    self.file_tree.focused = false;
                    self.scene_dirty = true;
                    self.window.request_redraw();
                    return;
                }
                _ => {}
            }
        }

        // Completion popup intercepts navigation + accept + dismiss keys.
        // Runs before the main matcher so Enter accepts the suggestion
        // instead of inserting a newline, etc.
        if self.completion.is_some() {
            match &event.logical_key {
                Key::Named(NamedKey::ArrowDown) => {
                    self.completion_next();
                    self.scene_dirty = true;
                    self.window.request_redraw();
                    return;
                }
                Key::Named(NamedKey::ArrowUp) => {
                    self.completion_prev();
                    self.scene_dirty = true;
                    self.window.request_redraw();
                    return;
                }
                Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Tab)
                    if self.accept_completion() =>
                {
                    return;
                }
                Key::Named(NamedKey::Escape) => {
                    self.completion = None;
                    self.scene_dirty = true;
                    self.window.request_redraw();
                    return;
                }
                _ => {}
            }
        }

        let mut text_changed = true;
        let handled = match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                // Escape dismisses the hover popup first (when one is open);
                // otherwise it collapses the multi-cursor selection set.
                if self.hover_popup.is_some() {
                    self.hover_popup = None;
                } else {
                    self.collapse_selection_to_primary();
                }
                text_changed = false;
                true
            }
            Key::Named(NamedKey::Backspace) => {
                if cmd {
                    self.doc_mut().editor.delete_to_line_start();
                } else if alt {
                    self.doc_mut().editor.delete_word_left();
                } else {
                    self.doc_mut().editor.backspace();
                }
                true
            }
            Key::Named(NamedKey::Delete) => {
                if cmd {
                    self.doc_mut().editor.delete_to_line_end();
                } else if alt {
                    self.doc_mut().editor.delete_word_right();
                } else {
                    self.doc_mut().editor.delete_forward();
                }
                true
            }
            Key::Named(NamedKey::Enter) => {
                // Auto-indent: copy the leading whitespace of the current
                // line into the new one. `insert_newline` uses the buffer's
                // detected LF/CRLF so we mimic the same here.
                let indent = self.current_line_indent();
                if indent.is_empty() {
                    self.doc_mut().editor.insert_newline();
                } else {
                    let le = self.doc().editor.buffer().line_ending().as_str();
                    let payload = format!("{le}{indent}");
                    self.doc_mut().editor.insert(&payload);
                }
                true
            }
            Key::Named(NamedKey::Space) => {
                // Ctrl-Space triggers LSP completion — standard
                // cross-editor shortcut. (Cmd-Space is Spotlight on
                // macOS, so we deliberately don't bind Cmd-Space.)
                if self.modifiers.control_key() {
                    self.request_completion();
                    text_changed = false;
                    true
                } else {
                    self.doc_mut().editor.insert(" ");
                    true
                }
            }
            Key::Named(NamedKey::Tab) => {
                let primary = self.doc().editor.selections().primary();
                let multi_line = !primary.is_cursor() && {
                    let buf = self.doc().editor.buffer();
                    let sl = buf.char_to_position(primary.start()).line;
                    let el = buf.char_to_position(primary.end()).line;
                    sl != el
                };
                if shift {
                    let indent_size = self.tab_spaces.len().max(1);
                    self.doc_mut().editor.outdent_lines(indent_size);
                } else if multi_line {
                    let spaces = self.tab_spaces.clone();
                    self.doc_mut().editor.indent_lines(&spaces);
                } else {
                    let spaces = self.tab_spaces.clone();
                    self.doc_mut().editor.insert(&spaces);
                }
                true
            }
            Key::Named(NamedKey::ArrowLeft) => {
                if cmd {
                    self.doc_mut().editor.move_line_start(shift);
                } else if alt {
                    self.doc_mut().editor.move_word_left(shift);
                } else {
                    self.doc_mut().editor.move_left(shift);
                }
                text_changed = false;
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                if cmd {
                    self.doc_mut().editor.move_line_end(shift);
                } else if alt {
                    self.doc_mut().editor.move_word_right(shift);
                } else {
                    self.doc_mut().editor.move_right(shift);
                }
                text_changed = false;
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                if cmd {
                    self.doc_mut().editor.move_buffer_start(shift);
                } else {
                    self.doc_mut().editor.move_up(shift);
                }
                text_changed = false;
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                if cmd {
                    self.doc_mut().editor.move_buffer_end(shift);
                } else {
                    self.doc_mut().editor.move_down(shift);
                }
                text_changed = false;
                true
            }
            // F12 — goto-definition for the symbol at the caret (LSP).
            Key::Named(NamedKey::F12) => {
                self.request_definition();
                text_changed = false;
                true
            }
            // Printable character input — winit gives us the resolved text.
            _ => match &event.text {
                Some(text) if !text.is_empty() => {
                    if !self.try_smart_bracket(text) {
                        self.doc_mut().editor.insert(text);
                    }
                    true
                }
                _ => false,
            },
        };

        if handled {
            self.text_dirty |= text_changed;
            self.scene_dirty = true;
            self.follow_caret = true;
            if text_changed && !self.doc().dirty {
                self.doc_mut().dirty = true;
                self.update_title();
                self.refresh_tabs_text();
            }
            // A text edit invalidates the hover popup's anchor; dismiss.
            if text_changed {
                self.hover_popup = None;
                // The completion popup, by contrast, follows the user
                // typing more characters — refresh its prefix instead
                // of dismissing.
                self.refresh_completion_after_edit();
            }
            // If the find bar is open, the match list now reflects the old text.
            if text_changed && self.doc().find.is_some() {
                let buffer_text = self.doc().editor.text();
                if let Some(f) = self.doc_mut().find.as_mut() {
                    f.refresh(&buffer_text);
                }
                self.refresh_find_text();
            }
            self.pending_keystroke = Some(Instant::now());
            self.window.request_redraw();
        }
    }

    /// Left-button press: tab-strip click switches tabs (or closes the tab
    /// when the press lands on its "×"); gutter click selects the whole
    /// logical line; in the editor area, single-click places a caret,
    /// double-click selects the word, triple-click selects the line.
    fn handle_mouse_press(&mut self) {
        let Some((mx, my)) = self.mouse_pos else {
            return;
        };
        self.note_activity();
        // Clicks inside the terminal pane focus it; clicks anywhere
        // else unfocus it so the editor regains the keyboard.
        if self.terminal_pane_height() > 0.0 {
            let pane = self.terminal_pane_rect();
            let in_pane = pane.contains(Point::new(mx, my));
            if let Some(term) = self.terminal.as_mut() {
                if term.focused != in_pane {
                    term.focused = in_pane;
                    self.scene_dirty = true;
                    self.window.request_redraw();
                }
            }
            if in_pane {
                return; // swallow the click — don't fall through to editor
            }
        }
        // Same focus contract for the file-tree sidebar: clicks inside
        // give it the keyboard, clicks elsewhere hand focus back to
        // the editor. The panel stays visible either way.
        if self.file_tree.visible {
            let in_sidebar = self.in_sidebar(mx, my);
            if self.file_tree.focused != in_sidebar {
                self.file_tree.focused = in_sidebar;
                self.scene_dirty = true;
                self.window.request_redraw();
            }
        }
        // Drag-resize handle: a click on the strip starts a drag. The
        // grab offset is the distance from the click point to the
        // current right edge so the edge stays glued to the cursor
        // through the drag (otherwise the sidebar would snap a few
        // pixels on the first move). Tested *before* sidebar/tab/etc.
        // branches so a click squarely on the edge doesn't also fire
        // a row open.
        if self.sidebar_resize_hit(mx, my) {
            let grab_offset = mx - self.sidebar_width();
            self.sidebar_resize_drag = Some(grab_offset);
            self.last_click = None;
            return;
        }
        // Find-in-files panel claims clicks first while open — clicking
        // a result row opens that file; clicking the input row focuses
        // it; clicking outside the panel dismisses it.
        if self.find_in_files.is_some() {
            if let Some(row) = self.find_in_files_row_at(mx, my) {
                if let Some(f) = self.find_in_files.as_mut() {
                    f.selected = row;
                    f.input_focused = false;
                }
                self.open_selected_find_result();
                return;
            }
            let panel = self.find_in_files_panel_rect();
            if !panel.contains(Point::new(mx, my)) {
                self.find_in_files = None;
                self.scene_dirty = true;
                self.window.request_redraw();
                return;
            }
            // Click inside the panel but not on a result — focus the
            // input so the user can keep typing.
            if let Some(f) = self.find_in_files.as_mut() {
                f.input_focused = true;
            }
            self.refresh_find_in_files_text();
            self.scene_dirty = true;
            self.window.request_redraw();
            return;
        }
        if let Some(idx) = self.tab_close_at_pixel(mx, my) {
            self.close_tab_at(idx);
            return;
        }
        if let Some(idx) = self.tab_at_pixel(mx, my) {
            self.switch_tab(idx);
            return;
        }
        if self.in_sidebar(mx, my) {
            if let Some(row) = self.file_tree_row_at(my) {
                self.handle_sidebar_click(row);
            }
            self.last_click = None;
            return;
        }
        if self.in_gutter(mx, my) {
            self.select_line_at_pixel(my);
            self.last_click = None;
            return;
        }
        let alt = self.modifiers.alt_key();
        let count = self.record_click(mx, my);
        match count {
            2 => {
                self.select_word_at_pixel(mx, my);
                return;
            }
            3 => {
                self.select_line_at_pixel(my);
                return;
            }
            _ => {}
        }
        let Some(char_idx) = self.char_at_pixel(mx, my) else {
            return;
        };
        self.drag_anchor = Some(char_idx);
        if alt {
            // Alt-click adds another caret instead of replacing.
            self.doc_mut()
                .editor
                .add_selection(Selection::cursor(char_idx));
        } else {
            self.doc_mut()
                .editor
                .set_selection(Selection::cursor(char_idx));
        }
        // Cmd-click is the canonical "go to definition" shortcut. Place
        // the caret first (so the LSP request lands at the click spot)
        // and then fire the request — the response jumps the caret onward.
        if is_cmd_or_ctrl(self.modifiers) && !alt {
            self.request_definition();
        }
        self.scene_dirty = true;
        self.follow_caret = true;
        self.window.request_redraw();
    }

    /// Update the multi-click state and return the current click count
    /// (1 / 2 / 3 / …). Successive clicks reset to 1 once they leave the
    /// time window or move further than the distance threshold.
    fn record_click(&mut self, x: f32, y: f32) -> u32 {
        let now = Instant::now();
        let count = match self.last_click {
            Some((t, (px, py), c))
                if now.duration_since(t) < MULTI_CLICK_INTERVAL && {
                    let dx = x - px;
                    let dy = y - py;
                    dx * dx + dy * dy < MULTI_CLICK_DIST_SQ
                } =>
            {
                c + 1
            }
            _ => 1,
        };
        self.last_click = Some((now, (x, y), count));
        count
    }

    /// Select the word (run of alphanumeric or underscore) containing the
    /// pixel position. A click on whitespace just places the caret.
    fn select_word_at_pixel(&mut self, x: f32, y: f32) {
        let Some(char_idx) = self.char_at_pixel(x, y) else {
            return;
        };
        let text = self.doc().editor.text();
        let chars: Vec<char> = text.chars().collect();
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let n = chars.len();
        let clamped = char_idx.min(n);
        let mut start = clamped;
        let mut end = clamped;
        while start > 0 && is_word(chars[start - 1]) {
            start -= 1;
        }
        while end < n && is_word(chars[end]) {
            end += 1;
        }
        if start == end {
            // Click landed on a non-word char — fall back to caret placement.
            self.doc_mut()
                .editor
                .set_selection(Selection::cursor(clamped));
        } else {
            self.doc_mut()
                .editor
                .set_selection(Selection::new(start, end));
        }
        self.scene_dirty = true;
        self.follow_caret = true;
        self.window.request_redraw();
    }

    /// Toggle word-wrap for the active editor. When disabled, the editor
    /// uses an effectively unbounded wrap width so long lines extend off
    /// the right edge of the viewport (horizontal scrolling is a later
    /// follow-up — for v1 the editor just clips).
    fn toggle_word_wrap(&mut self) {
        self.word_wrap = !self.word_wrap;
        let width = if self.word_wrap {
            self.gpu.surface_config.width as f32 - self.text_padding
        } else {
            // Large enough that no realistic line wraps; cosmic-text still
            // shapes the buffer correctly at this width.
            1.0e7
        };
        self.text.set_width(&mut self.font_system, width);
        self.text_dirty = true;
        self.scene_dirty = true;
        let msg = if self.word_wrap {
            "word wrap on"
        } else {
            "word wrap off"
        };
        self.set_status_flash(msg.to_string());
        self.window.request_redraw();
    }

    /// Toggle visible-whitespace mode. The underlying buffer is unchanged;
    /// `text_dirty` triggers a reshape with substitutions on the next
    /// render frame.
    fn toggle_visible_whitespace(&mut self) {
        self.visible_whitespace = !self.visible_whitespace;
        self.text_dirty = true;
        self.scene_dirty = true;
        let msg = if self.visible_whitespace {
            "whitespace visible"
        } else {
            "whitespace hidden"
        };
        self.set_status_flash(msg.to_string());
        self.window.request_redraw();
    }

    /// Cmd-D smart-select: with no real selection, expand the primary caret
    /// to the word it sits in (like double-click); with a selection, find
    /// the next occurrence of the selected text *after* the selection's
    /// end and add it as a new cursor.
    fn add_next_occurrence(&mut self) {
        let primary = self.doc().editor.selections().primary();
        if primary.is_cursor() {
            // Expand to word — same definition as double-click.
            let head = primary.head;
            let text = self.doc().editor.text();
            let chars: Vec<char> = text.chars().collect();
            let is_word = |c: char| c.is_alphanumeric() || c == '_';
            let n = chars.len();
            let mut start = head.min(n);
            let mut end = head.min(n);
            while start > 0 && is_word(chars[start - 1]) {
                start -= 1;
            }
            while end < n && is_word(chars[end]) {
                end += 1;
            }
            if start == end {
                return;
            }
            self.doc_mut()
                .editor
                .set_selection(Selection::new(start, end));
        } else {
            let needle: String = self
                .doc()
                .editor
                .buffer()
                .slice(primary.start()..primary.end());
            let Some(range) =
                find_next_occurrence(&self.doc().editor.text(), &needle, primary.end())
            else {
                return;
            };
            self.doc_mut()
                .editor
                .add_selection(Selection::new(range.start, range.end));
        }
        self.text_dirty = false;
        self.scene_dirty = true;
        self.follow_caret = true;
        self.window.request_redraw();
    }

    /// Jump the primary caret to the next occurrence of its currently-
    /// selected text, *without* keeping the old position as another cursor.
    /// Useful when Cmd-D would have added a false positive that the user
    /// wants to skip over. Wraps past the end of the buffer.
    fn skip_to_next_occurrence(&mut self) {
        let primary = self.doc().editor.selections().primary();
        if primary.is_cursor() {
            return;
        }
        let needle: String = self
            .doc()
            .editor
            .buffer()
            .slice(primary.start()..primary.end());
        let Some(range) = find_next_occurrence(&self.doc().editor.text(), &needle, primary.end())
        else {
            return;
        };
        self.doc_mut()
            .editor
            .set_selection(Selection::new(range.start, range.end));
        self.scene_dirty = true;
        self.follow_caret = true;
        self.window.request_redraw();
    }

    /// Add a caret on the line above the primary at the same column.
    /// No-op when the primary is already on the first line; clamps the
    /// column to the new line's length.
    fn add_cursor_above(&mut self) {
        self.add_cursor_vertical(true);
    }

    /// Add a caret on the line below the primary at the same column.
    fn add_cursor_below(&mut self) {
        self.add_cursor_vertical(false);
    }

    fn add_cursor_vertical(&mut self, up: bool) {
        let editor = &self.doc().editor;
        let buffer = editor.buffer();
        // Stack from the *extreme* current cursor in the requested
        // direction, not the primary — pressing Cmd-Alt-↓ N times should
        // add N cursors below, not the same line N times. The primary's
        // column anchors the stack so wandering caret positions don't
        // drift the column from press to press.
        let target_col = buffer
            .char_to_position(editor.selections().primary().head)
            .column;
        let extreme_line = editor
            .selections()
            .iter()
            .map(|s| buffer.char_to_position(s.head).line)
            .fold(None, |acc: Option<usize>, line| {
                Some(match acc {
                    None => line,
                    Some(prev) if up => prev.min(line),
                    Some(prev) => prev.max(line),
                })
            });
        let Some(extreme_line) = extreme_line else {
            return;
        };
        let new_line = if up {
            if extreme_line == 0 {
                return;
            }
            extreme_line - 1
        } else {
            let next = extreme_line + 1;
            if next >= buffer.len_lines() {
                return;
            }
            next
        };
        let line_len = buffer
            .line(new_line)
            .map(|s| {
                s.trim_end_matches('\n')
                    .trim_end_matches('\r')
                    .chars()
                    .count()
            })
            .unwrap_or(0);
        let col = target_col.min(line_len);
        let Some(char_idx) = buffer.position_to_char(Position::new(new_line, col)) else {
            return;
        };
        self.doc_mut()
            .editor
            .add_selection(Selection::cursor(char_idx));
        self.scene_dirty = true;
        self.follow_caret = true;
        self.window.request_redraw();
    }

    /// Esc / Cmd-K behavior: drop every cursor except the primary; if the
    /// primary is a real range, collapse it to a cursor at the head.
    fn collapse_selection_to_primary(&mut self) {
        let multi = self.doc().editor.selections().has_multiple();
        let primary = self.doc().editor.selections().primary();
        if multi {
            self.doc_mut().editor.collapse_to_primary();
        } else if !primary.is_cursor() {
            self.doc_mut()
                .editor
                .set_selection(Selection::cursor(primary.head));
        } else {
            return;
        }
        self.scene_dirty = true;
        self.follow_caret = true;
        self.window.request_redraw();
    }

    /// Actual width of one monospace digit in physical pixels, measured
    /// from the gutter's shaped buffer. Falls back to the
    /// `font_size × MONOSPACE_CHAR_FACTOR` approximation only when the
    /// gutter is somehow empty.
    fn measured_char_width(&self) -> f32 {
        self.text
            .buffer
            .layout_runs()
            .flat_map(|run| run.glyphs.iter())
            .find(|g| g.w > 0.0)
            .map(|g| g.w)
            .or_else(|| {
                self.gutter_text
                    .buffer
                    .layout_runs()
                    .flat_map(|run| run.glyphs.iter())
                    .find(|g| g.w > 0.0)
                    .map(|g| g.w)
            })
            .unwrap_or_else(|| self.text.font_size_pt() * MONOSPACE_CHAR_FACTOR * self.scale)
    }

    /// Glyph advance specifically for the terminal pane. Reading from
    /// `terminal_text` (rather than the editor's `text` / gutter) keeps
    /// the cursor's pixel position lock-stepped with whatever cosmic-
    /// text actually shaped into the pane — the chrome and the pane
    /// can land on different monospace faces even when both ask for
    /// `Family::Monospace`, and the editor's set_content_rich shaping
    /// can pick a different face than the pane's rich-span shaping.
    /// Falls back to [`measured_char_width`](Self::measured_char_width)
    /// before the pane has shaped anything (spawn time, hidden pane).
    fn terminal_measured_char_width(&self) -> f32 {
        self.terminal_text
            .buffer
            .layout_runs()
            .flat_map(|run| run.glyphs.iter())
            .find(|g| g.w > 0.0)
            .map(|g| g.w)
            .unwrap_or_else(|| self.measured_char_width())
    }

    /// Vertical advance between terminal rows, in physical pixels.
    /// Mirrors `terminal_measured_char_width` for the Y axis — the
    /// cell height the cursor block and the cell-count math need has
    /// to match the line_height cosmic-text actually used when it
    /// shaped the pane's rich spans, not what the chrome's TextStack
    /// thinks its own line height is.
    fn terminal_measured_line_height(&self) -> f32 {
        self.terminal_text
            .buffer
            .layout_runs()
            .next()
            .map(|run| run.line_height)
            .filter(|h| *h > 0.0)
            .unwrap_or_else(|| self.line_height())
    }

    /// Find the matching bracket for whichever bracket the primary caret
    /// sits next to. Looks at the char right of the caret first, then the
    /// char left of it. Returns `(this_pos, match_pos)` or `None` when
    /// the caret isn't adjacent to a bracket or the match is missing.
    fn matching_bracket_positions(&self) -> Option<(usize, usize)> {
        let primary = self.doc().editor.selections().primary();
        if !primary.is_cursor() {
            return None;
        }
        let head = primary.head;
        let text = self.doc().editor.text();
        let chars: Vec<char> = text.chars().collect();
        let pair_of = |c: char| -> Option<(char, bool)> {
            match c {
                '(' => Some((')', true)),
                '[' => Some((']', true)),
                '{' => Some(('}', true)),
                ')' => Some(('(', false)),
                ']' => Some(('[', false)),
                '}' => Some(('{', false)),
                _ => None,
            }
        };

        let candidate = (head < chars.len())
            .then(|| pair_of(chars[head]).map(|(t, f)| (head, chars[head], t, f)))
            .flatten()
            .or_else(|| {
                (head > 0)
                    .then(|| {
                        pair_of(chars[head - 1]).map(|(t, f)| (head - 1, chars[head - 1], t, f))
                    })
                    .flatten()
            });
        let (pos, this, target, forward) = candidate?;

        let mut depth = 1i32;
        if forward {
            let mut i = pos + 1;
            while i < chars.len() {
                if chars[i] == this {
                    depth += 1;
                } else if chars[i] == target {
                    depth -= 1;
                    if depth == 0 {
                        return Some((pos, i));
                    }
                }
                i += 1;
            }
        } else {
            let mut i = pos;
            while i > 0 {
                i -= 1;
                if chars[i] == this {
                    depth += 1;
                } else if chars[i] == target {
                    depth -= 1;
                    if depth == 0 {
                        return Some((pos, i));
                    }
                }
            }
        }
        None
    }

    /// Touch `last_interaction` so the caret stays solid through the
    /// current burst of typing or clicking.
    fn note_activity(&mut self) {
        self.last_interaction = Instant::now();
        if !self.caret_visible {
            self.caret_visible = true;
            self.scene_dirty = true;
        }
    }

    /// Heartbeat from the blink timer. Skips toggling while the user is
    /// still mid-interaction; otherwise flips visibility and redraws.
    fn tick_caret(&mut self) {
        if self.last_interaction.elapsed() < CARET_BLINK_PAUSE {
            if !self.caret_visible {
                self.caret_visible = true;
                self.scene_dirty = true;
                self.window.request_redraw();
            }
            return;
        }
        self.caret_visible = !self.caret_visible;
        self.scene_dirty = true;
        self.window.request_redraw();
    }

    /// Select every character in the active document.
    fn select_all(&mut self) {
        let len = self.doc().editor.buffer().len_chars();
        self.doc_mut().editor.set_selection(Selection::new(0, len));
        self.scene_dirty = true;
        self.window.request_redraw();
    }

    /// Step back one undo snapshot on the active document. Refreshes
    /// scrollbars and the title's "•" indicator.
    fn undo(&mut self) {
        if self.doc_mut().editor.undo() {
            self.text_dirty = true;
            self.scene_dirty = true;
            self.follow_caret = true;
            self.mark_dirty_if_clean();
            self.window.request_redraw();
        }
    }

    /// Step forward one undo snapshot — mirror of [`undo`](Self::undo).
    fn redo(&mut self) {
        if self.doc_mut().editor.redo() {
            self.text_dirty = true;
            self.scene_dirty = true;
            self.follow_caret = true;
            self.mark_dirty_if_clean();
            self.window.request_redraw();
        }
    }

    /// Copy the primary selection's text, or the primary cursor's whole
    /// line (including its newline) when there's no selection.
    fn copy_selection(&self) {
        let primary = self.doc().editor.selections().primary();
        let text = if primary.is_cursor() {
            let pos = self.doc().editor.buffer().char_to_position(primary.head);
            self.doc()
                .editor
                .buffer()
                .line(pos.line)
                .unwrap_or_default()
        } else {
            self.doc()
                .editor
                .buffer()
                .slice(primary.start()..primary.end())
        };
        clipboard_set(&text);
    }

    /// Copy + delete. Cursor-only cuts the whole line; a real selection cuts
    /// just its span.
    fn cut_selection(&mut self) {
        self.copy_selection();
        let primary = self.doc().editor.selections().primary();
        if primary.is_cursor() {
            self.doc_mut().editor.delete_line();
        } else {
            self.doc_mut().editor.insert("");
        }
        self.text_dirty = true;
        self.scene_dirty = true;
        self.follow_caret = true;
        self.mark_dirty_if_clean();
        self.window.request_redraw();
    }

    /// Insert clipboard contents at every cursor.
    fn paste_clipboard(&mut self) {
        let Some(text) = clipboard_get() else {
            return;
        };
        self.doc_mut().editor.insert(&text);
        self.text_dirty = true;
        self.scene_dirty = true;
        self.follow_caret = true;
        self.mark_dirty_if_clean();
        self.window.request_redraw();
    }

    /// Mark the active document dirty if it wasn't already, and refresh
    /// the title / tab strip so the "•" indicator appears.
    fn mark_dirty_if_clean(&mut self) {
        if !self.doc().dirty {
            self.doc_mut().dirty = true;
            self.update_title();
            self.refresh_tabs_text();
        }
    }

    /// Cmd-Enter: insert a blank line below the primary's current line and
    /// move the caret onto it with the same leading indent.
    fn insert_blank_line_below(&mut self) {
        let indent = self.current_line_indent();
        let head = self.doc().editor.selections().primary().head;
        let buffer = self.doc().editor.buffer();
        let line = buffer.char_to_position(head).line;
        let line_text = buffer.line(line).unwrap_or_default();
        let content_chars = line_text
            .trim_end_matches('\n')
            .trim_end_matches('\r')
            .chars()
            .count();
        let Some(line_end) = buffer.position_to_char(Position::new(line, content_chars)) else {
            return;
        };
        let le = buffer.line_ending().as_str().to_string();
        self.doc_mut()
            .editor
            .set_selection(Selection::cursor(line_end));
        let payload = format!("{le}{indent}");
        self.doc_mut().editor.insert(&payload);
        self.text_dirty = true;
        self.scene_dirty = true;
        self.follow_caret = true;
        self.mark_dirty_if_clean();
        self.window.request_redraw();
    }

    /// Cmd-Shift-Enter: insert a blank line above and land the caret on it.
    fn insert_blank_line_above(&mut self) {
        let indent = self.current_line_indent();
        let head = self.doc().editor.selections().primary().head;
        let buffer = self.doc().editor.buffer();
        let line = buffer.char_to_position(head).line;
        let Some(line_start) = buffer.position_to_char(Position::new(line, 0)) else {
            return;
        };
        let le = buffer.line_ending().as_str().to_string();
        let le_chars = le.chars().count();
        self.doc_mut()
            .editor
            .set_selection(Selection::cursor(line_start));
        let payload = format!("{indent}{le}");
        self.doc_mut().editor.insert(&payload);
        // After insert, caret is past the newline (start of the *original*
        // line, now shifted down). Step back over the newline so the user
        // lands on the new blank line, after the indent.
        for _ in 0..le_chars {
            self.doc_mut().editor.move_left(false);
        }
        self.text_dirty = true;
        self.scene_dirty = true;
        self.follow_caret = true;
        self.mark_dirty_if_clean();
        self.window.request_redraw();
    }

    /// Smarter bracket / quote handling. Returns `true` when the input was
    /// consumed; `false` means the caller should fall through to a plain
    /// `editor.insert(text)`.
    ///
    /// Cases handled:
    /// 1. **Overtype** — typing `)`, `]`, or `}` when the next char already
    ///    matches just moves the caret right, instead of doubling up.
    /// 2. **Wrap selection** — typing an opener (`(`, `[`, `{`, quote)
    ///    while a non-empty selection exists wraps it: `foo` → `(foo)`,
    ///    with the selection preserved on the inner text.
    /// 3. **Auto-pair** (existing) — typing an opener at a cursor inserts
    ///    the matching closer and steps back one char.
    fn try_smart_bracket(&mut self, text: &str) -> bool {
        if text.chars().count() != 1 {
            return false;
        }
        let c = text.chars().next().unwrap();

        // 1. Overtype: typing a closer right before the same closer.
        if matches!(c, ')' | ']' | '}') {
            if let Some(next) = self.char_after_primary() {
                if next == c {
                    self.doc_mut().editor.move_right(false);
                    return true;
                }
            }
        }

        // 2. Wrap selection.
        let primary = self.doc().editor.selections().primary();
        if !primary.is_cursor() {
            if let Some(closer) = matching_closer(c) {
                let start = primary.start();
                let end = primary.end();
                let selected = self.doc().editor.buffer().slice(start..end);
                let inner_len = selected.chars().count();
                let wrapped = format!("{c}{selected}{closer}");
                self.doc_mut().editor.insert(&wrapped);
                // `insert` collapses to a cursor at the end of the
                // inserted text; restore the selection on the inner
                // content (one char past the opener through one char
                // before the closer).
                let inner_start = start + 1;
                let inner_end = inner_start + inner_len;
                self.doc_mut()
                    .editor
                    .set_selection(Selection::new(inner_start, inner_end));
                return true;
            }
        }

        // 3. Auto-pair the opener.
        if let Some(pair) = auto_pair(text) {
            self.doc_mut().editor.insert(pair);
            self.doc_mut().editor.move_left(false);
            return true;
        }

        false
    }

    /// Char immediately to the right of the primary caret, or `None` at
    /// end-of-buffer. Iterates `chars()` once — fine for the bracket-
    /// overtype check which fires at most once per keystroke.
    fn char_after_primary(&self) -> Option<char> {
        let head = self.doc().editor.selections().primary().head;
        self.doc().editor.text().chars().nth(head)
    }

    /// Leading whitespace of the line where the primary caret currently
    /// sits. Used by auto-indent to copy the same indent onto the new line
    /// when the user presses Enter.
    fn current_line_indent(&self) -> String {
        let head = self.doc().editor.selections().primary().head;
        let pos = self.doc().editor.buffer().char_to_position(head);
        let Some(line) = self.doc().editor.buffer().line(pos.line) else {
            return String::new();
        };
        line.chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect()
    }

    /// Is the physical-pixel point inside the gutter strip?
    fn in_gutter(&self, x: f32, y: f32) -> bool {
        let bottom = self.gpu.surface_config.height as f32 - STATUS_BAR_HEIGHT_DIP * self.scale;
        x >= self.sidebar_width() && x < self.text_inset_x && y >= self.text_inset_y && y < bottom
    }

    /// Current width of the file-tree sidebar in physical pixels, or 0
    /// when it's hidden. Helper for the layout maths below.
    fn sidebar_width(&self) -> f32 {
        if self.file_tree.visible {
            self.sidebar_width_dip * self.scale
        } else {
            0.0
        }
    }

    /// `true` if the physical-pixel point lies on the resize handle —
    /// a thin strip centred on the sidebar's right edge that the user
    /// drags to widen / narrow the panel. Returns false when the
    /// sidebar is hidden.
    fn sidebar_resize_hit(&self, x: f32, y: f32) -> bool {
        if !self.file_tree.visible {
            return false;
        }
        let edge = self.sidebar_width();
        let half = SIDEBAR_RESIZE_HANDLE_DIP * self.scale * 0.5;
        let in_strip = x >= edge - half && x <= edge + half;
        if !in_strip {
            return false;
        }
        let sidebar_top = TAB_BAR_HEIGHT_DIP * self.scale;
        let sidebar_bottom = self.editor_bottom_y();
        y >= sidebar_top && y < sidebar_bottom
    }

    /// Update the sidebar width from a mouse position while a resize
    /// drag is in flight. `grab_offset` is the distance the mouse was
    /// from the right edge when the drag started — preserving it
    /// stops the edge from snapping to the cursor on the first frame.
    /// Width is clamped into [`SIDEBAR_MIN_WIDTH_DIP`,
    /// `SIDEBAR_MAX_WIDTH_DIP`] so the panel can't drag to zero (use
    /// Cmd-B to hide it) or eat the entire window.
    fn update_sidebar_width_from_drag(&mut self, mx: f32, grab_offset: f32) {
        let target_edge_px = (mx - grab_offset).max(0.0);
        let target_dip = target_edge_px / self.scale;
        let clamped = target_dip.clamp(SIDEBAR_MIN_WIDTH_DIP, SIDEBAR_MAX_WIDTH_DIP);
        if (clamped - self.sidebar_width_dip).abs() < 0.5 {
            return;
        }
        self.sidebar_width_dip = clamped;
        self.recompute_text_inset();
        self.scene_dirty = true;
        self.window.request_redraw();
    }

    /// Is the physical-pixel point inside the file-tree sidebar?
    fn in_sidebar(&self, x: f32, y: f32) -> bool {
        if !self.file_tree.visible {
            return false;
        }
        let sidebar_top = TAB_BAR_HEIGHT_DIP * self.scale;
        let sidebar_bottom =
            self.gpu.surface_config.height as f32 - STATUS_BAR_HEIGHT_DIP * self.scale;
        x >= 0.0 && x < self.sidebar_width() && y >= sidebar_top && y < sidebar_bottom
    }

    /// Bounds rect for the file-tree sidebar's body, in physical pixels.
    /// Returns zero-area when the sidebar is hidden.
    fn sidebar_rect(&self) -> Rect {
        let top = TAB_BAR_HEIGHT_DIP * self.scale;
        let bottom = self.editor_bottom_y();
        Rect::new(0.0, top, self.sidebar_width(), (bottom - top).max(0.0))
    }

    /// Toggle the sidebar's visibility and recompute the editor's left
    /// inset so the gutter + text shift to make room (or reclaim it).
    /// Showing the sidebar also gives it keyboard focus and seeds the
    /// selection at row 0 if nothing was selected before, so a user
    /// who toggles in via Cmd-B can immediately arrow-key around.
    fn toggle_file_tree(&mut self) {
        self.file_tree.visible = !self.file_tree.visible;
        self.recompute_text_inset();
        if self.file_tree.visible {
            self.refresh_file_tree_text();
            self.file_tree.focused = true;
            if self.file_tree.selected.is_none() && !self.file_tree.nodes.is_empty() {
                self.file_tree.selected = Some(0);
            }
            self.scroll_selected_into_view();
        } else {
            // Hidden panel cannot be focused — clear it so the editor
            // gets keyboard input on the next keystroke.
            self.file_tree.focused = false;
        }
        self.scene_dirty = true;
        self.window.request_redraw();
    }

    /// Scroll the file-tree viewport so the selected row sits inside
    /// the visible band. Called after every keyboard move and when the
    /// panel re-opens with a stale selection.
    fn scroll_selected_into_view(&mut self) {
        let Some(idx) = self.file_tree.selected else {
            return;
        };
        let line_h = self.line_height();
        if line_h <= 0.0 {
            return;
        }
        let row_top = (idx as f32) * line_h;
        let row_bot = row_top + line_h;
        let view_top = self.file_tree.scroll_y;
        let body_h = (self.editor_bottom_y() - TAB_BAR_HEIGHT_DIP * self.scale).max(line_h);
        let view_bot = view_top + body_h;
        if row_top < view_top {
            self.file_tree.scroll_y = row_top;
        } else if row_bot > view_bot {
            self.file_tree.scroll_y = row_bot - body_h;
        }
        // Don't let the scroll go negative when the content is short
        // enough to fit in view.
        let max_scroll = ((self.file_tree.nodes.len() as f32) * line_h - body_h).max(0.0);
        self.file_tree.scroll_y = self.file_tree.scroll_y.clamp(0.0, max_scroll);
    }

    /// Recompute `text_inset_x` and the wrap width on the editor text
    /// stack — call after anything that changes the sidebar's width
    /// (toggle, scale change, font resize, drag-resize). The sidebar
    /// text stack itself stays at `NO_WRAP_WIDTH_PX` so long filenames
    /// clip at the sidebar's right edge instead of wrapping onto a
    /// second visible row (which would break the row-index hit-test
    /// and the keyboard-selection highlight).
    fn recompute_text_inset(&mut self) {
        let font = self.text.font_size_pt();
        let gutter_width = gutter_outer_width(font, self.scale);
        let right_pad = TEXT_INSET_DIP * self.scale;
        self.text_inset_x = self.sidebar_width() + gutter_width;
        self.text_padding = self.text_inset_x + right_pad;
        let surface_w = self.gpu.surface_config.width as f32;
        self.text.set_width(
            &mut self.font_system,
            (surface_w - self.text_padding).max(0.0),
        );
    }

    /// Recompute the workspace-wide git status. Runs libgit2 once over
    /// the repo at `file_tree.root` and caches the result on `self` —
    /// the next `refresh_file_tree_text` call paints the markers from
    /// this cache. Outside a repo the helper returns an empty map and
    /// the markers silently disappear.
    fn refresh_workspace_git_status(&mut self) {
        self.workspace_git_status = git::compute_workspace_status(&self.file_tree.root);
        self.workspace_git_dir_status =
            git::aggregate_dirs(&self.workspace_git_status, &self.file_tree.root);
    }

    /// Re-read `package.json`'s `"scripts"` map so the next time the
    /// palette opens, its entries reflect the current state. Cheap —
    /// one file read plus serde parse — so it runs on every
    /// `FileTreeChanged` alongside the git-status refresh.
    fn refresh_npm_scripts(&mut self) {
        self.npm_scripts = scripts::read_scripts(&self.file_tree.root);
    }

    /// Re-detect whether the workspace is a Flutter project. Pubspec
    /// edits (a Flutter SDK dep getting added or removed) flow through
    /// `FileTreeChanged` just like package.json does.
    fn refresh_flutter_project(&mut self) {
        self.flutter_project = flutter::detect_flutter(&self.file_tree.root);
        // A pubspec edit can also flip a workspace into a Flutter
        // project (`flutter create .`) — kick off a device refresh
        // when that happens so the palette is ready next open.
        if self.flutter_project.is_some() {
            self.refresh_flutter_devices_async();
        } else {
            self.flutter_devices.clear();
        }
    }

    /// Spawn a background thread that runs `flutter devices --machine`
    /// and posts the result back through the event loop. The fetch
    /// takes 1–3 seconds typically; doing it asynchronously keeps the
    /// palette snappy. Safe to fire multiple times in flight — the
    /// last refresh to arrive wins.
    fn refresh_flutter_devices_async(&self) {
        let proxy = self.flash_proxy.clone();
        std::thread::spawn(move || {
            let devices = flutter::list_devices();
            let _ = proxy.send_event(AppEvent::FlutterDevicesRefreshed(devices));
        });
    }

    /// Rebuild the file-tree text stack from the current node list.
    /// Indents nest visually via leading spaces; the marker `▾`/`▸`
    /// distinguishes expanded vs collapsed directories; files get a
    /// single-character git-status suffix when the workspace's status
    /// cache has an entry for them.
    fn refresh_file_tree_text(&mut self) {
        // Build per-row body text (indent + marker + name) up front so
        // the rich-span builder below can borrow stable `&str` slices
        // from each row's owned String. Status (Option<FileGitStatus>)
        // rides alongside the body so the suffix gets a coloured span.
        let row_data: Vec<(String, Option<git::FileGitStatus>)> = self
            .file_tree
            .nodes
            .iter()
            .map(|node| {
                let mut s = String::new();
                // Two spaces per depth level keeps the indent crisp
                // without burning columns on bigger files.
                for _ in 0..node.depth {
                    s.push_str("  ");
                }
                let marker = match node.kind {
                    NodeKind::Directory { expanded: true } => "▾ ",
                    NodeKind::Directory { expanded: false } => "▸ ",
                    NodeKind::File => "  ",
                };
                s.push_str(marker);
                s.push_str(&node.name);
                // Files use the per-file map; directories use the
                // aggregate (highest-priority status anywhere in
                // their subtree), so a `src/` row tells the user
                // "something inside is modified" before they expand.
                let status = match node.kind {
                    NodeKind::File => self.workspace_git_status.get(&node.path).copied(),
                    NodeKind::Directory { .. } => {
                        // The aggregate's canonicalised paths come
                        // from libgit2's workdir; the file tree's
                        // paths come from `read_dir`. Try both forms
                        // so macOS's `/var` → `/private/var` symlink
                        // doesn't drop a row.
                        let p = node
                            .path
                            .canonicalize()
                            .unwrap_or_else(|_| node.path.clone());
                        self.workspace_git_dir_status.get(&p).copied()
                    }
                };
                (s, status)
            })
            .collect();

        let body_attrs = || Attrs::new().family(Family::Monospace);
        let mut spans: Vec<(&str, Attrs)> = Vec::with_capacity(row_data.len() * 3);
        for (i, (body, status)) in row_data.iter().enumerate() {
            if i > 0 {
                spans.push(("\n", body_attrs()));
            }
            spans.push((body.as_str(), body_attrs()));
            if let Some(s) = status {
                // Pad with two spaces so the status letter sits a few
                // pixels off the filename — easier to read at a glance.
                spans.push(("  ", body_attrs()));
                spans.push((
                    file_git_status_label(*s),
                    body_attrs().color(file_git_status_color(*s)),
                ));
            }
        }
        self.file_tree_text
            .set_content_rich(&mut self.font_system, spans);
    }

    /// Hit-test a physical-pixel point against the sidebar's rows.
    /// Returns the node index, or `None` when the point is outside the
    /// row strip or past the loaded items.
    fn file_tree_row_at(&self, y: f32) -> Option<usize> {
        if !self.file_tree.visible {
            return None;
        }
        let top = TAB_BAR_HEIGHT_DIP * self.scale;
        let line_h = self.line_height();
        if y < top {
            return None;
        }
        let row = ((y - top + self.file_tree.scroll_y) / line_h) as usize;
        (row < self.file_tree.nodes.len()).then_some(row)
    }

    /// Forward a sidebar click to the tree's state machine, opening a
    /// file or toggling a directory accordingly. Refreshes the shaped
    /// text on any change.
    fn handle_sidebar_click(&mut self, row: usize) {
        match self.file_tree.click(row) {
            ClickResult::Nothing => {
                self.refresh_file_tree_text();
                self.scene_dirty = true;
                self.window.request_redraw();
            }
            ClickResult::OpenFile(path) => {
                self.open_path(path);
            }
        }
    }

    // ── Find-in-files (Cmd-Shift-F) ───────────────────────────────────────

    /// Open or close the find-in-files overlay. Opening starts with a
    /// blank query focused on the input row.
    fn toggle_find_in_files(&mut self) {
        if self.find_in_files.is_some() {
            self.find_in_files = None;
        } else {
            self.find_in_files = Some(FindInFiles::new());
            self.refresh_find_in_files_text();
        }
        self.scene_dirty = true;
        self.window.request_redraw();
    }

    /// Run the project-wide search with the current query and refresh
    /// the panel. Root is `file_tree.root` (which is the same project
    /// root the LSP uses).
    fn run_find_in_files(&mut self) {
        let root = self.file_tree.root.clone();
        let Some(f) = self.find_in_files.as_mut() else {
            return;
        };
        f.results = find_in_files::search(&f.query, &root);
        f.selected = 0;
        f.scroll = 0;
        // Move focus to the results list so Enter now opens; ↑/↓
        // navigate; Tab can flip back to the input to refine the query.
        f.input_focused = f.results.is_empty();
        self.refresh_find_in_files_text();
        self.scene_dirty = true;
        self.window.request_redraw();
    }

    /// Open the currently-selected match and close the panel.
    fn open_selected_find_result(&mut self) {
        let Some(f) = self.find_in_files.as_ref() else {
            return;
        };
        let Some(m) = f.results.get(f.selected) else {
            return;
        };
        let path = m.path.clone();
        let target_line = m.line;
        self.find_in_files = None;
        self.open_path(path);
        // Jump the caret to the matching line. `open_path` activates
        // the doc; resolve the line's char index and set the selection.
        let buf = self.docs[self.active].editor.buffer();
        if let Some(char_idx) = buf.position_to_char(Position::new(target_line, 0)) {
            self.docs[self.active]
                .editor
                .set_selection(Selection::cursor(char_idx));
            self.follow_caret = true;
            self.scene_dirty = true;
            self.window.request_redraw();
        }
    }

    /// Reshape the find-in-files panel's text from the current state.
    /// Only the visible window of result rows is fed to cosmic-text —
    /// scrolling through a 500-match list stays cheap because the
    /// shape budget is bounded by `find_in_files_visible_rows()`.
    /// Layout: row 0 = "Search: <query>", row 1 = status, row 2 blank,
    /// row 3..N = results inside the visible window.
    fn refresh_find_in_files_text(&mut self) {
        let visible = self.find_in_files_visible_rows();
        let workspace = self.file_tree.root.clone();
        let text = {
            let Some(f) = self.find_in_files.as_ref() else {
                return;
            };
            let mut s = String::with_capacity(64 + visible * 80);
            // Caret marker on the input line shows focus state at a
            // glance: solid ▎ when typing, dim when results have focus.
            let caret = if f.input_focused { '▎' } else { ' ' };
            s.push_str(&format!("Search: {}{caret}\n", f.query));
            if f.results.is_empty() {
                s.push_str(if f.query.is_empty() {
                    "type a query and press Enter\n"
                } else {
                    "no matches (press Enter to search)\n"
                });
            } else {
                let end = (f.scroll + visible).min(f.results.len());
                s.push_str(&format!(
                    "{} match(es), showing {}-{} — ↑↓ navigates, wheel pages, Enter opens, Esc closes\n",
                    f.results.len(),
                    f.scroll + 1,
                    end,
                ));
            }
            s.push('\n');
            let end = (f.scroll + visible).min(f.results.len());
            for m in &f.results[f.scroll..end] {
                let rel = m.path.strip_prefix(&workspace).unwrap_or(&m.path).display();
                let trimmed = m.line_text.trim();
                s.push_str(&format!("{rel}:{} {trimmed}\n", m.line + 1));
            }
            s
        };
        self.find_in_files_text
            .set_content(&mut self.font_system, &text);
    }

    // ── Embedded terminal (Cmd-J) ─────────────────────────────────────────

    /// Toggle the bottom terminal pane. Spawns the shell lazily on
    /// first open; subsequent toggles just flip visibility so
    /// scrollback survives. Focus follows visibility — showing the
    /// pane grabs the keyboard.
    fn toggle_terminal(&mut self) {
        if let Some(term) = self.terminal.as_mut() {
            term.visible = !term.visible;
            term.focused = term.visible;
            self.recompute_text_inset();
            // Window may have resized while the pane was hidden, so
            // re-sync cell count whenever we bring it back into view.
            if self.terminal.as_ref().is_some_and(|t| t.visible) {
                self.resync_terminal_cells();
            }
            self.scene_dirty = true;
            self.window.request_redraw();
            return;
        }
        // First open: spawn the shell rooted at the project root.
        let cwd = Some(self.file_tree.root.clone());
        let pane_height = TERMINAL_HEIGHT_DIP * self.scale;
        // Real cosmic-text monospace advance — see comment in
        // `resync_terminal_cells`. At spawn time `terminal_text` is
        // empty, so this falls back to the chrome's measurement;
        // `resync_terminal_cells` re-runs once the first frame has
        // shaped the pane and the value gets refined.
        let cell_w = self.terminal_measured_char_width();
        let cell_h = self.terminal_measured_line_height();
        match terminal::TerminalPane::spawn(
            self.flash_proxy.clone(),
            cwd,
            TERMINAL_INITIAL_COLS,
            TERMINAL_INITIAL_ROWS,
            cell_w,
            cell_h,
            pane_height,
        ) {
            Ok(pane) => {
                self.terminal = Some(pane);
                // Shape the (mostly-empty) pane content FIRST so the
                // measurement helpers have real glyph metrics from
                // `terminal_text` to work with. Without this,
                // `resync_terminal_cells` falls back to the chrome's
                // `measured_char_width()` / `line_height()` — which
                // can pick a different monospace face than the pane
                // and produce mis-sized cells. The visible symptom
                // is a sparse, mis-aligned first paint that snaps
                // back after a second Cmd-J toggle.
                self.refresh_terminal_text();
                // Now that `terminal_text` has been shaped, resize
                // the PTY's grid from the real metrics.
                self.resync_terminal_cells();
                self.scene_dirty = true;
                self.window.request_redraw();
            }
            Err(e) => {
                log::warn!("terminal spawn failed: {e}");
                self.set_status_flash(format!("terminal: {e}"));
            }
        }
    }

    /// Currently-rendered pane height in physical pixels, or 0 when
    /// the terminal is hidden or unspawned.
    fn terminal_pane_height(&self) -> f32 {
        match &self.terminal {
            Some(t) if t.visible => t.height_px,
            _ => 0.0,
        }
    }

    /// Bounds rect for the terminal pane, in physical pixels.
    fn terminal_pane_rect(&self) -> Rect {
        let h = self.terminal_pane_height();
        let surface_w = self.gpu.surface_config.width as f32;
        let surface_h = self.gpu.surface_config.height as f32;
        let status_h = STATUS_BAR_HEIGHT_DIP * self.scale;
        let top = (surface_h - status_h - h).max(0.0);
        Rect::new(0.0, top, surface_w, h)
    }

    /// Cursor block rect for the terminal, in physical pixels. Returns
    /// `None` when the pane is hidden or when the cursor's grid
    /// coordinates fall outside the rendered window (very large
    /// scrollback offset).
    fn terminal_cursor_rect(&self) -> Option<Rect> {
        let pane = self.terminal.as_ref()?;
        if !pane.visible {
            return None;
        }
        let panel = self.terminal_pane_rect();
        let pad = 6.0 * self.scale;
        // Use the pane's own glyph advance so the cursor lands on the
        // same pixel column the shaped text does. Reading from the
        // chrome's `text` buffer (or the 0.6 × font_size
        // approximation) drifts because cosmic-text can resolve
        // `Family::Monospace` to a different face for the pane than
        // for the editor, especially when the pane has the rich
        // colour spans the editor doesn't.
        let cell_w = self.terminal_measured_char_width();
        let cell_h = self.terminal_measured_line_height();
        let term = pane.term.lock();
        let grid = term.grid();
        // alacritty stores the cursor as a Line (signed, history-relative)
        // and Column. Translate into the visible window: a positive
        // display offset means the user scrolled up, so the live
        // cursor row sits N rows below the screen.
        use alacritty_terminal::grid::Dimensions as _;
        let display_offset = grid.display_offset() as i32;
        let cursor = grid.cursor.point;
        let row_on_screen = cursor.line.0 + display_offset;
        if row_on_screen < 0 || row_on_screen as usize >= grid.screen_lines() {
            return None;
        }
        let x = panel.min_x() + pad + (cursor.column.0 as f32) * cell_w;
        let y = panel.min_y() + pad + (row_on_screen as f32) * cell_h;
        Some(Rect::new(x, y, cell_w.max(1.0), cell_h))
    }

    /// Snapshot the terminal grid into the terminal_text TextStack
    /// for the next frame. Called from the render path when the
    /// terminal is visible and there's a wakeup pending.
    ///
    /// Per-cell foreground colours are read from the grid and folded
    /// into `(slice, Attrs)` spans so cosmic-text shapes coloured
    /// runs in one pass. Background colours and bold-weight / italic /
    /// underline attributes are *not* honoured in v1 — the pane uses
    /// a single monospace face, so weight / style / underline are
    /// follow-ups. Background colours and INVERSE *are* honoured:
    /// the background of every cell whose `bg` isn't the pane default
    /// gets emitted to [`Self::terminal_bg_quads`] so the next
    /// `rebuild_scene` can paint them under the text.
    fn refresh_terminal_text(&mut self) {
        let Some(pane) = self.terminal.as_ref() else {
            return;
        };
        use alacritty_terminal::term::cell::Flags;
        use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor};
        use terminal_palette::{brighten_named, resolve, PaletteColor};

        // Theme-driven palette. `[terminal]` in theme.toml can override
        // any of the 16 ANSI slots and the three pane sentinels;
        // missing or unparseable entries fall back to either the
        // hardcoded Tango defaults (palette slots) or the editor's
        // chrome colours (foreground / background / cursor), so the
        // pane visually stays continuous with the rest of the editor.
        let palette_ctx = build_terminal_palette(&self.theme);

        // Build (run_text, run_color) entries in one pass. A new run
        // starts when the resolved colour differs from the previous
        // cell, or when we cross a row boundary (the '\n' rides on
        // its own run so we don't accidentally extend the previous
        // colour past the line terminator).
        let mut runs: Vec<(String, PaletteColor)> = Vec::with_capacity(pane.rows * 2);
        let mut current = String::new();
        let mut current_color = palette_ctx.default_fg;

        // Per-cell background runs are gathered alongside the text
        // pass: consecutive cells in the same row with the same bg
        // get one wide quad so a stripe of bg colour is just one
        // entry. Pane origin / cell size only matter at draw time,
        // so we collect by `(row, col_start, col_end, palette)` here
        // and convert to pixel rects in `rebuild_scene`.
        let mut bg_runs: Vec<TerminalBgRun> = Vec::new();

        {
            let term = pane.term.lock();
            let grid = term.grid();
            use alacritty_terminal::grid::Dimensions as _;
            let display_offset = grid.display_offset();
            let total_rows = grid.screen_lines();
            for screen_row in 0..total_rows {
                if screen_row > 0 {
                    if !current.is_empty() {
                        runs.push((std::mem::take(&mut current), current_color));
                    }
                    runs.push(("\n".to_string(), current_color));
                }
                let line =
                    alacritty_terminal::index::Line(screen_row as i32 - display_offset as i32);

                // In-flight bg-run state for this row.
                let mut row_bg: Option<(usize, PaletteColor)> = None;

                for col in 0..grid.columns() {
                    let cell = &grid[line][alacritty_terminal::index::Column(col)];
                    let flags = cell.flags;
                    let inverse = flags.contains(Flags::INVERSE);
                    let mut fg = cell.fg;
                    // Classic xterm behaviour: BOLD brightens the
                    // named fg colour (8 → 16-colour palette upgrade).
                    // Indexed and Spec colours pass through.
                    if flags.contains(Flags::BOLD) {
                        fg = brighten_named(fg);
                    }
                    let bg = cell.bg;
                    // INVERSE swaps fg/bg roles for rendering — the
                    // glyph paints in the cell's bg colour, the cell
                    // backdrop in the fg colour.
                    let (fg_eff, bg_eff) = if inverse { (bg, fg) } else { (fg, bg) };

                    let fg_color = resolve(fg_eff, &palette_ctx);
                    if fg_color != current_color && !current.is_empty() {
                        runs.push((std::mem::take(&mut current), current_color));
                    }
                    current_color = fg_color;

                    // Background quads: skip the default Background
                    // sentinel (the pane fills that already), keep
                    // everything else. INVERSE always emits a quad
                    // because the swap means even default-fg ends up
                    // as a coloured backdrop.
                    let bg_visible =
                        inverse || !matches!(bg_eff, AnsiColor::Named(NamedColor::Background));
                    let bg_color = if bg_visible {
                        Some(resolve(bg_eff, &palette_ctx))
                    } else {
                        None
                    };

                    match (row_bg, bg_color) {
                        (Some((start, prev)), Some(c)) if prev == c => {
                            // Same colour as the in-flight run; extend.
                            row_bg = Some((start, prev));
                            let _ = col;
                        }
                        (Some((start, prev)), Some(c)) => {
                            // Different colour — close the old run, start a new one.
                            bg_runs.push(TerminalBgRun {
                                row: screen_row,
                                col_start: start,
                                col_end: col,
                                color: prev,
                            });
                            row_bg = Some((col, c));
                        }
                        (Some((start, prev)), None) => {
                            // Bg became default — close.
                            bg_runs.push(TerminalBgRun {
                                row: screen_row,
                                col_start: start,
                                col_end: col,
                                color: prev,
                            });
                            row_bg = None;
                        }
                        (None, Some(c)) => {
                            row_bg = Some((col, c));
                        }
                        (None, None) => {}
                    }

                    let c = cell.c;
                    // Drop the trailing space of a wide-char's right
                    // half (alacritty stores a 0 there).
                    if c == '\0' {
                        current.push(' ');
                    } else {
                        current.push(c);
                    }
                }
                // End-of-row flush for the in-flight bg run.
                if let Some((start, color)) = row_bg {
                    bg_runs.push(TerminalBgRun {
                        row: screen_row,
                        col_start: start,
                        col_end: grid.columns(),
                        color,
                    });
                }
            }
            if !current.is_empty() {
                runs.push((current, current_color));
            }
        }

        self.terminal_bg_runs = bg_runs;

        // Emit spans. `Attrs::new()` defaults to `Family::SansSerif`,
        // and cosmic-text honours that per-span without merging with
        // the AttrsList's default — so every span must explicitly
        // re-state monospace or the prompt drifts into a proportional
        // font and the rendered text no longer lines up with the
        // PTY's column count. Skip `.color()` when the resolved
        // colour matches the chrome default so cosmic-text picks up
        // `TextArea.default_color` straight from the renderer.
        let spans: Vec<(&str, Attrs)> = runs
            .iter()
            .map(|(s, c)| {
                let mut attrs = Attrs::new().family(Family::Monospace);
                if *c != palette_ctx.default_fg {
                    attrs = attrs.color(Color::rgb(c.r, c.g, c.b));
                }
                (s.as_str(), attrs)
            })
            .collect();
        self.terminal_text
            .set_content_rich(&mut self.font_system, spans);
    }

    /// Translate a winit key event into the byte sequence a PTY
    /// expects for that key, and send it. Returns `true` when the
    /// key was claimed by the terminal (so the caller short-circuits
    /// the normal editor key handler).
    fn route_key_to_terminal(&mut self, event: &KeyEvent) -> bool {
        let Some(pane) = self.terminal.as_ref() else {
            return false;
        };
        if !pane.visible || !pane.focused {
            return false;
        }
        // Cmd-J still toggles the pane; let it fall through. Use the
        // physical key so layouts that remap the J position still
        // resolve here.
        if is_cmd_or_ctrl(self.modifiers) && shortcut_letter(event, 'j') {
            return false;
        }
        // Cmd-V pastes the clipboard into the PTY. Standard terminal
        // behaviour (iTerm, Apple Terminal, alacritty bind it the
        // same way).
        if is_cmd_or_ctrl(self.modifiers) && shortcut_letter(event, 'v') {
            if let Some(text) = clipboard_get() {
                if let Some(pane) = self.terminal.as_ref() {
                    pane.write(text.into_bytes());
                }
            }
            return true;
        }
        // Every other Cmd / Ctrl chord stays with the host — Cmd-S
        // saves the active doc, Cmd-Shift-P opens the palette, etc.
        // Without this guard the printable-text fallback below would
        // forward bytes like Cmd-S's pasteboard escape to the PTY.
        if is_cmd_or_ctrl(self.modifiers) {
            return false;
        }
        let bytes: Cow<'static, [u8]> = match &event.logical_key {
            Key::Named(NamedKey::Enter) => Cow::Borrowed(b"\r"),
            Key::Named(NamedKey::Backspace) => Cow::Borrowed(b"\x7f"),
            Key::Named(NamedKey::Tab) => Cow::Borrowed(b"\t"),
            Key::Named(NamedKey::Escape) => Cow::Borrowed(b"\x1b"),
            Key::Named(NamedKey::ArrowUp) => Cow::Borrowed(b"\x1b[A"),
            Key::Named(NamedKey::ArrowDown) => Cow::Borrowed(b"\x1b[B"),
            Key::Named(NamedKey::ArrowRight) => Cow::Borrowed(b"\x1b[C"),
            Key::Named(NamedKey::ArrowLeft) => Cow::Borrowed(b"\x1b[D"),
            Key::Named(NamedKey::Home) => Cow::Borrowed(b"\x1b[H"),
            Key::Named(NamedKey::End) => Cow::Borrowed(b"\x1b[F"),
            Key::Named(NamedKey::Delete) => Cow::Borrowed(b"\x1b[3~"),
            Key::Named(NamedKey::PageUp) => Cow::Borrowed(b"\x1b[5~"),
            Key::Named(NamedKey::PageDown) => Cow::Borrowed(b"\x1b[6~"),
            Key::Named(NamedKey::Space) => Cow::Borrowed(b" "),
            _ => {
                // Printable text comes via `event.text`.
                if let Some(text) = event.text.as_deref() {
                    if !text.is_empty() {
                        Cow::Owned(text.as_bytes().to_vec())
                    } else {
                        return false;
                    }
                } else {
                    return false;
                }
            }
        };
        pane.write(bytes);
        // The PTY may echo back asynchronously — we'll repaint when
        // the wakeup event arrives. Mark the scene dirty pre-emptively
        // so the caret feels responsive.
        self.scene_dirty = true;
        self.window.request_redraw();
        true
    }

    /// Geometry of the find-in-files panel — centred horizontally,
    /// pinned a third of the way down vertically.
    fn find_in_files_panel_rect(&self) -> Rect {
        let w = FIND_FILES_WIDTH_DIP * self.scale;
        let h = FIND_FILES_HEIGHT_DIP * self.scale;
        let surface_w = self.gpu.surface_config.width as f32;
        let surface_h = self.gpu.surface_config.height as f32;
        let left = ((surface_w - w) / 2.0).max(0.0);
        let top = (surface_h / 4.0).max(40.0 * self.scale);
        Rect::new(left, top, w, h)
    }

    fn find_in_files_text_origin(&self) -> (f32, f32) {
        let panel = self.find_in_files_panel_rect();
        let pad = FIND_FILES_PAD_DIP * self.scale;
        (panel.min_x() + pad, panel.min_y() + pad)
    }

    /// Selection-row rect for the currently-highlighted result. None
    /// when focus is on the input row (no result highlight) or the
    /// list is empty.
    fn find_in_files_selection_rect(&self) -> Option<Rect> {
        let f = self.find_in_files.as_ref()?;
        if f.input_focused || f.results.is_empty() {
            return None;
        }
        let panel = self.find_in_files_panel_rect();
        let pad = FIND_FILES_PAD_DIP * self.scale;
        let line_h = self.line_height();
        let visible_idx = f.selected.checked_sub(f.scroll)?;
        let row = FIND_FILES_HEADER_ROWS + visible_idx;
        let y = panel.min_y() + pad + (row as f32) * line_h;
        Some(Rect::new(panel.min_x(), y, panel.size.width, line_h))
    }

    /// Hit-test a click against the find-in-files panel. Returns the
    /// result index if the click landed on one of the result rows.
    fn find_in_files_row_at(&self, x: f32, y: f32) -> Option<usize> {
        let f = self.find_in_files.as_ref()?;
        let panel = self.find_in_files_panel_rect();
        if !panel.contains(Point::new(x, y)) {
            return None;
        }
        let pad = FIND_FILES_PAD_DIP * self.scale;
        let line_h = self.line_height();
        let local_y = y - panel.min_y() - pad;
        let row = (local_y / line_h) as i32 - FIND_FILES_HEADER_ROWS as i32;
        if row < 0 {
            return None;
        }
        let idx = row as usize + f.scroll;
        (idx < f.results.len()).then_some(idx)
    }

    /// Select the logical line under `y`. Reuses the editor's shaped layout
    /// to find which line the click landed on so wraps map correctly. The
    /// selection covers the line content plus its trailing newline.
    fn select_line_at_pixel(&mut self, y: f32) {
        let ty = y - self.text_inset_y + self.doc().scroll_y;
        // Dummy x inside the editor area so `hit` always returns Some.
        let line = match self.text.buffer.hit(1.0, ty) {
            Some(c) => c.line,
            None => return,
        };
        let buffer = self.doc().editor.buffer();
        let Some(start) = buffer.position_to_char(Position::new(line, 0)) else {
            return;
        };
        let end = if line + 1 < buffer.len_lines() {
            buffer
                .position_to_char(Position::new(line + 1, 0))
                .unwrap_or(start)
        } else {
            buffer.len_chars()
        };
        self.doc_mut()
            .editor
            .set_selection(Selection::new(start, end));
        self.scene_dirty = true;
        self.follow_caret = true;
        self.window.request_redraw();
    }

    /// Middle-button press inside the tab strip closes the tab under the
    /// cursor (common browser-tab convention). Below the strip it is ignored.
    fn handle_mouse_middle(&mut self) {
        let Some((mx, my)) = self.mouse_pos else {
            return;
        };
        if let Some(idx) = self.tab_at_pixel(mx, my) {
            self.close_tab_at(idx);
        }
    }

    /// Left-button release: end any drag.
    fn handle_mouse_release(&mut self) {
        self.drag_anchor = None;
        self.sidebar_resize_drag = None;
    }

    /// Write the active document to its path, or prompt for one with a Save As
    /// dialog when there is none.
    fn save_to_file(&mut self) {
        let path_was_new = self.doc().file_path.is_none();
        let path = match self.doc().file_path.clone() {
            Some(p) => p,
            None => match rfd::FileDialog::new().save_file() {
                Some(p) => p,
                None => return,
            },
        };
        let text = self.doc().editor.text();
        match std::fs::write(&path, &text) {
            Ok(()) => {
                log::info!("saved {}", path.display());
                let label = filename_for_flash(&path);
                let auto_reload = self.should_auto_hot_reload(&path);
                {
                    let d = self.doc_mut();
                    d.file_path = Some(path);
                    d.dirty = false;
                }
                self.update_title();
                self.refresh_tabs_text();
                if auto_reload {
                    // VSCode-style: a save of a .dart file with a
                    // live `flutter run` session triggers hot reload
                    // automatically. The terminal pane stays where
                    // it is (no focus change); the user sees the
                    // status bar flash, the app updates in seconds.
                    if let Some(t) = self.terminal.as_ref() {
                        t.write(b"r" as &[u8]);
                    }
                    self.set_status_flash(format!("saved · {label} · hot reload"));
                } else {
                    self.set_status_flash(format!("saved · {label}"));
                }
                // First save of an untitled doc: introduce it to the LSP
                // server now that it has a path. Subsequent saves are a
                // plain didSave.
                if path_was_new {
                    self.lsp_did_open_doc(self.active);
                } else {
                    self.lsp_did_save_active();
                }
                self.scene_dirty = true;
                self.window.request_redraw();
            }
            Err(e) => {
                log::error!("save failed for {}: {}", path.display(), e);
                let label = filename_for_flash(&path);
                self.set_status_flash(format!("save failed · {label} · {e}"));
            }
        }
    }

    /// Read `path` and place it in the editor. If the active document is a
    /// pristine, never-saved, never-edited scratch buffer, replace it in-place
    /// (matches VSCode's behaviour for untouched "Untitled-1"); otherwise
    /// push a new tab and activate it. I/O failure is logged AND flashed on
    /// the status bar — the editor's state is left intact.
    fn open_path(&mut self, path: PathBuf) {
        let flash_label = filename_for_flash(&path);
        // Canonicalise up front so the "is this file already open?"
        // check below treats `./src/main.rs`, `src/main.rs`, and the
        // absolute form as the same tab.
        let canon = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if let Some(idx) = self
            .docs
            .iter()
            .position(|d| d.file_path.as_deref() == Some(canon.as_path()))
        {
            // Already open — switch to that tab instead of pushing a
            // duplicate. Matches VSCode: clicking a file in the
            // sidebar twice keeps one tab.
            self.switch_tab(idx);
            self.set_status_flash(format!("opened · {flash_label}"));
            return;
        }
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                // Always store an absolute path: the LSP layer turns it into
                // a `file://` URL, which requires absolute, and tools like
                // rust-analyzer use the URI to anchor workspace lookups.
                let path = canon;
                let new_doc = Document::from_file(path, &content);
                if self.doc().is_pristine_scratch() {
                    self.docs[self.active] = new_doc;
                } else {
                    self.docs.push(new_doc);
                    self.active = self.docs.len() - 1;
                }
                self.text_dirty = true;
                self.scene_dirty = true;
                self.follow_caret = false;
                self.update_title();
                self.refresh_tabs_text();
                self.refresh_find_text();
                self.set_status_flash(format!("opened · {flash_label}"));
                self.lsp_did_open_doc(self.active);
                self.window.request_redraw();
            }
            Err(e) => {
                log::error!("could not read {}: {}", path.display(), e);
                self.set_status_flash(format!("open failed · {flash_label} · {e}"));
            }
        }
    }

    /// Prompt for *one or more* files with an Open dialog and load each in a
    /// tab. The first opened file may replace a pristine scratch (see
    /// [`open_path`](Self::open_path)); subsequent ones always push a new tab.
    /// Cancel is a no-op.
    fn open_file_dialog(&mut self) {
        let Some(paths) = rfd::FileDialog::new().pick_files() else {
            return;
        };
        for path in paths {
            self.open_path(path);
        }
    }

    /// Handle a file dragged from Finder/Explorer and dropped on the window.
    /// One DroppedFile event fires per file, so this opens exactly one tab
    /// per call.
    fn handle_dropped_file(&mut self, path: PathBuf) {
        self.open_path(path);
    }

    /// Sync the window title with the active document.
    fn update_title(&self) {
        let base = window_title(self.doc().file_path.as_deref());
        let title = if self.doc().dirty {
            format!("• {base}")
        } else {
            base
        };
        self.window.set_title(&title);
    }

    // ── tab strip ──────────────────────────────────────────────────────────

    /// Add a new empty document and make it active.
    fn open_new_tab(&mut self) {
        self.docs.push(Document::new_scratch(""));
        self.active = self.docs.len() - 1;
        self.text_dirty = true;
        self.scene_dirty = true;
        self.follow_caret = false;
        self.update_title();
        self.refresh_tabs_text();
        self.refresh_find_text();
        self.window.request_redraw();
    }

    /// Close the active tab. Prompts on unsaved changes; if it is the only
    /// tab, a fresh scratch tab replaces it (the window never becomes empty).
    fn close_active_tab(&mut self) {
        if !self.confirm_unsaved("Close tab") {
            return;
        }
        if let Some(path) = self.doc().file_path.clone() {
            self.lsp_did_close_path(&path);
        }
        self.docs.remove(self.active);
        if self.docs.is_empty() {
            self.docs.push(Document::new_scratch(""));
        }
        if self.active >= self.docs.len() {
            self.active = self.docs.len() - 1;
        }
        self.text_dirty = true;
        self.scene_dirty = true;
        self.follow_caret = false;
        self.update_title();
        self.refresh_tabs_text();
        self.refresh_find_text();
        self.window.request_redraw();
    }

    /// Activate tab `idx`. No-op if it's already active or out of range.
    fn switch_tab(&mut self, idx: usize) {
        if idx == self.active || idx >= self.docs.len() {
            return;
        }
        self.active = idx;
        self.text_dirty = true;
        self.scene_dirty = true;
        // No need to invalidate `gutter_lines` — the cache is keyed on
        // line count, not document identity, and "1..N" is the same shape
        // regardless of which document has N lines.
        // Keep the new tab's stored scroll position rather than recentering.
        self.follow_caret = false;
        self.update_title();
        self.refresh_tabs_text();
        self.refresh_find_text();
        // Flash the full path (or the label) — useful when many tabs share a
        // similar truncated label and the user wants confirmation of which
        // file landed active.
        let flash = match self.doc().file_path.as_deref() {
            Some(p) => p.display().to_string(),
            None => self.doc().label(),
        };
        self.set_status_flash(flash);
        self.window.request_redraw();
    }

    fn next_tab(&mut self) {
        if self.docs.len() <= 1 {
            return;
        }
        let next = (self.active + 1) % self.docs.len();
        self.switch_tab(next);
    }

    fn prev_tab(&mut self) {
        if self.docs.len() <= 1 {
            return;
        }
        let prev = if self.active == 0 {
            self.docs.len() - 1
        } else {
            self.active - 1
        };
        self.switch_tab(prev);
    }

    /// Tab strip rectangle (the whole strip, not one slot).
    fn tab_strip_rect(&self) -> Rect {
        Rect::new(
            0.0,
            0.0,
            self.gpu.surface_config.width as f32,
            TAB_BAR_HEIGHT_DIP * self.scale,
        )
    }

    /// Rectangle for tab `idx`, in physical pixels.
    fn tab_slot_rect(&self, idx: usize) -> Rect {
        let w = TAB_WIDTH_DIP * self.scale;
        let h = TAB_BAR_HEIGHT_DIP * self.scale;
        Rect::new(idx as f32 * w, 0.0, w, h)
    }

    /// Square close-button hit/draw region at the right edge of tab `idx`.
    fn tab_close_rect(&self, idx: usize) -> Rect {
        let slot = self.tab_slot_rect(idx);
        let w = TAB_CLOSE_W_DIP * self.scale;
        let pad = TAB_CLOSE_PAD_DIP * self.scale;
        let cy = slot.min_y() + slot.size.height * 0.5;
        Rect::new(slot.min_x() + slot.size.width - w - pad, cy - w * 0.5, w, w)
    }

    /// Tab whose close button is under a physical-pixel point.
    fn tab_close_at_pixel(&self, x: f32, y: f32) -> Option<usize> {
        (0..self.docs.len()).find(|&i| self.tab_close_rect(i).contains(Point::new(x, y)))
    }

    /// Close the tab at `idx`. Switches to it first so the unsaved-changes
    /// prompt (if any) is unambiguous about which document it's asking about.
    fn close_tab_at(&mut self, idx: usize) {
        if idx >= self.docs.len() {
            return;
        }
        if idx != self.active {
            self.switch_tab(idx);
        }
        self.close_active_tab();
    }

    /// Tab index under a physical-pixel point, or `None` if the point is not
    /// in the strip area (or past the last tab).
    fn tab_at_pixel(&self, x: f32, y: f32) -> Option<usize> {
        let strip_h = TAB_BAR_HEIGHT_DIP * self.scale;
        if !(0.0..strip_h).contains(&y) {
            return None;
        }
        let slot_w = TAB_WIDTH_DIP * self.scale;
        if x < 0.0 {
            return None;
        }
        let idx = (x / slot_w) as usize;
        if idx >= self.docs.len() {
            return None;
        }
        Some(idx)
    }

    // ── status bar ─────────────────────────────────────────────────────────

    /// Status bar backdrop rectangle, in physical pixels.
    fn status_bar_rect(&self) -> Rect {
        let surface_w = self.gpu.surface_config.width as f32;
        let surface_h = self.gpu.surface_config.height as f32;
        let h = STATUS_BAR_HEIGHT_DIP * self.scale;
        Rect::new(0.0, surface_h - h, surface_w, h)
    }

    /// Re-shape the gutter to cover every buffer line. A no-op when the
    /// line count is unchanged since the last refresh — typing within an
    /// Recompute the active document's per-line git status against
    /// HEAD when the editor revision has moved. Gated on revision so
    /// tab switches (same revision) are free; called once per render
    /// pass where text_dirty was set.
    fn refresh_git_status(&mut self, current_text: &str) {
        let Some(path) = self.docs[self.active].file_path.clone() else {
            // Untitled scratch — no path, nothing to diff against.
            self.docs[self.active].git_status.clear();
            self.docs[self.active].git_status_revision = None;
            return;
        };
        let revision = self.docs[self.active].editor.revision();
        if self.docs[self.active].git_status_revision == Some(revision) {
            return;
        }
        self.docs[self.active].git_status = git::compute_line_status(&path, current_text);
        self.docs[self.active].git_status_revision = Some(revision);
    }

    /// existing line doesn't reshape thousands of digits.
    fn refresh_gutter(&mut self) {
        use std::fmt::Write;
        let lines = self.doc().editor.buffer().len_lines();
        if lines == self.gutter_lines {
            return;
        }
        let mut s = String::with_capacity(lines * (GUTTER_DIGITS + 1));
        for n in 1..=lines {
            if n > 1 {
                s.push('\n');
            }
            let _ = write!(s, "{:>width$}", n, width = GUTTER_DIGITS);
        }
        self.gutter_text.set_content(&mut self.font_system, &s);
        self.gutter_lines = lines;
    }

    /// Re-shape both halves of the status bar.
    ///
    /// Left half: `path  ·  language  ·  Spaces: N  ·  LE` — or a transient
    /// flash message while [`status_flash`](Self::status_flash) is active
    /// and unexpired.
    /// Right half: `Ln L, Col C  ·  N lines`.
    fn refresh_status(&mut self) {
        let doc = self.doc();
        let label = doc.label();
        let language = language_for(doc.file_path.as_deref());
        let le = line_ending_label(doc.editor.buffer().line_ending());
        let head = doc.editor.selections().primary().head;
        let pos = doc.editor.buffer().char_to_position(head);
        let lines = doc.editor.buffer().len_lines();
        let indent = self.tab_spaces.len();
        // When the find bar is open, prepend its match count to the right
        // half so the user keeps an eye on the search while typing in the
        // editor.
        let find_prefix = doc
            .find
            .as_ref()
            .filter(|f| f.match_count() > 0)
            .map(|f| format!("{}/{}  ·  ", f.current_index() + 1, f.match_count()))
            .unwrap_or_default();

        let flash = self
            .status_flash
            .as_ref()
            .filter(|(_, t)| t.elapsed() < FLASH_DURATION)
            .map(|(s, _)| s.clone());
        let counts = self.lsp.diagnostic_counts();
        let diag_suffix = if counts.total() > 0 {
            format!(
                "  ·  ⚠ {}/{}/{}/{}",
                counts.errors, counts.warnings, counts.info, counts.hints
            )
        } else {
            String::new()
        };
        let left = flash.unwrap_or_else(|| {
            format!("{label}  ·  {language}  ·  Spaces: {indent}  ·  {le}{diag_suffix}")
        });
        // Flutter session indicator on the right half so it's visible
        // alongside the caret position. Mirrors VSCode's debug bar
        // hint — "running" while a `flutter run` is active, gone
        // afterwards. Keeps the user oriented on whether Cmd-S will
        // trigger an automatic hot reload.
        let flutter_prefix = if self.flutter_session_active {
            "Flutter: running  ·  "
        } else {
            ""
        };
        let right = format!(
            "{flutter_prefix}{find_prefix}Ln {}, Col {}  ·  {} lines",
            pos.line + 1,
            pos.column + 1,
            lines
        );

        self.status_left.set_content(&mut self.font_system, &left);
        self.status_right.set_content(&mut self.font_system, &right);
    }

    // ── LSP wiring ────────────────────────────────────────────────────────

    /// Drain LSP server queues, react to any actionable events. Wired to a
    /// 100ms timer so diagnostics published while the user is idle still
    /// show up immediately.
    fn poll_lsp(&mut self) {
        let events = self.lsp.drain();
        if events.is_empty() {
            return;
        }
        let mut want_redraw = false;
        for ev in events {
            match ev {
                LspEvent::DiagnosticsUpdated { path: _ } => {
                    want_redraw = true;
                }
                LspEvent::Hover { doc_path, result } => {
                    self.handle_lsp_hover(doc_path, result);
                    want_redraw = true;
                }
                LspEvent::Definition {
                    doc_path,
                    locations,
                } => {
                    self.handle_lsp_definition(doc_path, locations);
                    want_redraw = true;
                }
                LspEvent::Completion {
                    doc_path,
                    anchor_char,
                    items,
                } => {
                    self.handle_lsp_completion(doc_path, anchor_char, items);
                    want_redraw = true;
                }
                LspEvent::ServerExited { kind } => {
                    log::warn!("LSP server {kind:?} exited");
                    self.set_status_flash(format!("LSP {kind:?} disconnected"));
                    want_redraw = true;
                }
            }
        }
        if want_redraw {
            self.scene_dirty = true;
            self.window.request_redraw();
        }
    }

    /// Look up the open document whose `file_path` equals `path`. Used to
    /// route LSP responses back to the originating tab.
    fn doc_idx_for_path(&self, path: &Path) -> Option<usize> {
        self.docs
            .iter()
            .position(|d| d.file_path.as_deref() == Some(path))
    }

    /// Send `textDocument/didOpen` for the document at `idx`. No-op when
    /// the doc has no file path or no server is wired.
    fn lsp_did_open_doc(&mut self, idx: usize) {
        let Some(path) = self.docs[idx].file_path.clone() else {
            return;
        };
        let Some(lang) = Language::for_path(&path) else {
            return;
        };
        let text = self.docs[idx].editor.text();
        let version = *self.lsp_doc_version.entry(path.clone()).or_insert(0) + 1;
        self.lsp_doc_version.insert(path.clone(), version);
        self.lsp.did_open(&path, lang, version, text);
    }

    /// Send `textDocument/didChange` for the active doc. Called from the
    /// render path right after the editor revision moves. `text` is the
    /// shaped buffer text we already materialized for the reshape pass —
    /// reusing it avoids a second `Rope::to_string()` per keystroke,
    /// which on a 4000-line file was costing ~10 ms.
    fn lsp_did_change_active(&mut self, text: &str) {
        let Some(path) = self.doc().file_path.clone() else {
            return;
        };
        let Some(lang) = Language::for_path(&path) else {
            return;
        };
        if !self.lsp.has_server(lang) {
            return;
        }
        let v = self.lsp_doc_version.entry(path.clone()).or_insert(0);
        *v += 1;
        let version = *v;
        self.lsp.did_change(&path, lang, version, text.to_string());
    }

    /// Send `textDocument/didSave` for the active doc.
    fn lsp_did_save_active(&mut self) {
        let Some(path) = self.doc().file_path.clone() else {
            return;
        };
        let Some(lang) = Language::for_path(&path) else {
            return;
        };
        let text = self.doc().editor.text();
        self.lsp.did_save(&path, lang, Some(text));
    }

    /// Send `textDocument/didClose` for the given file. Called before a
    /// tab is removed from `docs`.
    fn lsp_did_close_path(&mut self, path: &Path) {
        let Some(lang) = Language::for_path(path) else {
            return;
        };
        self.lsp.did_close(path, lang);
        self.lsp_doc_version.remove(path);
    }

    /// Trigger a `textDocument/hover` request at the primary caret of the
    /// active doc. The response will be handled asynchronously in
    /// [`poll_lsp`](Self::poll_lsp).
    fn request_hover(&mut self) {
        let Some(path) = self.doc().file_path.clone() else {
            return;
        };
        let Some(lang) = Language::for_path(&path) else {
            return;
        };
        let head = self.doc().editor.selections().primary().head;
        let pos = self.doc().editor.buffer().char_to_position(head);
        let lsp_pos = lsp_types::Position {
            line: pos.line as u32,
            character: pos.column as u32,
        };
        // Hover is anchored to the current caret; remember which char we
        // asked about so the popup follows scrolling.
        if self.lsp.request_hover(&path, lang, lsp_pos).is_some() {
            self.hover_popup = None;
        }
    }

    /// Trigger a `textDocument/definition` request at the primary caret.
    fn request_definition(&mut self) {
        let Some(path) = self.doc().file_path.clone() else {
            return;
        };
        let Some(lang) = Language::for_path(&path) else {
            return;
        };
        let head = self.doc().editor.selections().primary().head;
        let pos = self.doc().editor.buffer().char_to_position(head);
        let lsp_pos = lsp_types::Position {
            line: pos.line as u32,
            character: pos.column as u32,
        };
        self.lsp.request_definition(&path, lang, lsp_pos);
    }

    /// Trigger `textDocument/completion` at the caret. Anchor is the
    /// start of the word the caret is inside (so the popup stays
    /// stable as the user keeps typing letters of the same word).
    /// Already-open popups are replaced — the user just hit the
    /// completion shortcut again to refresh.
    fn request_completion(&mut self) {
        let Some(path) = self.doc().file_path.clone() else {
            log::info!("completion: skipped — no file path on active doc");
            return;
        };
        let Some(lang) = Language::for_path(&path) else {
            log::info!(
                "completion: skipped — no language recognised for {}",
                path.display()
            );
            return;
        };
        let head = self.doc().editor.selections().primary().head;
        let anchor = word_start_before(&self.doc().editor.text(), head);
        let pos = self.doc().editor.buffer().char_to_position(head);
        let lsp_pos = lsp_types::Position {
            line: pos.line as u32,
            character: pos.column as u32,
        };
        match self.lsp.request_completion(
            &path,
            lang,
            lsp_pos,
            anchor,
            editor_lsp_client::lsp_types::CompletionTriggerKind::INVOKED,
            None,
        ) {
            Some(id) => log::info!(
                "completion: request sent (id={id}, anchor_char={anchor}, line={}, col={})",
                pos.line,
                pos.column
            ),
            None => log::info!(
                "completion: server not ready for {lang:?} on {}",
                path.display()
            ),
        }
    }

    fn handle_lsp_completion(
        &mut self,
        doc_path: PathBuf,
        anchor_char: usize,
        items: Vec<editor_lsp_client::lsp_types::CompletionItem>,
    ) {
        log::info!(
            "completion: response for {} — {} item(s)",
            doc_path.display(),
            items.len()
        );
        if self.doc().file_path.as_deref() != Some(&doc_path) {
            return;
        }
        if items.is_empty() {
            self.set_status_flash("no completions".to_string());
            return;
        }
        // The caret may have moved (the user kept typing while the request
        // was in flight) — compute the current prefix from the live caret
        // via the rope slice rather than materialising the whole buffer.
        let head = self.doc().editor.selections().primary().head;
        if head < anchor_char {
            return; // caret moved before the anchor; the popup is stale
        }
        let prefix = self.doc().editor.buffer().slice(anchor_char..head);
        let mut popup = CompletionPopup {
            items,
            filtered: Vec::new(),
            anchor_char,
            prefix,
            selected: 0,
            scroll: 0,
        };
        popup.refilter();
        if popup.filtered.is_empty() {
            return;
        }
        self.completion = Some(popup);
        self.shape_completion_popup_inplace();
    }

    /// Re-shape the popup's text stack from `self.completion`. Reads only
    /// the visible window so a 200-item response stays a 10-line shape
    /// job. Reads + writes `self` via split borrows — no allocation of
    /// the full item set (a `CompletionPopup` clone would touch every
    /// `String` field of every `CompletionItem`).
    fn shape_completion_popup_inplace(&mut self) {
        let text = {
            let Some(popup) = self.completion.as_ref() else {
                return;
            };
            let start = popup.scroll;
            let end = (popup.scroll + COMPLETION_MAX_ROWS).min(popup.filtered.len());
            let mut s = String::with_capacity((end - start) * 32);
            for (row, &i) in popup.filtered[start..end].iter().enumerate() {
                if row > 0 {
                    s.push('\n');
                }
                s.push_str(&popup.items[i].label);
            }
            s
        };
        self.completion_text
            .set_content(&mut self.font_system, &text);
    }

    /// Try to accept the currently-selected completion. Returns `true`
    /// when an item was inserted (so the caller can swallow the key).
    fn accept_completion(&mut self) -> bool {
        let Some(popup) = self.completion.as_ref() else {
            return false;
        };
        let Some(&item_idx) = popup.filtered.get(popup.selected) else {
            self.completion = None;
            return false;
        };
        let item = &popup.items[item_idx];
        // Prefer insert_text when the server explicitly supplied one;
        // fall back to the label. `text_edit`'s explicit range is
        // ignored for v1 — most servers' ranges are equivalent to the
        // anchor → caret span we already computed.
        let insert = item
            .insert_text
            .clone()
            .unwrap_or_else(|| item.label.clone());
        let anchor = popup.anchor_char;
        let head = self.doc().editor.selections().primary().head;
        self.completion = None;
        // Replace [anchor, head) with the chosen text.
        let editor = &mut self.docs[self.active].editor;
        editor.set_selection(Selection::new(anchor, head));
        editor.insert(&insert);
        self.text_dirty = true;
        self.scene_dirty = true;
        self.follow_caret = true;
        self.mark_dirty_if_clean();
        self.window.request_redraw();
        true
    }

    /// Move the selection one row down within the filtered list, wrapping
    /// from bottom to top. Scrolls the visible window if the new selection
    /// is past the bottom edge, and re-shapes the popup text when the
    /// window changes (since the text stack only holds visible rows).
    fn completion_next(&mut self) {
        let needs_reshape = {
            let Some(popup) = self.completion.as_mut() else {
                return;
            };
            if popup.filtered.is_empty() {
                return;
            }
            let before = popup.scroll;
            popup.selected = (popup.selected + 1) % popup.filtered.len();
            popup.adjust_scroll();
            popup.scroll != before
        };
        if needs_reshape {
            self.shape_completion_popup_inplace();
        }
    }

    fn completion_prev(&mut self) {
        let needs_reshape = {
            let Some(popup) = self.completion.as_mut() else {
                return;
            };
            if popup.filtered.is_empty() {
                return;
            }
            let before = popup.scroll;
            popup.selected = if popup.selected == 0 {
                popup.filtered.len() - 1
            } else {
                popup.selected - 1
            };
            popup.adjust_scroll();
            popup.scroll != before
        };
        if needs_reshape {
            self.shape_completion_popup_inplace();
        }
    }

    /// Re-evaluate the popup against the buffer state after an edit. The
    /// caret may have moved (typing extends the prefix; backspace
    /// shrinks it); if it falls outside the anchored word the popup
    /// dismisses.
    fn refresh_completion_after_edit(&mut self) {
        let Some(anchor) = self.completion.as_ref().map(|p| p.anchor_char) else {
            return;
        };
        let head = self.doc().editor.selections().primary().head;
        let len_chars = self.doc().editor.buffer().len_chars();
        if head < anchor || anchor > len_chars {
            self.completion = None;
            return;
        }
        // Slice the rope for just the prefix — way cheaper than
        // materialising the whole buffer (which on a 4000-line file
        // was costing ~10 ms of `Rope::to_string` per keystroke).
        let prefix = self.doc().editor.buffer().slice(anchor..head);
        if prefix.chars().any(|c| !is_word_char(c)) {
            self.completion = None;
            return;
        }
        let empty = {
            let popup = self
                .completion
                .as_mut()
                .expect("completion present per earlier check");
            popup.prefix = prefix;
            popup.refilter();
            popup.filtered.is_empty()
        };
        if empty {
            self.completion = None;
            return;
        }
        self.shape_completion_popup_inplace();
    }

    fn handle_lsp_hover(&mut self, doc_path: PathBuf, result: Option<lsp_types::Hover>) {
        if self.doc().file_path.as_deref() != Some(&doc_path) {
            return; // user switched tabs since the request
        }
        let Some(hover) = result else {
            self.set_status_flash("no hover info".to_string());
            return;
        };
        let text = hover_contents_to_string(&hover.contents);
        if text.is_empty() {
            self.set_status_flash("no hover info".to_string());
            return;
        }
        let anchor = self.doc().editor.selections().primary().head;
        self.hover_text.set_content(&mut self.font_system, &text);
        self.hover_popup = Some(HoverPopup {
            anchor_char: anchor,
        });
    }

    fn handle_lsp_definition(&mut self, doc_path: PathBuf, locations: Vec<lsp_types::Location>) {
        if self.doc().file_path.as_deref() != Some(&doc_path) {
            return;
        }
        let Some(loc) = locations.into_iter().next() else {
            self.set_status_flash("no definition".to_string());
            return;
        };
        let Ok(target) = loc.uri.to_file_path() else {
            return;
        };
        let line = loc.range.start.line as usize;
        let col = loc.range.start.character as usize;
        if let Some(idx) = self.doc_idx_for_path(&target) {
            // Already open — just jump there.
            self.switch_tab(idx);
            let buf = self.docs[self.active].editor.buffer();
            if let Some(char_idx) = buf.position_to_char(Position::new(line, col)) {
                self.docs[self.active]
                    .editor
                    .set_selection(Selection::cursor(char_idx));
                self.follow_caret = true;
                self.scene_dirty = true;
            }
        } else {
            // Open the file in a new tab, then jump.
            self.open_path(target);
            let buf = self.docs[self.active].editor.buffer();
            if let Some(char_idx) = buf.position_to_char(Position::new(line, col)) {
                self.docs[self.active]
                    .editor
                    .set_selection(Selection::cursor(char_idx));
                self.follow_caret = true;
                self.scene_dirty = true;
            }
        }
    }

    /// Rebuild the tab-strip TextStack: one line, each label padded to
    /// `TAB_LABEL_CHARS` slots so the next label starts near the next slot
    /// boundary. Monospace-assumed; complex-script labels will drift slightly
    /// but stay within their slot's backdrop.
    fn refresh_tabs_text(&mut self) {
        let mut s = String::with_capacity(self.docs.len() * (TAB_LABEL_CHARS + 4));
        for d in &self.docs {
            let prefix = if d.dirty { "• " } else { "  " };
            let prefix_chars = prefix.chars().count();
            let label = d.label();
            // Reserve one trailing space so the cell never butts straight up
            // against the next slot's start.
            let max_label = TAB_LABEL_CHARS
                .saturating_sub(prefix_chars)
                .saturating_sub(1);
            let label_chars = label.chars().count();
            let truncated = if label_chars > max_label && max_label > 1 {
                let head: String = label.chars().take(max_label - 1).collect();
                format!("{head}…")
            } else {
                label
            };
            let cell = format!("{prefix}{truncated}");
            let cell_chars = cell.chars().count();
            s.push_str(&cell);
            for _ in cell_chars..TAB_LABEL_CHARS {
                s.push(' ');
            }
        }
        self.tabs_text.set_content(&mut self.font_system, &s);
    }

    // ── command palette ────────────────────────────────────────────────────

    /// Open the command palette, populated with every registered
    /// command plus any `package.json` scripts detected in the
    /// workspace root. Built-in entries come first so the empty-query
    /// order stays stable; scripts append after them.
    fn open_palette(&mut self) {
        let mut entries: Vec<CommandEntry> = BUILTIN_COMMAND_IDS
            .iter()
            .cloned()
            .map(CommandEntry::builtin)
            .collect();
        for script in &self.npm_scripts {
            entries.push(CommandEntry {
                id: CommandId::RunScript(script.name.clone()),
                label: format!("Run script: {}", script.name),
            });
        }
        if self.flutter_project.is_some() {
            // With a cached device list, surface one Run entry per
            // device so the user can target a specific phone /
            // emulator / browser without typing the id. Without a
            // cached list (initial launch, flutter binary missing,
            // refresh still in flight) fall back to the bare
            // `Flutter: Run` which lets Flutter pick the default.
            if self.flutter_devices.is_empty() {
                entries.push(CommandEntry::builtin(CommandId::FlutterRun));
            } else {
                for dev in &self.flutter_devices {
                    // Keep the label short — the device id (often a
                    // 32-char UUID) is internal; the user picks by
                    // name + platform. Emulators get a trailing
                    // " (emulator)" hint because that's the only
                    // distinction that matters at pick time when a
                    // real phone of the same name is also attached.
                    let suffix = if dev.emulator { " (emulator)" } else { "" };
                    let label = if dev.target_platform.is_empty() {
                        format!("Flutter: Run on {}{}", dev.name, suffix)
                    } else {
                        format!(
                            "Flutter: Run on {} · {}{}",
                            dev.name, dev.target_platform, suffix
                        )
                    };
                    entries.push(CommandEntry {
                        id: CommandId::FlutterRunOnDevice(dev.id.clone()),
                        label,
                    });
                }
            }
            entries.push(CommandEntry::builtin(CommandId::FlutterHotReload));
            entries.push(CommandEntry::builtin(CommandId::FlutterHotRestart));
            entries.push(CommandEntry::builtin(CommandId::FlutterStop));
        }
        self.palette = Some(CommandPalette::new(entries));
        self.refresh_palette_text();
        self.scene_dirty = true;
        self.window.request_redraw();
    }

    /// Close the command palette (without firing anything).
    fn close_palette(&mut self) {
        self.palette = None;
        self.scene_dirty = true;
        self.window.request_redraw();
    }

    /// Reshape the secondary TextStack to reflect the palette's current
    /// query + visible commands. Called after every palette mutation.
    fn refresh_palette_text(&mut self) {
        let Some(palette) = self.palette.as_ref() else {
            return;
        };
        let mut text = String::with_capacity(128);
        text.push_str("❯ ");
        text.push_str(palette.query());
        text.push_str("\n\n");
        // Render only the rows currently scrolled into view. Caps the
        // popup at PALETTE_VISIBLE_ROWS so a long list (Flutter
        // devices + Themes + Run scripts) stays inside the window
        // instead of falling off the bottom.
        for label in palette.windowed_labels(PALETTE_VISIBLE_ROWS) {
            text.push_str("  ");
            text.push_str(label);
            text.push('\n');
        }
        self.palette_text.set_content(&mut self.font_system, &text);
    }

    /// Route a key while the palette is open.
    fn handle_palette_key(&mut self, event: KeyEvent) {
        // Cmd-V pastes the clipboard into the palette query. Without
        // this the user couldn't paste e.g. a copied file path or a
        // script name into the prompt. Done before the match so the
        // localised V (or any cmd-modified character) doesn't fall
        // through to push_char as a literal.
        if is_cmd_or_ctrl(self.modifiers) && shortcut_letter(&event, 'v') {
            if let Some(text) = clipboard_get() {
                if let Some(p) = self.palette.as_mut() {
                    for c in text.chars() {
                        p.push_char(c);
                    }
                    p.scroll_into_view(PALETTE_VISIBLE_ROWS);
                }
                self.refresh_palette_text();
                self.scene_dirty = true;
                self.window.request_redraw();
            }
            return;
        }
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => self.close_palette(),
            Key::Named(NamedKey::ArrowUp) => {
                if let Some(p) = self.palette.as_mut() {
                    p.prev();
                    p.scroll_into_view(PALETTE_VISIBLE_ROWS);
                }
                self.refresh_palette_text();
                self.scene_dirty = true;
                self.window.request_redraw();
            }
            Key::Named(NamedKey::ArrowDown) => {
                if let Some(p) = self.palette.as_mut() {
                    p.next();
                    p.scroll_into_view(PALETTE_VISIBLE_ROWS);
                }
                self.refresh_palette_text();
                self.scene_dirty = true;
                self.window.request_redraw();
            }
            Key::Named(NamedKey::Enter) => {
                // Clone the id so we can call `execute_command` (which
                // mutably borrows `self`) without holding the palette
                // borrow over the call.
                let cmd_id = self
                    .palette
                    .as_ref()
                    .and_then(|p| p.selected())
                    .map(|e| e.id.clone());
                if let Some(id) = cmd_id {
                    self.execute_command(id);
                }
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(p) = self.palette.as_mut() {
                    p.backspace();
                }
                self.refresh_palette_text();
                self.scene_dirty = true;
                self.window.request_redraw();
            }
            _ => {
                if is_cmd_or_ctrl(self.modifiers) {
                    return;
                }
                if let Some(text) = &event.text {
                    if let Some(p) = self.palette.as_mut() {
                        for c in text.chars() {
                            p.push_char(c);
                        }
                    }
                    self.refresh_palette_text();
                    self.scene_dirty = true;
                    self.window.request_redraw();
                }
            }
        }
    }

    /// Run a palette-selected command. Closes the palette first so the
    /// underlying handler sees a consistent state.
    fn execute_command(&mut self, cmd: CommandId) {
        self.close_palette();
        match cmd {
            CommandId::NewFile => self.new_file(),
            CommandId::OpenFile => self.open_file_dialog(),
            CommandId::SaveFile => self.save_to_file(),
            CommandId::SaveFileAs => self.save_as(),
            CommandId::SaveAll => self.save_all(),
            CommandId::CloseOtherTabs => self.close_other_tabs(),
            CommandId::CloseAllTabs => self.close_all_tabs(),
            CommandId::ThemeDefault => self.apply_bundled_theme("Default Dark", ""),
            CommandId::ThemeSolarizedDark => {
                self.apply_bundled_theme("Solarized Dark", BUNDLED_SOLARIZED_DARK)
            }
            CommandId::ThemeSolarizedLight => {
                self.apply_bundled_theme("Solarized Light", BUNDLED_SOLARIZED_LIGHT)
            }
            CommandId::ThemeMonokai => self.apply_bundled_theme("Monokai", BUNDLED_MONOKAI),
            CommandId::ThemeGruvboxDark => {
                self.apply_bundled_theme("Gruvbox Dark", BUNDLED_GRUVBOX_DARK)
            }
            CommandId::ThemeNord => self.apply_bundled_theme("Nord", BUNDLED_NORD),
            CommandId::ThemeTokyoNight => {
                self.apply_bundled_theme("Tokyo Night", BUNDLED_TOKYO_NIGHT)
            }
            CommandId::BrowseThemes => self.browse_themes(),
            CommandId::ImportVscodeSettings => self.import_vscode_settings(),
            CommandId::RunScript(name) => self.run_npm_script(&name),
            CommandId::FlutterRun => {
                self.flutter_session_active = true;
                self.flutter_send(b"flutter run\n", "flutter run");
                self.refresh_flutter_devices_async();
            }
            CommandId::FlutterRunOnDevice(id) => {
                self.flutter_session_active = true;
                self.flutter_run_on_device(&id);
                self.refresh_flutter_devices_async();
            }
            CommandId::FlutterHotReload => self.flutter_send(b"r", "flutter: hot reload"),
            CommandId::FlutterHotRestart => self.flutter_send(b"R", "flutter: hot restart"),
            CommandId::FlutterStop => {
                self.flutter_session_active = false;
                self.flutter_send(b"q", "flutter: stop");
                self.refresh_flutter_devices_async();
            }
        }
    }

    /// Write `bytes` to the embedded terminal, opening + focusing the
    /// pane first if it's hidden. Used by every `Flutter:` command —
    /// `flutter run\n` to start a session, the bare `r`/`R`/`q` keys
    /// to drive an already-running session through the standard
    /// Flutter CLI shortcuts. `label` is flashed to the status bar
    /// so the user sees their action without flipping to the pane.
    fn flutter_send(&mut self, bytes: &'static [u8], label: &str) {
        let needs_show = self.terminal.as_ref().map(|t| !t.visible).unwrap_or(true);
        if needs_show {
            self.toggle_terminal();
        }
        if let Some(t) = self.terminal.as_mut() {
            t.focused = true;
            t.write(bytes);
        }
        self.set_status_flash(label.to_string());
        self.scene_dirty = true;
        self.window.request_redraw();
    }

    /// Variant of [`flutter_send`](Self::flutter_send) that runs
    /// `flutter run -d <id>` for the picker-selected device. Builds
    /// the bytes per-call because the id is owned data — the static
    /// `&[u8]` signature on `flutter_send` doesn't fit.
    fn flutter_run_on_device(&mut self, device_id: &str) {
        let needs_show = !self.terminal.as_ref().is_some_and(|t| t.visible);
        if needs_show {
            self.toggle_terminal();
        }
        let cmd = format!("flutter run -d {device_id}\n");
        if let Some(t) = self.terminal.as_mut() {
            t.focused = true;
            t.write(cmd.into_bytes());
        }
        self.set_status_flash(format!("flutter run -d {device_id}"));
        self.scene_dirty = true;
        self.window.request_redraw();
    }

    /// `true` when a Flutter session is active AND the just-saved
    /// document is a `.dart` file. Used by `save_to_file` to mirror
    /// VSCode's save-triggers-hot-reload UX. The pane stays out of
    /// the user's way otherwise — saves to non-Dart files don't
    /// disturb a running session.
    fn should_auto_hot_reload(&self, saved: &Path) -> bool {
        self.flutter_session_active
            && saved
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("dart"))
    }

    /// Run `<package_manager> run <name>\n` in the embedded terminal,
    /// auto-opening the pane if it's currently hidden. The package
    /// manager is detected from the workspace's lockfile; the cwd is
    /// the file-tree root that the script was discovered against.
    fn run_npm_script(&mut self, name: &str) {
        let root = self.file_tree.root.clone();
        let pm = scripts::detect_package_manager(&root);
        let cmd = format!("{} run {}\n", pm.binary(), name);

        // Make sure the pane is visible and focused before we feed it
        // the command — `Cmd-J` users expect to see the output.
        let needs_show = self.terminal.as_ref().map(|t| !t.visible).unwrap_or(true);
        if needs_show {
            self.toggle_terminal();
        }
        if let Some(t) = self.terminal.as_mut() {
            t.focused = true;
            t.write(cmd.into_bytes());
        }
        self.set_status_flash(format!("running: {} run {}", pm.binary(), name));
        self.scene_dirty = true;
        self.window.request_redraw();
    }

    /// "Theme: Browse…" — open a file dialog rooted at
    /// `~/.config/lighteditor/themes/` (created on demand) so the user can
    /// pick a custom theme file they've dropped there. Picking applies the
    /// theme and persists it to `theme.toml`.
    fn browse_themes(&mut self) {
        let themes_dir = dirs::config_dir().map(|d| d.join(CONFIG_SUBDIR).join("themes"));
        if let Some(dir) = themes_dir.as_deref() {
            let _ = std::fs::create_dir_all(dir);
        }
        let dialog = match themes_dir.as_deref() {
            Some(d) => rfd::FileDialog::new()
                .set_directory(d)
                .add_filter("Theme", &["toml", "json"]),
            None => rfd::FileDialog::new().add_filter("Theme", &["toml", "json"]),
        };
        let Some(path) = dialog.pick_file() else {
            return;
        };
        let label = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Custom".to_string());
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase());
        // VSCode-format themes are loaded through the JSON converter,
        // then serialised to TOML for persistence so the existing
        // theme.toml watcher hot-reload path still works.
        if matches!(ext.as_deref(), Some("json")) {
            match editor_config::load_vscode_theme(&path) {
                Ok(theme) => {
                    let toml_content = toml::to_string(&theme).unwrap_or_default();
                    self.apply_bundled_theme(&label, &toml_content);
                }
                Err(e) => {
                    log::error!("could not load VSCode theme {}: {}", path.display(), e);
                    self.set_status_flash(format!("vscode theme failed: {e}"));
                }
            }
            return;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                log::error!("could not read theme {}: {}", path.display(), e);
                return;
            }
        };
        self.apply_bundled_theme(&label, &content);
    }

    /// Apply a bundled theme: parse the embedded TOML (or fall back to
    /// `Theme::default()` for the empty-content "Default Dark" entry), swap
    /// the in-memory theme, and write the content to `theme.toml` so the
    /// pick persists across restarts. The file watcher's dedup keeps the
    /// disk write from triggering a second reload.
    fn apply_bundled_theme(&mut self, label: &str, toml_content: &str) {
        let theme = if toml_content.is_empty() {
            Theme::default()
        } else {
            match toml::from_str::<Theme>(toml_content) {
                Ok(t) => t,
                Err(e) => {
                    log::error!("bundled theme {label} failed to parse: {e}");
                    return;
                }
            }
        };
        self.reload_theme(theme);
        // Persist to disk so the chosen theme survives a restart. The
        // settings/theme watcher will fire ThemeChanged on the write; the
        // app-level dedup compares Theme equality and skips a second
        // in-memory re-apply.
        if let Some(path) = self.theme_file_path() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let payload = if toml_content.is_empty() {
                // For the "Default Dark" pick, write the serialised
                // default theme so the file is self-documenting.
                toml::to_string(&Theme::default()).unwrap_or_default()
            } else {
                toml_content.to_string()
            };
            if let Err(e) = std::fs::write(&path, payload) {
                log::warn!("couldn't persist theme to {}: {}", path.display(), e);
            }
        }
        log::info!("applied theme: {label}");
    }

    /// Apply a bundled theme matched by display name (case-insensitive),
    /// returning `true` if one matched. Used to honour a VSCode
    /// `workbench.colorTheme` on import — the common names map onto the
    /// themes we ship. Unmatched names return `false` so the caller can
    /// tell the user to import the theme JSON directly.
    fn apply_theme_by_name(&mut self, name: &str) -> bool {
        let key = name.trim().to_ascii_lowercase();
        let (label, content): (&str, &str) = match key.as_str() {
            "default dark+"
            | "dark+"
            | "dark (visual studio)"
            | "default dark modern"
            | "dark modern"
            | "visual studio dark" => ("Default Dark", ""),
            "solarized dark" => ("Solarized Dark", BUNDLED_SOLARIZED_DARK),
            "solarized light" => ("Solarized Light", BUNDLED_SOLARIZED_LIGHT),
            "monokai" => ("Monokai", BUNDLED_MONOKAI),
            "gruvbox dark" | "gruvbox dark medium" | "gruvbox dark hard" => {
                ("Gruvbox Dark", BUNDLED_GRUVBOX_DARK)
            }
            "nord" => ("Nord", BUNDLED_NORD),
            "tokyo night" | "tokyonight" => ("Tokyo Night", BUNDLED_TOKYO_NIGHT),
            _ => return false,
        };
        self.apply_bundled_theme(label, content);
        true
    }

    /// XDG/macOS path where the user's `theme.toml` lives. `None` if the
    /// OS has no config dir.
    fn theme_file_path(&self) -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join(CONFIG_SUBDIR).join(THEME_FILENAME))
    }

    /// Our user-level `settings.toml`.
    fn settings_file_path(&self) -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join(CONFIG_SUBDIR).join(CONFIG_FILENAME))
    }

    /// Import editor settings from the user's VSCode `settings.json`.
    ///
    /// Looks at the stock VSCode location plus the common forks
    /// (Insiders / VSCodium / Cursor / Windsurf) under the platform
    /// config dir, and the workspace's `.vscode/settings.json` — the
    /// workspace file wins where both set a key. Mapped editor keys
    /// (font size, line height, tab size, excluded dirs) are merged onto
    /// the current settings, persisted, and applied live; the active
    /// `workbench.colorTheme` is applied too when it names a theme we
    /// ship bundled.
    fn import_vscode_settings(&mut self) {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(cfg) = dirs::config_dir() {
            for app in ["Code", "Code - Insiders", "VSCodium", "Cursor", "Windsurf"] {
                candidates.push(cfg.join(app).join("User").join("settings.json"));
            }
        }
        // Workspace settings last so they override the user-level file.
        candidates.push(self.file_tree.root.join(".vscode").join("settings.json"));

        // The user-level settings.toml is both our merge base and our
        // write target. (App owns the live `Settings`; we round-trip
        // through disk so the settings watcher reloads + re-applies via
        // the canonical path.)
        let Some(settings_path) = self.settings_file_path() else {
            return;
        };
        let base = Settings::load_or_default(&settings_path);
        let mut merged = base.clone();
        let mut color_theme: Option<String> = None;
        let mut found = 0usize;
        for path in &candidates {
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            found += 1;
            let partial = editor_config::import_vscode_settings(&text);
            merged.merge(&partial);
            // Later files (workspace) override the colour-theme pick.
            if let Some(name) = editor_config::vscode_color_theme(&text) {
                color_theme = Some(name);
            }
            log::info!("imported VSCode settings from {}", path.display());
        }

        if found == 0 {
            self.set_status_flash("no VSCode settings.json found".to_string());
            return;
        }

        // Apply + persist the editor settings if anything mapped.
        let settings_changed = merged != base;
        if settings_changed {
            if let Some(parent) = settings_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match toml::to_string(&merged) {
                Ok(toml_str) => {
                    if let Err(e) = std::fs::write(&settings_path, toml_str) {
                        log::error!("could not write {}: {}", settings_path.display(), e);
                    }
                }
                Err(e) => log::error!("could not serialise settings: {e}"),
            }
            // Apply immediately; the settings watcher also fires on the
            // write and re-applies via App, deduped on equality.
            self.reload_settings(&merged);
        }

        // Apply the colour theme if it names one we ship.
        let theme_applied = color_theme
            .as_deref()
            .map(|name| self.apply_theme_by_name(name))
            .unwrap_or(false);

        // Build a status line describing what actually happened.
        let mut parts: Vec<String> = Vec::new();
        if settings_changed {
            parts.push("editor settings".to_string());
        }
        match (&color_theme, theme_applied) {
            (Some(name), true) => parts.push(format!("theme “{name}”")),
            (Some(name), false) => {
                parts.push(format!("theme “{name}” (not bundled — use Theme: Browse…)"))
            }
            (None, _) => {}
        }
        let msg = if parts.is_empty() {
            "VSCode settings: nothing to import".to_string()
        } else {
            format!("imported {}", parts.join(" + "))
        };
        self.set_status_flash(msg);
    }

    /// Close every tab except the active one. Each dirty non-active tab is
    /// switched to and prompted; cancelling a prompt keeps that tab open and
    /// the walk continues to the next dirty one.
    fn close_other_tabs(&mut self) {
        if self.docs.len() <= 1 {
            return;
        }
        let keep = self.active;
        // Range loop because the body mutably borrows self via
        // `confirm_unsaved`, so iterating `self.docs` is awkward.
        #[allow(clippy::needless_range_loop)]
        let mut keep_flags: Vec<bool> = (0..self.docs.len()).map(|i| i == keep).collect();
        #[allow(clippy::needless_range_loop)]
        for i in 0..self.docs.len() {
            if i == keep || !self.docs[i].dirty {
                continue;
            }
            self.active = i;
            self.update_title();
            if !self.confirm_unsaved("Close tab") {
                keep_flags[i] = true;
            }
        }
        self.retain_docs(&keep_flags, Some(keep));
    }

    /// Close every tab; replaced with a fresh scratch if all close. Each
    /// dirty tab is prompted; cancel keeps that tab.
    fn close_all_tabs(&mut self) {
        let mut keep_flags: Vec<bool> = vec![false; self.docs.len()];
        #[allow(clippy::needless_range_loop)]
        for i in 0..self.docs.len() {
            if !self.docs[i].dirty {
                continue;
            }
            self.active = i;
            self.update_title();
            if !self.confirm_unsaved("Close tab") {
                keep_flags[i] = true;
            }
        }
        self.retain_docs(&keep_flags, None);
    }

    /// Filter `docs` by `keep_flags`. If `preferred_active` is `Some(i)` and
    /// the doc at index `i` survives, it becomes active; otherwise the first
    /// surviving doc wins. An empty result is replaced by a fresh scratch.
    fn retain_docs(&mut self, keep_flags: &[bool], preferred_active: Option<usize>) {
        // Send didClose for every doc we're about to drop. Collect paths
        // first so iteration doesn't overlap the mutable LSP calls.
        let to_close: Vec<PathBuf> = self
            .docs
            .iter()
            .enumerate()
            .filter_map(|(i, d)| {
                if !keep_flags[i] {
                    d.file_path.clone()
                } else {
                    None
                }
            })
            .collect();
        for p in to_close {
            self.lsp_did_close_path(&p);
        }
        let mut new_docs = Vec::with_capacity(self.docs.len());
        let mut new_active = 0;
        for (i, doc) in self.docs.drain(..).enumerate() {
            if keep_flags[i] {
                if Some(i) == preferred_active {
                    new_active = new_docs.len();
                }
                new_docs.push(doc);
            }
        }
        if new_docs.is_empty() {
            new_docs.push(Document::new_scratch(""));
        }
        self.docs = new_docs;
        self.active = new_active.min(self.docs.len() - 1);
        self.text_dirty = true;
        self.scene_dirty = true;
        self.follow_caret = false;
        self.update_title();
        self.refresh_tabs_text();
        self.refresh_find_text();
        self.window.request_redraw();
    }

    /// Save every dirty document in turn. Documents without a path get a
    /// Save As dialog each; the user may cancel any one of them, in which
    /// case that document stays dirty and the rest still get saved. After
    /// the walk we return to the document the user was on and flash an
    /// aggregate count, overriding any per-file flash `save_to_file` set.
    fn save_all(&mut self) {
        let original_active = self.active;
        let mut dirty_total = 0usize;
        let mut saved = 0usize;
        for i in 0..self.docs.len() {
            if !self.docs[i].dirty {
                continue;
            }
            dirty_total += 1;
            if i != self.active {
                self.active = i;
            }
            self.save_to_file();
            if !self.docs[self.active].dirty {
                saved += 1;
            }
        }
        if original_active < self.docs.len() {
            self.active = original_active;
        }
        self.update_title();
        self.refresh_tabs_text();
        self.refresh_find_text();
        self.scene_dirty = true;
        self.text_dirty = true;
        if dirty_total > 0 {
            let msg = if saved == dirty_total {
                if saved == 1 {
                    "saved 1 file".to_string()
                } else {
                    format!("saved {saved} files")
                }
            } else {
                format!("saved {saved} of {dirty_total} files")
            };
            self.set_status_flash(msg);
        }
        self.window.request_redraw();
    }

    /// Save As: always prompt for a path, even if the buffer already has one.
    fn save_as(&mut self) {
        let Some(path) = rfd::FileDialog::new().save_file() else {
            return;
        };
        let old_path = self.doc().file_path.clone();
        let text = self.doc().editor.text();
        match std::fs::write(&path, &text) {
            Ok(()) => {
                log::info!("saved as {}", path.display());
                let label = filename_for_flash(&path);
                {
                    let d = self.doc_mut();
                    d.file_path = Some(path);
                    d.dirty = false;
                }
                self.update_title();
                self.refresh_tabs_text();
                self.set_status_flash(format!("saved · {label}"));
                // If the path changed, the LSP server's view of the old
                // path is stale — close it. Then introduce the new one.
                if let Some(old) = old_path {
                    self.lsp_did_close_path(&old);
                }
                self.lsp_did_open_doc(self.active);
                self.scene_dirty = true;
                self.window.request_redraw();
            }
            Err(e) => {
                log::error!("save failed: {}", e);
                let label = filename_for_flash(&path);
                self.set_status_flash(format!("save failed · {label} · {e}"));
            }
        }
    }

    /// The palette's backdrop rectangle, in physical pixels.
    fn palette_panel_rect(&self) -> Rect {
        let pad = PALETTE_PAD_DIP * self.scale;
        let width = PALETTE_WIDTH_DIP * self.scale;
        let top = PALETTE_TOP_DIP * self.scale;
        let lh = self.line_height();
        let rows = self
            .palette
            .as_ref()
            .map(|p| p.visible_count().min(PALETTE_VISIBLE_ROWS))
            .unwrap_or(0);
        let inner_height = (2 + rows) as f32 * lh;
        let height = inner_height + 2.0 * pad;
        let surface_w = self.gpu.surface_config.width as f32;
        let left = (surface_w - width) * 0.5;
        Rect::new(left, top, width, height)
    }

    fn palette_text_origin(&self) -> (f32, f32) {
        let panel = self.palette_panel_rect();
        let pad = PALETTE_PAD_DIP * self.scale;
        (panel.min_x() + pad, panel.min_y() + pad)
    }

    fn palette_selection_rect(&self) -> Option<Rect> {
        let palette = self.palette.as_ref()?;
        let row = palette.selected_row_windowed(PALETTE_VISIBLE_ROWS)?;
        let lh = self.line_height();
        let (_ox, oy) = self.palette_text_origin();
        let row_y = oy + (2 + row) as f32 * lh;
        let panel = self.palette_panel_rect();
        let pad = PALETTE_PAD_DIP * self.scale;
        Some(Rect::new(
            panel.min_x() + pad * 0.5,
            row_y,
            panel.size.width - pad,
            lh,
        ))
    }

    // ── find bar ───────────────────────────────────────────────────────────

    /// Open the find bar on the active document with an empty query.
    fn open_find(&mut self) {
        self.doc_mut().find = Some(FindBar::new());
        self.refresh_find_text();
        self.scene_dirty = true;
        self.window.request_redraw();
    }

    /// Close the find bar (without changing the selection).
    fn close_find(&mut self) {
        self.doc_mut().find = None;
        self.scene_dirty = true;
        self.window.request_redraw();
    }

    fn refresh_find_text(&mut self) {
        let Some(find) = self.doc().find.as_ref() else {
            self.find_text.set_content(&mut self.font_system, "");
            return;
        };
        let count = find.match_count();
        let count_label = if count == 0 {
            "no matches".to_string()
        } else {
            format!("{}/{}", find.current_index() + 1, count)
        };
        let (q_mark, r_mark) = match find.focus() {
            FindFocus::Query => ("❯", " "),
            FindFocus::Replacement => (" ", "❯"),
        };
        let mut flags = String::new();
        if find.case_sensitive() {
            flags.push_str(" · Aa");
        }
        if find.whole_word() {
            flags.push_str(" · ab|");
        }
        let caption = format!(
            "{q_mark} Find:    {query}   {count_label}{flags}\n{r_mark} Replace: {replacement}",
            query = find.query(),
            replacement = find.replacement(),
        );
        self.find_text.set_content(&mut self.font_system, &caption);
    }

    /// Move the editor selection to the find bar's current match.
    fn select_current_match(&mut self) {
        let Some(range) = self.doc().find.as_ref().and_then(|f| f.current_match()) else {
            return;
        };
        self.doc_mut()
            .editor
            .set_selection(Selection::new(range.start, range.end));
        self.scene_dirty = true;
        self.follow_caret = true;
    }

    /// Route a key while the find bar is open.
    fn handle_find_key(&mut self, event: KeyEvent) {
        // Cmd-V pastes the clipboard into the find query. Checked
        // before the alt-modified toggles so a Cmd-V chord doesn't
        // also fire the case-toggle by accident on any layout.
        if is_cmd_or_ctrl(self.modifiers)
            && !self.modifiers.alt_key()
            && shortcut_letter(&event, 'v')
        {
            if let Some(text) = clipboard_get() {
                let buffer_text = self.doc().editor.text();
                if let Some(f) = self.doc_mut().find.as_mut() {
                    for c in text.chars() {
                        f.push_char(c, &buffer_text);
                    }
                }
                self.refresh_find_text();
                self.select_current_match();
                self.scene_dirty = true;
                self.window.request_redraw();
            }
            return;
        }
        // Cmd-Alt-C / Cmd-Alt-W toggle match-case / whole-word — checked
        // before the regular character branch so the chord doesn't insert
        // 'c' or 'w' into the query.
        if is_cmd_or_ctrl(self.modifiers) && self.modifiers.alt_key() {
            if let Some(lower) = shortcut_letter_of(&event) {
                let buffer_text = self.doc().editor.text();
                match lower.as_str() {
                    "c" => {
                        if let Some(f) = self.doc_mut().find.as_mut() {
                            f.toggle_case_sensitive(&buffer_text);
                        }
                        self.refresh_find_text();
                        self.select_current_match();
                        self.scene_dirty = true;
                        self.window.request_redraw();
                        return;
                    }
                    "w" => {
                        if let Some(f) = self.doc_mut().find.as_mut() {
                            f.toggle_whole_word(&buffer_text);
                        }
                        self.refresh_find_text();
                        self.select_current_match();
                        self.scene_dirty = true;
                        self.window.request_redraw();
                        return;
                    }
                    _ => {}
                }
            }
        }
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => self.close_find(),
            Key::Named(NamedKey::Tab) => {
                if let Some(f) = self.doc_mut().find.as_mut() {
                    f.toggle_focus();
                }
                self.refresh_find_text();
                self.scene_dirty = true;
                self.window.request_redraw();
            }
            Key::Named(NamedKey::Enter) => {
                let cmd_alt = is_cmd_or_ctrl(self.modifiers) && self.modifiers.alt_key();
                if cmd_alt {
                    self.replace_all();
                    return;
                }
                let shift = self.modifiers.shift_key();
                let focus = self
                    .doc()
                    .find
                    .as_ref()
                    .map(|f| f.focus())
                    .unwrap_or(FindFocus::Query);
                match (focus, shift) {
                    (FindFocus::Replacement, false) => {
                        self.replace_current();
                    }
                    (_, true) => {
                        if let Some(f) = self.doc_mut().find.as_mut() {
                            f.prev_match();
                        }
                        self.refresh_find_text();
                        self.select_current_match();
                        self.scene_dirty = true;
                        self.window.request_redraw();
                    }
                    (_, false) => {
                        if let Some(f) = self.doc_mut().find.as_mut() {
                            f.next_match();
                        }
                        self.refresh_find_text();
                        self.select_current_match();
                        self.scene_dirty = true;
                        self.window.request_redraw();
                    }
                }
            }
            Key::Named(NamedKey::Backspace) => {
                let text = self.doc().editor.text();
                if let Some(f) = self.doc_mut().find.as_mut() {
                    f.backspace(&text);
                }
                self.refresh_find_text();
                self.select_current_match();
                self.scene_dirty = true;
                self.window.request_redraw();
            }
            _ => {
                if is_cmd_or_ctrl(self.modifiers) {
                    return;
                }
                if let Some(text) = &event.text {
                    let buffer_text = self.doc().editor.text();
                    let typing_in_query = self
                        .doc()
                        .find
                        .as_ref()
                        .map(|f| f.focus() == FindFocus::Query)
                        .unwrap_or(true);
                    if let Some(f) = self.doc_mut().find.as_mut() {
                        for c in text.chars() {
                            f.push_char(c, &buffer_text);
                        }
                    }
                    self.refresh_find_text();
                    // Only chase the new match when the user is editing the
                    // *query* — typing in the replacement field shouldn't
                    // jump the editor around.
                    if typing_in_query {
                        self.select_current_match();
                    }
                    self.scene_dirty = true;
                    self.window.request_redraw();
                }
            }
        }
    }

    /// Replace the currently-highlighted match with the bar's replacement
    /// string, then refresh matches and advance to the next match (if any).
    fn replace_current(&mut self) {
        let (range, replacement) = match self.doc().find.as_ref() {
            Some(bar) => match bar.current_match() {
                Some(r) => (r, bar.replacement().to_string()),
                None => return,
            },
            None => return,
        };
        {
            let editor = &mut self.doc_mut().editor;
            editor.set_selection(Selection::new(range.start, range.end));
            editor.insert(&replacement);
        }
        let buffer_text = self.doc().editor.text();
        if let Some(f) = self.doc_mut().find.as_mut() {
            f.refresh(&buffer_text);
        }
        if !self.doc().dirty {
            self.doc_mut().dirty = true;
            self.update_title();
            self.refresh_tabs_text();
        }
        self.text_dirty = true;
        self.scene_dirty = true;
        self.follow_caret = true;
        self.refresh_find_text();
        self.select_current_match();
        self.window.request_redraw();
    }

    /// Replace every match in buffer order with the bar's replacement
    /// string. Walks in reverse so earlier positions don't shift after we
    /// touch later ones, then refreshes matches and flashes the count.
    fn replace_all(&mut self) {
        let (matches, replacement) = match self.doc().find.as_ref() {
            Some(bar) => (bar.matches().to_vec(), bar.replacement().to_string()),
            None => return,
        };
        if matches.is_empty() {
            return;
        }
        let count = matches.len();
        for range in matches.iter().rev() {
            let editor = &mut self.doc_mut().editor;
            editor.set_selection(Selection::new(range.start, range.end));
            editor.insert(&replacement);
        }
        let buffer_text = self.doc().editor.text();
        if let Some(f) = self.doc_mut().find.as_mut() {
            f.refresh(&buffer_text);
        }
        if !self.doc().dirty {
            self.doc_mut().dirty = true;
            self.update_title();
            self.refresh_tabs_text();
        }
        self.text_dirty = true;
        self.scene_dirty = true;
        self.follow_caret = true;
        self.refresh_find_text();
        self.set_status_flash(format!("replaced {count}"));
        self.window.request_redraw();
    }

    /// The find bar's backdrop rectangle. Two rows tall (Find / Replace),
    /// tucked under the tab strip.
    fn find_panel_rect(&self) -> Rect {
        let pad = FIND_PAD_DIP * self.scale;
        let width = FIND_WIDTH_DIP * self.scale;
        let top = (TAB_BAR_HEIGHT_DIP + FIND_TOP_DIP) * self.scale;
        let height = 2.0 * self.line_height() + 2.0 * pad;
        let surface_w = self.gpu.surface_config.width as f32;
        let left = surface_w - width - FIND_TOP_DIP * self.scale;
        Rect::new(left.max(0.0), top, width, height)
    }

    fn find_text_origin(&self) -> (f32, f32) {
        let panel = self.find_panel_rect();
        let pad = FIND_PAD_DIP * self.scale;
        (panel.min_x() + pad, panel.min_y() + pad)
    }

    /// Hover popup geometry: hangs one line below the caret, snapping into
    /// the viewport if it would overflow the right edge. Returns `None`
    /// when the caret is offscreen or no popup is active.
    fn hover_panel_rect(&self) -> Option<Rect> {
        let hover = self.hover_popup.as_ref()?;
        let (cx, cy) = self.caret_pixel(hover.anchor_char)?;
        let scroll = self.doc().scroll_y;
        let line_h = self.line_height();
        let pad = HOVER_PAD_DIP * self.scale;
        let width = HOVER_WIDTH_DIP * self.scale;
        let max_h = HOVER_MAX_HEIGHT_DIP * self.scale;
        // Caret pixel is in scroll-local coords (returned by caret_pixel
        // includes the line_top offset only). Anchor the popup just under
        // the caret line.
        let top = self.text_inset_y + cy - scroll + line_h + 4.0 * self.scale;
        // Measure the shaped hover text height (each layout run is one
        // visual line at line_h) and cap at max_h.
        let lines = self.hover_text.buffer.layout_runs().count().max(1) as f32;
        let height = (lines * line_h + 2.0 * pad).min(max_h);
        let surface_w = self.gpu.surface_config.width as f32;
        let left = (self.text_inset_x + cx).min(surface_w - width - pad);
        Some(Rect::new(left.max(0.0), top, width, height))
    }

    fn hover_text_origin(&self) -> Option<(f32, f32)> {
        let panel = self.hover_panel_rect()?;
        let pad = HOVER_PAD_DIP * self.scale;
        Some((panel.min_x() + pad, panel.min_y() + pad))
    }

    /// Completion-popup geometry. Anchors to the caret like the hover
    /// popup, but height is row-based (line height × visible row count).
    fn completion_panel_rect(&self) -> Option<Rect> {
        let popup = self.completion.as_ref()?;
        let (cx, cy) = self.caret_pixel(popup.anchor_char)?;
        let scroll = self.doc().scroll_y;
        let line_h = self.line_height();
        let pad = COMPLETION_PAD_DIP * self.scale;
        let width = COMPLETION_WIDTH_DIP * self.scale;
        let visible_rows = popup.filtered.len().clamp(1, COMPLETION_MAX_ROWS);
        let height = visible_rows as f32 * line_h + 2.0 * pad;
        let top = self.text_inset_y + cy - scroll + line_h + 2.0 * self.scale;
        let surface_w = self.gpu.surface_config.width as f32;
        let left = (self.text_inset_x + cx).min(surface_w - width - pad);
        Some(Rect::new(left.max(0.0), top, width, height))
    }

    fn completion_text_origin(&self) -> Option<(f32, f32)> {
        let panel = self.completion_panel_rect()?;
        let pad = COMPLETION_PAD_DIP * self.scale;
        Some((panel.min_x() + pad, panel.min_y() + pad))
    }

    /// Highlight rect for the currently-selected row in the completion
    /// popup, in surface coordinates.
    fn completion_selection_rect(&self) -> Option<Rect> {
        let popup = self.completion.as_ref()?;
        let panel = self.completion_panel_rect()?;
        let pad = COMPLETION_PAD_DIP * self.scale;
        let line_h = self.line_height();
        let visible_idx = popup.selected.checked_sub(popup.scroll)?;
        if visible_idx >= COMPLETION_MAX_ROWS {
            return None;
        }
        let y = panel.min_y() + pad + (visible_idx as f32) * line_h;
        Some(Rect::new(panel.min_x(), y, panel.size.width, line_h))
    }

    fn match_highlight_rects(&self) -> Vec<Rect> {
        let Some(find) = self.doc().find.as_ref() else {
            return Vec::new();
        };
        let current = find.current_index();
        find.matches()
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != current)
            .flat_map(|(_, r)| self.selection_rects(&Selection::new(r.start, r.end)))
            .collect()
    }

    /// Open a fresh scratch document in a new tab (Cmd-N). If the active tab
    /// is already a pristine scratch we just stay there.
    fn new_file(&mut self) {
        if self.doc().is_pristine_scratch() {
            return;
        }
        self.open_new_tab();
    }

    /// If the active document is dirty, ask the user what to do. Returns
    /// `true` if the caller may proceed (saved or discarded), `false` if it
    /// must abort. Clean documents always return `true`.
    fn confirm_unsaved(&mut self, reason: &str) -> bool {
        if !self.doc().dirty {
            return true;
        }
        let name = self.doc().label();
        let result = rfd::MessageDialog::new()
            .set_title(format!("{reason}: unsaved changes"))
            .set_description(format!("{name} has unsaved changes. Save them first?"))
            .set_level(rfd::MessageLevel::Warning)
            .set_buttons(rfd::MessageButtons::YesNoCancel)
            .show();
        match result {
            rfd::MessageDialogResult::Yes => {
                self.save_to_file();
                !self.doc().dirty
            }
            rfd::MessageDialogResult::No => true,
            _ => false,
        }
    }

    /// Close-window guard: if any document is dirty, prompt for it one by one.
    /// Returns `true` if the window may close.
    fn confirm_close_all(&mut self) -> bool {
        let mut i = 0;
        while i < self.docs.len() {
            if self.docs[i].dirty {
                self.active = i;
                self.update_title();
                if !self.confirm_unsaved("Close") {
                    return false;
                }
            }
            i += 1;
        }
        true
    }

    /// Pointer moved. During a drag, extend the selection from the drag anchor
    /// to the pointer; a drag past the top/bottom edge also scrolls the view.
    /// Also updates the hovered close-"×" state for visual feedback.
    fn handle_mouse_move(&mut self, x: f32, y: f32) {
        self.mouse_pos = Some((x, y));
        let new_hover = self.tab_close_at_pixel(x, y);
        if new_hover != self.hovered_close {
            self.hovered_close = new_hover;
            self.scene_dirty = true;
            self.window.request_redraw();
        }
        // Sidebar drag-resize: while the resize handle is held, every
        // mouse move slides the right edge. The branch returns so the
        // selection-drag path below doesn't also fire on the same
        // motion.
        if let Some(grab_offset) = self.sidebar_resize_drag {
            self.update_sidebar_width_from_drag(x, grab_offset);
            return;
        }
        let Some(anchor) = self.drag_anchor else {
            return;
        };

        // Bottom edge of the editor viewport — above the status bar so the
        // drag-to-scroll trigger lines up with where the user can still see
        // the caret.
        let viewport_bottom =
            self.gpu.surface_config.height as f32 - STATUS_BAR_HEIGHT_DIP * self.scale;
        let max = self.max_scroll();
        let scroll = self.doc().scroll_y;
        let new_scroll = if y < self.text_inset_y {
            (scroll - (self.text_inset_y - y)).clamp(0.0, max)
        } else if y > viewport_bottom {
            (scroll + (y - viewport_bottom)).clamp(0.0, max)
        } else {
            scroll
        };
        if new_scroll != scroll {
            self.doc_mut().scroll_y = new_scroll;
        }

        let Some(head) = self.char_at_pixel(x, y) else {
            return;
        };
        self.doc_mut()
            .editor
            .set_selection(Selection::new(anchor, head));
        self.scene_dirty = true;
        self.follow_caret = false;
        self.window.request_redraw();
    }

    /// Mouse wheel: route to whichever pane is under the pointer —
    /// the find-in-files panel and the terminal pane each claim
    /// wheel events when the cursor is over them; otherwise the
    /// editor scrolls.
    fn handle_scroll(&mut self, delta_y: f32) {
        // Terminal pane: scroll its scrollback. alacritty's
        // `Grid::scroll_display(Scroll::Delta(N))` shifts the
        // display offset by N lines (positive = scroll up into
        // history, negative = back toward the live tail).
        if self.terminal_pane_height() > 0.0 {
            if let Some((mx, my)) = self.mouse_pos {
                let pane = self.terminal_pane_rect();
                if pane.contains(Point::new(mx, my)) {
                    let lines = (delta_y / self.line_height()).round() as i32;
                    if lines != 0 {
                        if let Some(t) = self.terminal.as_ref() {
                            let mut term = t.term.lock();
                            term.scroll_display(alacritty_terminal::grid::Scroll::Delta(lines));
                        }
                        self.scene_dirty = true;
                        self.window.request_redraw();
                    }
                    return;
                }
            }
        }
        // Find-in-files panel claims wheel events while open and the
        // pointer is over it — otherwise scroll wheel just falls
        // through to the editor.
        if self.find_in_files.is_some() {
            if let Some((mx, my)) = self.mouse_pos {
                let panel = self.find_in_files_panel_rect();
                if panel.contains(Point::new(mx, my)) {
                    self.scroll_find_in_files(delta_y);
                    return;
                }
            }
        }
        // File-tree sidebar claims wheel events when the pointer is
        // over it, so the tree's own scroll position moves instead of
        // the editor's scrolling under a stationary tree.
        if self.file_tree.visible {
            if let Some((mx, my)) = self.mouse_pos {
                if self.in_sidebar(mx, my) {
                    self.scroll_file_tree(delta_y);
                    return;
                }
            }
        }
        let max = self.max_scroll();
        let current = self.doc().scroll_y;
        let new = (current - delta_y).clamp(0.0, max);
        if new != current {
            self.doc_mut().scroll_y = new;
            self.scene_dirty = true;
            self.window.request_redraw();
        }
    }

    /// Scroll the file-tree sidebar by the wheel delta. The same
    /// clamp [`scroll_selected_into_view`](Self::scroll_selected_into_view)
    /// uses so the panel can't scroll past the bottom of its content.
    fn scroll_file_tree(&mut self, delta_y: f32) {
        let line_h = self.line_height();
        if line_h <= 0.0 {
            return;
        }
        let body_h = (self.editor_bottom_y() - TAB_BAR_HEIGHT_DIP * self.scale).max(line_h);
        let max = ((self.file_tree.nodes.len() as f32) * line_h - body_h).max(0.0);
        let new = (self.file_tree.scroll_y - delta_y).clamp(0.0, max);
        if (new - self.file_tree.scroll_y).abs() > f32::EPSILON {
            self.file_tree.scroll_y = new;
            self.scene_dirty = true;
            self.window.request_redraw();
        }
    }

    /// Scroll the find-in-files results list by the wheel-equivalent
    /// number of rows.
    fn scroll_find_in_files(&mut self, delta_y: f32) {
        let line_h = self.line_height();
        // Convert pixel delta to row delta (round up so even a small
        // wheel tick advances one row).
        let row_delta = (delta_y / line_h).round() as i32;
        if row_delta == 0 {
            return;
        }
        let visible = self.find_in_files_visible_rows();
        let max_scroll = {
            let Some(f) = self.find_in_files.as_ref() else {
                return;
            };
            f.results.len().saturating_sub(visible)
        };
        if let Some(f) = self.find_in_files.as_mut() {
            let new = (f.scroll as i32 - row_delta).clamp(0, max_scroll as i32) as usize;
            if new == f.scroll {
                return;
            }
            f.scroll = new;
        }
        self.refresh_find_in_files_text();
        self.scene_dirty = true;
        self.window.request_redraw();
    }

    /// How many result rows fit inside the panel given its height and
    /// the current line metric. At least 1 so the math doesn't divide
    /// by zero on a too-small window.
    fn find_in_files_visible_rows(&self) -> usize {
        let panel = self.find_in_files_panel_rect();
        let pad = FIND_FILES_PAD_DIP * self.scale;
        let inner_h = (panel.size.height - 2.0 * pad).max(0.0);
        let line_h = self.line_height();
        let total_rows = (inner_h / line_h) as usize;
        total_rows.saturating_sub(FIND_FILES_HEADER_ROWS).max(1)
    }

    /// Total shaped text height in physical pixels — visual, not logical:
    /// wrapped lines count once per visible row.
    fn content_height(&self) -> f32 {
        self.text
            .buffer
            .layout_runs()
            .map(|run| run.line_top + run.line_height)
            .fold(0.0_f32, f32::max)
            .max(self.line_height())
    }

    /// Visible height of the editor viewport in physical pixels — the surface
    /// minus the tab strip on top, the status bar on the bottom, and the
    /// terminal pane (when visible) above the status bar.
    fn visible_height(&self) -> f32 {
        self.gpu.surface_config.height as f32
            - self.text_inset_y
            - STATUS_BAR_HEIGHT_DIP * self.scale
            - self.terminal_pane_height()
    }

    /// Bottom y of the editor surface (above the terminal pane, if any,
    /// then above the status bar). Used by the gutter/sidebar/text bounds
    /// to clip rendering above the pane.
    fn editor_bottom_y(&self) -> f32 {
        self.gpu.surface_config.height as f32
            - STATUS_BAR_HEIGHT_DIP * self.scale
            - self.terminal_pane_height()
    }

    /// The largest valid scroll offset — content height beyond the viewport.
    fn max_scroll(&self) -> f32 {
        (self.content_height() - self.visible_height()).max(0.0)
    }

    /// Scroll the viewport the minimum amount needed to bring the primary
    /// caret fully into view. A no-op when it is already visible.
    fn ensure_caret_visible(&mut self) {
        let head = self.doc().editor.selections().primary().head;
        let Some((_, caret_top)) = self.caret_pixel(head) else {
            return;
        };
        let caret_bottom = caret_top + self.line_height();
        let visible = self.visible_height();
        let max = self.max_scroll();
        let mut scroll = self.doc().scroll_y;

        if caret_top < scroll {
            scroll = caret_top;
        } else if caret_bottom > scroll + visible {
            scroll = caret_bottom - visible;
        }
        self.doc_mut().scroll_y = scroll.clamp(0.0, max);
    }

    /// Hit-test a physical-pixel point to a buffer `char` index.
    fn char_at_pixel(&self, x: f32, y: f32) -> Option<usize> {
        let tx = x - self.text_inset_x;
        let ty = y - self.text_inset_y + self.doc().scroll_y;
        let cursor = self.text.buffer.hit(tx, ty)?;

        let line_str = self.doc().editor.buffer().line(cursor.line)?;
        let column = line_str
            .char_indices()
            .take_while(|(b, _)| *b < cursor.index)
            .count();
        self.doc()
            .editor
            .buffer()
            .position_to_char(Position::new(cursor.line, column))
    }

    /// Rebuild `scene` from the current editor + tab state.
    fn rebuild_scene(&mut self) {
        let w = self.gpu.surface_config.width as f32;
        let h = self.gpu.surface_config.height as f32;
        let mut root = SceneNode::group(Rect::new(0.0, 0.0, w, h));

        let et = &self.theme.editor;
        // Tab strip backdrop — sits behind every tab slot.
        let tab_bar_bg = quad_color(&et.tab_bar_bg);
        let tab_active_bg = quad_color(&et.tab_active_bg);
        let tab_inactive_bg = quad_color(&et.tab_inactive_bg);
        let tab_separator = quad_color(&et.tab_separator);
        root.push_child(SceneNode::quad(self.tab_strip_rect(), tab_bar_bg));
        for (i, _) in self.docs.iter().enumerate() {
            let slot = self.tab_slot_rect(i);
            let bg = if i == self.active {
                tab_active_bg
            } else {
                tab_inactive_bg
            };
            root.push_child(SceneNode::quad(slot, bg));
            // Thin separator on the right edge of every tab except the last.
            if i + 1 < self.docs.len() {
                let sep = Rect::new(
                    slot.min_x() + slot.size.width - 1.0 * self.scale,
                    slot.min_y() + 4.0 * self.scale,
                    1.0 * self.scale,
                    slot.size.height - 8.0 * self.scale,
                );
                root.push_child(SceneNode::quad(sep, tab_separator));
            }
        }

        // File-tree sidebar backdrop, drawn first so the gutter and any
        // selection highlights stack on top.
        if self.file_tree.visible {
            root.push_child(SceneNode::quad(
                self.sidebar_rect(),
                quad_color(&et.gutter_bg),
            ));
            let top = TAB_BAR_HEIGHT_DIP * self.scale;
            let line_h = self.line_height();
            // Active-doc highlight: when one of the visible nodes
            // points at the active doc, draw a faint underlay so the
            // user can see "what they're editing now" at a glance.
            if let Some(active_path) = self.doc().file_path.clone() {
                if let Some(idx) = self
                    .file_tree
                    .nodes
                    .iter()
                    .position(|n| n.path == active_path)
                {
                    let y = top + (idx as f32) * line_h - self.file_tree.scroll_y;
                    root.push_child(SceneNode::quad(
                        Rect::new(0.0, y, self.sidebar_width(), line_h),
                        quad_color(&et.active_line_bg),
                    ));
                }
            }
            // Keyboard-selection highlight: a brighter bar on the row
            // the arrow keys last landed on. Painted only while the
            // panel is focused so it doesn't compete with the editor
            // caret for the user's attention when the tree is just
            // sitting there as a reference.
            if self.file_tree.focused {
                if let Some(idx) = self.file_tree.selected {
                    let y = top + (idx as f32) * line_h - self.file_tree.scroll_y;
                    root.push_child(SceneNode::quad(
                        Rect::new(0.0, y, self.sidebar_width(), line_h),
                        quad_color(&et.selection_bg),
                    ));
                }
            }
            // Vertical divider on the right edge of the sidebar so the
            // user can see where the drag-resize handle is — without
            // it the sidebar and gutter share `gutter_bg` and read
            // visually as one chrome column ending at the gutter's
            // right edge. The divider rides on top of the sidebar
            // backdrop and is drawn in the same colour as the active-
            // line underlay (subtle but distinct).
            let divider_w = SIDEBAR_DIVIDER_DIP * self.scale;
            root.push_child(SceneNode::quad(
                Rect::new(
                    self.sidebar_width() - divider_w,
                    top,
                    divider_w,
                    self.editor_bottom_y() - top,
                ),
                quad_color(&et.active_line_bg),
            ));
        }

        // Gutter backdrop — a slim column on the left of the editor
        // text, slightly darker so the line numbers read as belonging
        // to a chrome region rather than the buffer.
        let gutter_h = (self.editor_bottom_y() - self.text_inset_y).max(0.0);
        let sidebar_w = self.sidebar_width();
        root.push_child(SceneNode::quad(
            Rect::new(
                sidebar_w,
                self.text_inset_y,
                (self.text_inset_x - sidebar_w).max(0.0),
                gutter_h,
            ),
            quad_color(&et.gutter_bg),
        ));

        // Git gutter markers — a thin coloured bar on each line that
        // differs from HEAD. Green = added, blue = modified, red = a
        // deletion was anchored just above this line. Renders BEFORE
        // the diagnostic dots so the dot wins when both apply, and
        // BEFORE the line numbers so the number stays legible on top.
        if !self.doc().git_status.is_empty() {
            let scroll = self.doc().scroll_y;
            let line_h = self.line_height();
            let marker_w = 3.0 * self.scale;
            let marker_x = sidebar_w; // hug the left edge of the gutter
            for (&line, &status) in &self.doc().git_status {
                let y_top = self.text_inset_y + (line as f32) * line_h - scroll;
                if y_top + line_h < self.text_inset_y || y_top > self.text_inset_y + gutter_h {
                    continue;
                }
                let h = if matches!(status, git::GitLineStatus::Deleted) {
                    // A "lines were deleted just above here" wedge —
                    // short bar at the line's top edge.
                    line_h * 0.35
                } else {
                    line_h
                };
                root.push_child(SceneNode::quad(
                    Rect::new(marker_x, y_top, marker_w, h),
                    git_marker_color(status),
                ));
            }
        }

        // Diagnostic dots — one per line with at least one LSP diagnostic,
        // coloured by the highest-severity diagnostic on that line. Sits at
        // the left edge of the gutter so it doesn't compete with the line
        // numbers.
        if let Some(path) = self.doc().file_path.clone() {
            if let Some(diags) = self.lsp.diagnostics_for(&path) {
                let scroll = self.doc().scroll_y;
                let line_h = self.line_height();
                let dot = DIAG_DOT_DIP * self.scale;
                let x = (GUTTER_PAD_LEFT_DIP * self.scale - dot * 0.5).max(2.0);
                let mut per_line: HashMap<u32, lsp_types::DiagnosticSeverity> = HashMap::new();
                for d in diags {
                    let line = d.range.start.line;
                    let sev = d
                        .severity
                        .unwrap_or(lsp_types::DiagnosticSeverity::INFORMATION);
                    per_line
                        .entry(line)
                        .and_modify(|cur| {
                            if severity_rank(sev) < severity_rank(*cur) {
                                *cur = sev;
                            }
                        })
                        .or_insert(sev);
                }
                for (line, sev) in per_line {
                    let y_top = self.text_inset_y + (line as f32) * line_h - scroll;
                    // Skip dots that scrolled out of the gutter.
                    if y_top + line_h < self.text_inset_y || y_top > self.text_inset_y + gutter_h {
                        continue;
                    }
                    let y = y_top + (line_h - dot) * 0.5;
                    root.push_child(SceneNode::quad(
                        Rect::new(x, y, dot, dot),
                        diagnostic_color(Some(sev)),
                    ));
                }
            }
        }

        // Active line backdrop — a faint full-width row at every visual run
        // of the logical line where the primary caret sits. Behind the
        // selection so the selection's brighter blue still reads. Starts
        // at the gutter (after the sidebar) so the bar doesn't paint
        // over the file-tree panel.
        let active_logical = {
            let head = self.doc().editor.selections().primary().head;
            self.doc().editor.buffer().char_to_position(head).line
        };
        let scroll = self.doc().scroll_y;
        let line_h = self.line_height();
        let active_color = quad_color(&et.active_line_bg);
        let row_left = self.sidebar_width();
        let row_w = (w - row_left).max(0.0);
        for run in self.text.buffer.layout_runs() {
            if run.line_i != active_logical {
                continue;
            }
            let y = self.text_inset_y + run.line_top - scroll;
            root.push_child(SceneNode::quad(
                Rect::new(row_left, y, row_w, line_h),
                active_color,
            ));
        }

        // Indent guides — thin vertical lines every `indent_unit` chars of
        // leading whitespace per visible logical line. The unit comes from
        // the document, not the user's `tab_size` setting, so a 4-space-
        // indented file viewed at `tab_size = 2` still gets guides at the
        // file's actual indent boundaries.
        //
        // Each guide's x comes from the actual rendered glyph position at
        // that column (the leading space char's `x` in the layout run), so
        // there's no `char_width × column` approximation drift.
        let tab_size = self.doc().indent_unit.max(1);
        let guide_color = quad_color(&et.indent_guide);
        let guide_w = self.scale.max(1.0);
        let mut prev_logical_g = usize::MAX;
        for run in self.text.buffer.layout_runs() {
            if run.line_i == prev_logical_g {
                continue;
            }
            prev_logical_g = run.line_i;
            let Some(line_text) = self.doc().editor.buffer().line(run.line_i) else {
                continue;
            };
            let leading_ws = line_text
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .count();
            let levels = leading_ws / tab_size;
            if levels == 0 {
                continue;
            }
            let y = self.text_inset_y + run.line_top - scroll;
            // Each indent column is `tab_size` *bytes* in (leading whitespace
            // is one byte per char), so we can find the glyph whose
            // `start` byte hits that column and use its rendered `x`.
            for level in 1..=levels {
                let target_byte = level * tab_size;
                let Some(gx) = run
                    .glyphs
                    .iter()
                    .find(|g| g.start >= target_byte)
                    .map(|g| g.x)
                else {
                    continue;
                };
                let x = self.text_inset_x + gx;
                root.push_child(SceneNode::quad(
                    Rect::new(x, y, guide_w, line_h),
                    guide_color,
                ));
            }
        }

        // Bracket-pair highlight — when the caret sits next to a bracket,
        // both the bracket and its match get a faint outlined background.
        if let Some((a, b)) = self.matching_bracket_positions() {
            let bracket_w = self.measured_char_width().max(self.caret_width);
            let bracket_color = quad_color(&et.bracket_match);
            for pos in [a, b] {
                if let Some((cx, cy)) = self.caret_pixel(pos) {
                    let rect = Rect::new(
                        self.text_inset_x + cx,
                        self.text_inset_y + cy - scroll,
                        bracket_w,
                        line_h,
                    );
                    root.push_child(SceneNode::quad(rect, bracket_color));
                }
            }
        }

        // Selection highlights sit behind text and carets.
        let selection_color = quad_color(&et.selection_bg);
        for selection in self.doc().editor.selections().iter() {
            for rect in self.selection_rects(selection) {
                root.push_child(SceneNode::quad(rect, selection_color));
            }
        }

        // Carets on top of the highlights — skipped during the "off" half
        // of the blink cycle.
        if self.caret_visible {
            let caret_color = quad_color(&et.caret);
            for selection in self.doc().editor.selections().iter() {
                if let Some((cx, cy)) = self.caret_pixel(selection.head) {
                    root.push_child(SceneNode::quad(
                        Rect::new(
                            self.text_inset_x + cx,
                            self.text_inset_y + cy - scroll,
                            self.caret_width,
                            line_h,
                        ),
                        caret_color,
                    ));
                }
            }
        }

        // Find-bar match highlights.
        let find_match_color = quad_color(&et.find_match_bg);
        for rect in self.match_highlight_rects() {
            root.push_child(SceneNode::quad(rect, find_match_color));
        }

        // Terminal pane backdrop — sits above the status bar and
        // below the editor area. A slightly darker fill so it reads
        // as a separate chrome region.
        if self.terminal_pane_height() > 0.0 {
            let panel = self.terminal_pane_rect();
            root.push_child(SceneNode::quad(panel, quad_color(&et.gutter_bg)));

            // Per-cell background quads — `ls --color` legends,
            // selection highlights from `less`, INVERSE prompts and
            // so on. Drawn after the pane backdrop and before the
            // cursor so a cell's bg shows under the caret block (the
            // caret then overpaints it for the focused-cursor case).
            let pad = 6.0 * self.scale;
            let cell_w = self.terminal_measured_char_width();
            let cell_h = self.terminal_measured_line_height();
            if cell_w > 0.0 && cell_h > 0.0 {
                for run in &self.terminal_bg_runs {
                    let x = panel.min_x() + pad + (run.col_start as f32) * cell_w;
                    let y = panel.min_y() + pad + (run.row as f32) * cell_h;
                    let w = ((run.col_end - run.col_start) as f32) * cell_w;
                    root.push_child(SceneNode::quad(
                        Rect::new(x, y, w, cell_h),
                        SceneColor::rgba(run.color.r, run.color.g, run.color.b, 0xff),
                    ));
                }
            }

            // Cursor block — solid when the terminal has focus,
            // hollow-ish otherwise. Drawn under the text so the
            // glyph at that cell still reads on top.
            if let Some(rect) = self.terminal_cursor_rect() {
                let color = if self.terminal.as_ref().is_some_and(|t| t.focused) {
                    quad_color(&et.caret)
                } else {
                    quad_color(&et.active_line_bg)
                };
                root.push_child(SceneNode::quad(rect, color));
            }
        }

        // Status bar backdrop — opaque so it covers any text that scrolled
        // behind it (text bounds also clip, but defence in depth is cheap).
        root.push_child(SceneNode::quad(
            self.status_bar_rect(),
            quad_color(&et.status_bg),
        ));

        self.scene = Scene::new(root);
        self.rebuild_overlay_scene();
    }

    /// Rebuild the overlay-layer scene — floating panels that draw on
    /// top of the editor text (find bar, hover popup, completion
    /// popup, palette). Kept in a separate `Scene` + `QuadRenderer`
    /// so the editor text doesn't bleed through the opaque
    /// `overlay_bg` panels.
    fn rebuild_overlay_scene(&mut self) {
        let w = self.gpu.surface_config.width as f32;
        let h = self.gpu.surface_config.height as f32;
        let et = self.theme.editor.clone();
        let mut root = SceneNode::group(Rect::new(0.0, 0.0, w, h));

        if self.doc().find.is_some() {
            root.push_child(SceneNode::quad(
                self.find_panel_rect(),
                quad_color(&et.overlay_bg),
            ));
        }
        if self.hover_popup.is_some() {
            if let Some(rect) = self.hover_panel_rect() {
                root.push_child(SceneNode::quad(rect, quad_color(&et.overlay_bg)));
            }
        }
        if self.completion.is_some() {
            if let Some(rect) = self.completion_panel_rect() {
                root.push_child(SceneNode::quad(rect, quad_color(&et.overlay_bg)));
                if let Some(popup) = self.completion.as_ref() {
                    let lh = self.line_height();
                    let pad = COMPLETION_PAD_DIP * self.scale;
                    let track = Rect::new(
                        rect.min_x(),
                        rect.min_y() + pad,
                        rect.size.width,
                        (rect.size.height - 2.0 * pad).max(lh),
                    );
                    if let Some(thumb) = scrollbar_thumb(
                        track,
                        popup.filtered.len(),
                        COMPLETION_MAX_ROWS,
                        popup.scroll,
                        self.scale,
                    ) {
                        root.push_child(SceneNode::quad(thumb, quad_color(&et.indent_guide)));
                    }
                }
            }
            if let Some(rect) = self.completion_selection_rect() {
                root.push_child(SceneNode::quad(rect, quad_color(&et.palette_selection_bg)));
            }
        }
        if self.palette.is_some() {
            root.push_child(SceneNode::quad(
                Rect::new(0.0, 0.0, w, h),
                quad_color(&et.overlay_scrim),
            ));
            let panel = self.palette_panel_rect();
            root.push_child(SceneNode::quad(panel, quad_color(&et.overlay_bg)));
            if let Some(highlight) = self.palette_selection_rect() {
                root.push_child(SceneNode::quad(
                    highlight,
                    quad_color(&et.palette_selection_bg),
                ));
            }
            if let Some(p) = self.palette.as_ref() {
                let lh = self.line_height();
                let pad = PALETTE_PAD_DIP * self.scale;
                // Rows sit below the 2-line header (query + blank).
                let track = Rect::new(
                    panel.min_x(),
                    panel.min_y() + pad + 2.0 * lh,
                    panel.size.width,
                    (PALETTE_VISIBLE_ROWS as f32 * lh).min(panel.size.height),
                );
                if let Some(thumb) = scrollbar_thumb(
                    track,
                    p.visible_count(),
                    PALETTE_VISIBLE_ROWS,
                    p.scroll(),
                    self.scale,
                ) {
                    root.push_child(SceneNode::quad(thumb, quad_color(&et.indent_guide)));
                }
            }
        }
        if self.find_in_files.is_some() {
            // Scrim dims the editor behind the search panel so the
            // focus is unambiguous.
            root.push_child(SceneNode::quad(
                Rect::new(0.0, 0.0, w, h),
                quad_color(&et.overlay_scrim),
            ));
            let panel = self.find_in_files_panel_rect();
            root.push_child(SceneNode::quad(panel, quad_color(&et.overlay_bg)));
            if let Some(rect) = self.find_in_files_selection_rect() {
                root.push_child(SceneNode::quad(rect, quad_color(&et.palette_selection_bg)));
            }
            if let Some(f) = self.find_in_files.as_ref() {
                let lh = self.line_height();
                let pad = FIND_FILES_PAD_DIP * self.scale;
                let visible = self.find_in_files_visible_rows();
                // Results sit below the header rows (input + status).
                let track = Rect::new(
                    panel.min_x(),
                    panel.min_y() + pad + FIND_FILES_HEADER_ROWS as f32 * lh,
                    panel.size.width,
                    (visible as f32 * lh).min(panel.size.height),
                );
                if let Some(thumb) =
                    scrollbar_thumb(track, f.results.len(), visible, f.scroll, self.scale)
                {
                    root.push_child(SceneNode::quad(thumb, quad_color(&et.indent_guide)));
                }
            }
        }

        self.overlay_scene = Scene::new(root);
    }

    /// Selection highlight rectangles.
    fn selection_rects(&self, selection: &Selection) -> Vec<Rect> {
        if selection.is_cursor() {
            return Vec::new();
        }
        let buffer = self.doc().editor.buffer();
        let start = buffer.char_to_position(selection.start());
        let end = buffer.char_to_position(selection.end());
        let mut rects = Vec::new();

        let inset_x = self.text_inset_x;
        let inset_y = self.text_inset_y;
        let scroll = self.doc().scroll_y;
        let line_height = self.line_height();
        let mut push = |x0: f32, y: f32, x1: f32| {
            rects.push(Rect::new(
                inset_x + x0,
                inset_y + y - scroll,
                (x1 - x0).max(3.0),
                line_height,
            ));
        };

        if start.line == end.line {
            if let (Some((sx, sy)), Some((ex, _))) = (
                self.caret_pixel_at(start.line, start.column),
                self.caret_pixel_at(end.line, end.column),
            ) {
                push(sx, sy, ex);
            }
            return rects;
        }

        if let (Some((sx, sy)), Some((ex, _))) = (
            self.caret_pixel_at(start.line, start.column),
            self.caret_pixel_at(start.line, self.line_content_chars(start.line)),
        ) {
            push(sx, sy, ex);
        }
        for line in (start.line + 1)..end.line {
            if let (Some((x0, y)), Some((x1, _))) = (
                self.caret_pixel_at(line, 0),
                self.caret_pixel_at(line, self.line_content_chars(line)),
            ) {
                push(x0, y, x1);
            }
        }
        if let (Some((x0, y)), Some((ex, _))) = (
            self.caret_pixel_at(end.line, 0),
            self.caret_pixel_at(end.line, end.column),
        ) {
            push(x0, y, ex);
        }
        rects
    }

    fn line_content_chars(&self, line: usize) -> usize {
        self.doc()
            .editor
            .buffer()
            .line(line)
            .map(|s| {
                s.trim_end_matches('\n')
                    .trim_end_matches('\r')
                    .chars()
                    .count()
            })
            .unwrap_or(0)
    }

    fn caret_pixel(&self, char_idx: usize) -> Option<(f32, f32)> {
        let pos = self.doc().editor.buffer().char_to_position(char_idx);
        self.caret_pixel_at(pos.line, pos.column)
    }

    fn caret_pixel_at(&self, line: usize, column: usize) -> Option<(f32, f32)> {
        let line_str = self.doc().editor.buffer().line(line)?;
        let byte_in_line = line_str
            .char_indices()
            .nth(column)
            .map(|(b, _)| b)
            .unwrap_or(line_str.len());

        let mut last_run_end: Option<(f32, f32)> = None;

        for run in self.text.buffer.layout_runs() {
            if run.line_i != line {
                continue;
            }
            let run_start = run.glyphs.first().map(|g| g.start).unwrap_or(0);
            let run_end = run.glyphs.last().map(|g| g.end).unwrap_or(run_start);
            let run_end_x = run.glyphs.last().map(|g| g.x + g.w).unwrap_or(0.0);
            last_run_end = Some((run_end_x, run.line_top));

            if byte_in_line >= run_start && byte_in_line <= run_end {
                let mut x = 0.0;
                for glyph in run.glyphs.iter() {
                    if glyph.start >= byte_in_line {
                        return Some((glyph.x, run.line_top));
                    }
                    x = glyph.x + glyph.w;
                }
                return Some((x, run.line_top));
            }
        }

        last_run_end.or(Some((0.0, line as f32 * self.line_height())))
    }

    fn render(&mut self) {
        let frame_start = Instant::now();
        // Phase-by-phase timing checkpoints. Only logged when the frame
        // overruns the hard latency limit, so the steady-state log stays
        // quiet.
        let mut t = FrameTimings::default();

        if self.text_dirty {
            let new_text = self.docs[self.active].editor.text();
            t.text_materialize = frame_start.elapsed();
            if self.visible_whitespace {
                // Visible-whitespace substitution changes some chars' byte
                // widths, so the syntax char ranges won't line up — fall
                // back to plain shaping in that mode.
                let shaped = substitute_whitespace(&new_text);
                self.text.set_content(&mut self.font_system, &shaped);
            } else if self.docs[self.active].highlighter.is_some() {
                // Cache key: editor revision. Switching tabs back to a doc
                // that hasn't changed since the last parse skips the whole
                // tree-sitter pass. Edits between parses are drained from
                // the editor and fed to the highlighter so tree-sitter can
                // reparse incrementally instead of from scratch.
                let revision = self.docs[self.active].editor.revision();
                let needs_parse = self.docs[self.active].cached_revision != Some(revision);
                if needs_parse {
                    let pending = self.docs[self.active].editor.take_pending_edits();
                    let highlighter = self.docs[self.active].highlighter.as_mut().unwrap();
                    if pending.tree_invalidated {
                        highlighter.reset();
                    } else {
                        for edit in &pending.edits {
                            highlighter.apply_edit(edit);
                        }
                    }
                    let highlights = highlighter.highlight(&new_text);
                    self.docs[self.active].cached_highlights = highlights;
                    self.docs[self.active].cached_revision = Some(revision);
                }
                t.syntax_parse = frame_start.elapsed();
                let default_color = text_color(&self.theme.editor.text_fg);
                let spans = build_highlight_spans(
                    &new_text,
                    &self.docs[self.active].cached_highlights,
                    default_color,
                    &self.theme.syntax,
                );
                t.build_spans = frame_start.elapsed();
                self.text.set_content_rich(&mut self.font_system, spans);
            } else {
                self.text.set_content(&mut self.font_system, &new_text);
            }
            t.reshape = frame_start.elapsed();
            self.text_dirty = false;
            // Ship the freshly-materialised text to the LSP server for
            // the active doc. The helper short-circuits when no server is
            // wired for this language, so the non-LSP path is free.
            self.lsp_did_change_active(&new_text);
            t.lsp_send = frame_start.elapsed();
            // Refresh per-line git status against HEAD. Cheap on
            // libgit2 (single-digit ms even on a 4000-line file) and
            // gated by the editor's revision counter so unchanged
            // tab-switches don't re-diff.
            self.refresh_git_status(&new_text);
        }
        if self.scene_dirty {
            if self.follow_caret {
                self.ensure_caret_visible();
                self.follow_caret = false;
            }
            self.refresh_gutter();
            self.refresh_status();
            self.rebuild_scene();
            self.scene_dirty = false;
        }
        t.scene = frame_start.elapsed();

        self.quads.prepare(
            &self.gpu.device,
            &self.gpu.queue,
            &self.scene,
            self.gpu.surface_config.width as f32,
            self.gpu.surface_config.height as f32,
        );
        self.overlay_quads.prepare(
            &self.gpu.device,
            &self.gpu.queue,
            &self.overlay_scene,
            self.gpu.surface_config.width as f32,
            self.gpu.surface_config.height as f32,
        );
        t.quads_prepare = frame_start.elapsed();

        let surface_w = self.gpu.surface_config.width;
        let surface_h = self.gpu.surface_config.height;
        let resolution = Resolution {
            width: surface_w,
            height: surface_h,
        };
        let full_bounds = TextBounds {
            left: 0,
            top: 0,
            right: surface_w as i32,
            bottom: surface_h as i32,
        };
        // Editor text clips to the viewport between the tab strip and the
        // status bar; otherwise scrolled glyphs would bleed into both bars
        // (which are drawn as quads underneath the text pass).
        let editor_text_bounds = TextBounds {
            left: 0,
            top: self.text_inset_y as i32,
            right: surface_w as i32,
            // Clip above the terminal pane too — without this, scrolled
            // editor glyphs paint underneath the pane's backdrop.
            bottom: self.editor_bottom_y() as i32,
        };

        // All text — editor / tabs / close ×s / status / find / palette —
        // batched into a single `prepare` + `render`. The order TextAreas
        // appear in the vec is the draw order, so overlays go last.
        let inset_x = self.text_inset_x;
        let inset_y = self.text_inset_y;
        let scroll = self.docs[self.active].scroll_y;
        let editor_color = text_color(&self.theme.editor.text_fg);
        let label_color = text_color(&self.theme.editor.tab_label_fg);
        let dim_color = text_color(&self.theme.editor.close_button);
        let gutter_dim = text_color(&self.theme.editor.gutter_fg);
        let gutter_active = text_color(&self.theme.editor.gutter_active_fg);
        let status_fg = text_color(&self.theme.editor.status_fg);
        // The line number for the line that has the primary caret on it
        // gets the brighter colour. Computed once here so the gutter loop
        // can skip emitting it dim.
        let active_line = {
            let head = self.doc().editor.selections().primary().head;
            self.doc().editor.buffer().char_to_position(head).line
        };

        let tab_strip_h = TAB_BAR_HEIGHT_DIP * self.scale;
        let tab_text_pad_x = TAB_PAD_X_DIP * self.scale;
        let tab_text_y = (tab_strip_h - self.line_height()) * 0.5;
        let strip_bounds = TextBounds {
            left: 0,
            top: 0,
            right: surface_w as i32,
            bottom: tab_strip_h as i32,
        };

        let docs_len = self.docs.len();
        let slot_w = TAB_WIDTH_DIP * self.scale;
        let close_w = TAB_CLOSE_W_DIP * self.scale;
        let close_pad = TAB_CLOSE_PAD_DIP * self.scale;
        let close_glyph_offset_x = close_w * 0.25;

        let status_bar = self.status_bar_rect();
        let status_y = status_bar.min_y() + (status_bar.size.height - self.line_height()) * 0.5;
        let status_pad_x = STATUS_PAD_X_DIP * self.scale;
        let status_left_x = status_bar.min_x() + status_pad_x;
        let status_right_x = (status_bar.min_x() + status_bar.size.width
            - status_pad_x
            - shaped_width(&self.status_right))
        .max(status_left_x);
        let status_bounds = TextBounds {
            left: 0,
            top: status_bar.min_y() as i32,
            right: surface_w as i32,
            bottom: surface_h as i32,
        };

        let find_open = self.doc().find.is_some();
        let find_xy = if find_open {
            Some(self.find_text_origin())
        } else {
            None
        };

        let palette_open = self.palette.is_some();
        let palette_xy = if palette_open {
            Some(self.palette_text_origin())
        } else {
            None
        };

        // Gutter line numbers are emitted one TextArea per *visible logical
        // buffer line*. We use the editor's own layout to find each line's
        // first visual run, then position the gutter buffer (which contains
        // every line stacked vertically) so the matching gutter row lines
        // up with the editor row — clip-bounds to one row hide the rest of
        // the buffer.
        let gutter_left = self.sidebar_width() + GUTTER_PAD_LEFT_DIP * self.scale;
        let line_height = self.line_height();
        let viewport_top = self.text_inset_y;
        let viewport_bottom = self.editor_bottom_y();

        // Snapshot the terminal's grid into its TextStack *before*
        // the text_areas Vec borrows TextStack buffers immutably.
        if self.terminal_pane_height() > 0.0 {
            self.refresh_terminal_text();
        }

        let mut text_areas: Vec<TextArea> = Vec::with_capacity(8 + docs_len);
        text_areas.push(TextArea {
            buffer: &self.text.buffer,
            left: inset_x,
            top: inset_y - scroll,
            scale: 1.0,
            bounds: editor_text_bounds,
            default_color: editor_color,
            custom_glyphs: &[],
        });
        let mut prev_logical = usize::MAX;
        let mut active_gutter_position: Option<(f32, TextBounds)> = None;
        for run in self.text.buffer.layout_runs() {
            // Only the first visual run of each logical line carries a
            // number; wrapped continuations leave the gutter blank.
            if run.line_i == prev_logical {
                continue;
            }
            prev_logical = run.line_i;
            let row_top = inset_y + run.line_top - scroll;
            // Skip rows entirely outside the editor viewport.
            if row_top + line_height < viewport_top || row_top > viewport_bottom {
                continue;
            }
            let area_top = row_top - run.line_i as f32 * line_height;
            let bounds = TextBounds {
                // Clip the gutter row to the gutter column only — when
                // the sidebar is open, leaving `left: 0` would draw the
                // line number on top of the sidebar's text.
                left: self.sidebar_width() as i32,
                top: row_top.max(viewport_top) as i32,
                right: inset_x as i32,
                bottom: (row_top + line_height).min(viewport_bottom) as i32,
            };
            if run.line_i == active_line {
                // Defer the active line to a second TextArea drawn with the
                // brighter colour; emitting it dim too would blend.
                active_gutter_position = Some((area_top, bounds));
                continue;
            }
            text_areas.push(TextArea {
                buffer: &self.gutter_text.buffer,
                left: gutter_left,
                top: area_top,
                scale: 1.0,
                bounds,
                default_color: gutter_dim,
                custom_glyphs: &[],
            });
        }
        if let Some((area_top, bounds)) = active_gutter_position {
            text_areas.push(TextArea {
                buffer: &self.gutter_text.buffer,
                left: gutter_left,
                top: area_top,
                scale: 1.0,
                bounds,
                default_color: gutter_active,
                custom_glyphs: &[],
            });
        }
        text_areas.push(TextArea {
            buffer: &self.tabs_text.buffer,
            left: tab_text_pad_x,
            top: tab_text_y,
            scale: 1.0,
            bounds: strip_bounds,
            default_color: label_color,
            custom_glyphs: &[],
        });
        if self.file_tree.visible {
            let sidebar = self.sidebar_rect();
            let pad_x = SIDEBAR_PAD_X_DIP * self.scale;
            let sidebar_bounds = TextBounds {
                left: sidebar.min_x() as i32,
                top: sidebar.min_y() as i32,
                right: sidebar.max_x() as i32,
                bottom: sidebar.max_y() as i32,
            };
            text_areas.push(TextArea {
                buffer: &self.file_tree_text.buffer,
                left: sidebar.min_x() + pad_x,
                top: sidebar.min_y() - self.file_tree.scroll_y,
                scale: 1.0,
                bounds: sidebar_bounds,
                default_color: editor_color,
                custom_glyphs: &[],
            });
        }
        let hovered_close = self.hovered_close;
        for i in 0..docs_len {
            let slot_x = i as f32 * slot_w;
            let close_x = slot_x + slot_w - close_w - close_pad + close_glyph_offset_x;
            let color = if Some(i) == hovered_close {
                editor_color
            } else {
                dim_color
            };
            text_areas.push(TextArea {
                buffer: &self.close_text.buffer,
                left: close_x,
                top: tab_text_y,
                scale: 1.0,
                bounds: strip_bounds,
                default_color: color,
                custom_glyphs: &[],
            });
        }
        if self.terminal_pane_height() > 0.0 {
            let pane = self.terminal_pane_rect();
            let pad = 6.0 * self.scale;
            let bounds = TextBounds {
                left: pane.min_x() as i32,
                top: pane.min_y() as i32,
                right: pane.max_x() as i32,
                bottom: pane.max_y() as i32,
            };
            text_areas.push(TextArea {
                buffer: &self.terminal_text.buffer,
                left: pane.min_x() + pad,
                top: pane.min_y() + pad,
                scale: 1.0,
                bounds,
                default_color: editor_color,
                custom_glyphs: &[],
            });
        }
        text_areas.push(TextArea {
            buffer: &self.status_left.buffer,
            left: status_left_x,
            top: status_y,
            scale: 1.0,
            bounds: status_bounds,
            default_color: status_fg,
            custom_glyphs: &[],
        });
        text_areas.push(TextArea {
            buffer: &self.status_right.buffer,
            left: status_right_x,
            top: status_y,
            scale: 1.0,
            bounds: status_bounds,
            default_color: status_fg,
            custom_glyphs: &[],
        });
        // ── Overlay-layer text — drawn AFTER the main editor text in
        // a separate `prepare`/`render` pass so the popups' panel
        // backgrounds (also overlay-layer quads) correctly occlude
        // the editor text behind them.
        let mut overlay_text_areas: Vec<TextArea> = Vec::with_capacity(4);
        if let Some((fx, fy)) = find_xy {
            overlay_text_areas.push(TextArea {
                buffer: &self.find_text.buffer,
                left: fx,
                top: fy,
                scale: 1.0,
                bounds: full_bounds,
                default_color: editor_color,
                custom_glyphs: &[],
            });
        }
        if let Some((px, py)) = palette_xy {
            overlay_text_areas.push(TextArea {
                buffer: &self.palette_text.buffer,
                left: px,
                top: py,
                scale: 1.0,
                bounds: full_bounds,
                default_color: editor_color,
                custom_glyphs: &[],
            });
        }
        if let Some((hx, hy)) = self.hover_text_origin() {
            let panel = self
                .hover_panel_rect()
                .expect("hover_text_origin implies hover_panel_rect");
            let hover_bounds = TextBounds {
                left: panel.min_x() as i32,
                top: panel.min_y() as i32,
                right: panel.max_x() as i32,
                bottom: panel.max_y() as i32,
            };
            overlay_text_areas.push(TextArea {
                buffer: &self.hover_text.buffer,
                left: hx,
                top: hy,
                scale: 1.0,
                bounds: hover_bounds,
                default_color: editor_color,
                custom_glyphs: &[],
            });
        }
        if let Some((cx, cy)) = self.completion_text_origin() {
            let panel = self
                .completion_panel_rect()
                .expect("completion_text_origin implies completion_panel_rect");
            let bounds = TextBounds {
                left: panel.min_x() as i32,
                top: panel.min_y() as i32,
                right: panel.max_x() as i32,
                bottom: panel.max_y() as i32,
            };
            overlay_text_areas.push(TextArea {
                buffer: &self.completion_text.buffer,
                left: cx,
                top: cy,
                scale: 1.0,
                bounds,
                default_color: editor_color,
                custom_glyphs: &[],
            });
        }
        if self.find_in_files.is_some() {
            let (fx, fy) = self.find_in_files_text_origin();
            let panel = self.find_in_files_panel_rect();
            let bounds = TextBounds {
                left: panel.min_x() as i32,
                top: panel.min_y() as i32,
                right: panel.max_x() as i32,
                bottom: panel.max_y() as i32,
            };
            overlay_text_areas.push(TextArea {
                buffer: &self.find_in_files_text.buffer,
                left: fx,
                top: fy,
                scale: 1.0,
                bounds,
                default_color: editor_color,
                custom_glyphs: &[],
            });
        }

        self.text_gpu.viewport.update(&self.gpu.queue, resolution);
        self.text_gpu
            .renderer
            .prepare(
                &self.gpu.device,
                &self.gpu.queue,
                &mut self.font_system,
                &mut self.text_gpu.atlas,
                &self.text_gpu.viewport,
                text_areas,
                &mut self.swash_cache,
            )
            .expect("text prepare failed");
        self.text_gpu
            .overlay_renderer
            .prepare(
                &self.gpu.device,
                &self.gpu.queue,
                &mut self.font_system,
                &mut self.text_gpu.atlas,
                &self.text_gpu.viewport,
                overlay_text_areas,
                &mut self.swash_cache,
            )
            .expect("overlay text prepare failed");
        t.text_prepare = frame_start.elapsed();

        let Some(frame) = self.gpu.acquire() else {
            return;
        };
        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("clear + main + overlay"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: Operations {
                        load: LoadOp::Clear(clear_color(&self.theme.editor.background)),
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
            });
            // Z-order: main quads → main text → overlay quads → overlay text.
            // The two-pair structure ensures popup backgrounds occlude
            // editor text and popup text sits on top of its backdrop.
            self.quads.render(&mut pass);
            self.text_gpu
                .renderer
                .render(&self.text_gpu.atlas, &self.text_gpu.viewport, &mut pass)
                .expect("text render failed");
            self.overlay_quads.render(&mut pass);
            self.text_gpu
                .overlay_renderer
                .render(&self.text_gpu.atlas, &self.text_gpu.viewport, &mut pass)
                .expect("overlay text render failed");
        }
        self.gpu.queue.submit(Some(encoder.finish()));
        frame.present();
        self.text_gpu.atlas.trim();

        self.last_frame_us = frame_start.elapsed().as_micros();
        self.frame_count += 1;

        if let Some(start) = self.cold_start.take() {
            log::info!(
                "first frame presented in {:.1}ms (cold start budget: 100ms target / 250ms hard)",
                start.elapsed().as_secs_f32() * 1000.0
            );
        }

        if let Some(key_at) = self.pending_keystroke.take() {
            let latency_ms = key_at.elapsed().as_secs_f32() * 1000.0;
            log::info!("keystroke latency {latency_ms:.2}ms (target 16ms / hard 33ms)");
            // For frames that overrun the hard limit, dump the phase
            // breakdown so it's clear whether the time was spent in our
            // code (text materialize / reshape / scene) or waiting on
            // the OS / GPU (atlas prepare / present).
            if latency_ms > 33.0 {
                t.log(frame_start.elapsed());
            }
        }

        if self.last_report.elapsed().as_secs() >= 1 {
            let elapsed = self.last_report.elapsed().as_secs_f32();
            log::info!(
                "{:.1} fps · last frame {:.2}ms",
                self.frame_count as f32 / elapsed,
                self.last_frame_us as f32 / 1000.0,
            );
            self.frame_count = 0;
            self.last_report = Instant::now();
        }
    }
}

struct App {
    cold_start: Instant,
    initial_text: String,
    file_path: Option<PathBuf>,
    settings: Settings,
    settings_path: Option<PathBuf>,
    /// Optional `.lighteditor/settings.toml` next to the cwd. Overlaid on top
    /// of `settings_path` when both exist (workspace > user > default).
    workspace_settings_path: Option<PathBuf>,
    /// Loaded theme — sent into `State::new` once and updated via the
    /// `ThemeChanged` event on hot-reload.
    theme: Theme,
    theme_path: Option<PathBuf>,
    /// Used to schedule `AppEvent::ClearFlash` from a sleeper thread.
    proxy: EventLoopProxy<AppEvent>,
    /// Kept alive so the watcher threads don't shut down; consulted only
    /// via the user-event proxy so the fields themselves are otherwise unused.
    _user_watcher: Option<RecommendedWatcher>,
    _workspace_watcher: Option<RecommendedWatcher>,
    _theme_watcher: Option<RecommendedWatcher>,
    state: Option<State>,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    fn new(
        initial_text: String,
        file_path: Option<PathBuf>,
        settings: Settings,
        settings_path: Option<PathBuf>,
        workspace_settings_path: Option<PathBuf>,
        theme: Theme,
        theme_path: Option<PathBuf>,
        proxy: EventLoopProxy<AppEvent>,
        user_watcher: Option<RecommendedWatcher>,
        workspace_watcher: Option<RecommendedWatcher>,
        theme_watcher: Option<RecommendedWatcher>,
    ) -> Self {
        Self {
            cold_start: Instant::now(),
            initial_text,
            file_path,
            settings,
            settings_path,
            workspace_settings_path,
            theme,
            theme_path,
            proxy,
            _user_watcher: user_watcher,
            _workspace_watcher: workspace_watcher,
            _theme_watcher: theme_watcher,
            state: None,
        }
    }

    /// Re-read user + workspace settings from disk and overlay them. The
    /// user file ranks below workspace per spec §4.1.5.
    fn current_settings(&self) -> Settings {
        let mut s = match self.settings_path.as_deref() {
            Some(p) => Settings::load_or_default(p),
            None => Settings::default(),
        };
        if let Some(p) = self.workspace_settings_path.as_deref() {
            s.merge(&Settings::load_partial(p));
        }
        s
    }
}

impl ApplicationHandler<AppEvent> for App {
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: AppEvent) {
        match event {
            AppEvent::SettingsChanged => {
                let new_settings = self.current_settings();
                // macOS fsevent fires several events for one save (write
                // tmp, rename, attribute change) — bail when the merged
                // contents haven't actually changed so the log isn't N×.
                if new_settings == self.settings {
                    return;
                }
                if let Some(state) = self.state.as_mut() {
                    state.reload_settings(&new_settings);
                }
                self.settings = new_settings;
            }
            AppEvent::ThemeChanged => {
                let Some(path) = self.theme_path.as_deref() else {
                    return;
                };
                let new_theme = Theme::load_or_default(path);
                if new_theme == self.theme {
                    return;
                }
                if let Some(state) = self.state.as_mut() {
                    state.reload_theme(new_theme.clone());
                }
                self.theme = new_theme;
            }
            AppEvent::ClearFlash => {
                if let Some(state) = self.state.as_mut() {
                    state.clear_status_flash();
                }
            }
            AppEvent::CaretTick => {
                if let Some(state) = self.state.as_mut() {
                    state.tick_caret();
                }
            }
            AppEvent::LspPoll => {
                if let Some(state) = self.state.as_mut() {
                    state.poll_lsp();
                }
            }
            AppEvent::TerminalWakeup => {
                if let Some(state) = self.state.as_mut() {
                    state.scene_dirty = true;
                    state.window.request_redraw();
                }
            }
            AppEvent::FlutterDevicesRefreshed(devices) => {
                if let Some(state) = self.state.as_mut() {
                    state.flutter_devices = devices;
                    // Only re-shape if the palette is actively showing
                    // — opening it always rebuilds from current state,
                    // so a closed palette just picks up the new list
                    // on the next open.
                    if state.palette.is_some() {
                        state.scene_dirty = true;
                        state.window.request_redraw();
                    }
                }
            }
            AppEvent::FileTreeChanged => {
                if let Some(state) = self.state.as_mut() {
                    state.file_tree.reload_preserving_expansion();
                    // Filesystem activity is also when git status moves
                    // (saving the active file, a git checkout in the
                    // terminal pane, etc.), so re-run the workspace
                    // status query on the same debounce that drove the
                    // tree reload.
                    state.refresh_workspace_git_status();
                    // package.json's "scripts" map is also workspace
                    // state that moves with filesystem events. The
                    // read is cheap (one file + serde parse), so we
                    // pay it on every wakeup rather than thread a
                    // package.json-specific watcher.
                    state.refresh_npm_scripts();
                    state.refresh_flutter_project();
                    // Only re-shape the row labels if the sidebar is
                    // visible — invisible reloads silently keep the
                    // tree warm so it pops up fresh on next Cmd-B.
                    if state.file_tree.visible {
                        state.refresh_file_tree_text();
                        state.scene_dirty = true;
                        state.window.request_redraw();
                    }
                }
            }
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title(window_title(self.file_path.as_deref()))
            .with_inner_size(PhysicalSize::new(WINDOW_WIDTH, WINDOW_HEIGHT));
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("failed to create window"),
        );
        let state = State::new(
            window,
            self.cold_start,
            &self.initial_text,
            self.file_path.clone(),
            &self.settings,
            self.theme.clone(),
            self.proxy.clone(),
        );
        state.window.request_redraw();
        self.state = Some(state);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested if state.confirm_close_all() => {
                log::info!("close requested — exiting");
                event_loop.exit();
            }
            WindowEvent::Resized(size) => state.resize(size),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                state.apply_scale(scale_factor as f32);
                state.resize(state.window.inner_size());
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                state.modifiers = modifiers.state();
            }
            WindowEvent::KeyboardInput { event, .. } => state.handle_key(event),
            WindowEvent::CursorMoved { position, .. } => {
                state.handle_mouse_move(position.x as f32, position.y as f32);
            }
            WindowEvent::MouseInput {
                state: button_state,
                button: MouseButton::Left,
                ..
            } => match button_state {
                ElementState::Pressed => state.handle_mouse_press(),
                ElementState::Released => state.handle_mouse_release(),
            },
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Middle,
                ..
            } => state.handle_mouse_middle(),
            WindowEvent::MouseWheel { delta, .. } => {
                let delta_y = match delta {
                    MouseScrollDelta::LineDelta(_, lines) => lines * state.line_height(),
                    MouseScrollDelta::PixelDelta(pos) => pos.y as f32,
                };
                state.handle_scroll(delta_y);
            }
            WindowEvent::DroppedFile(path) => state.handle_dropped_file(path),
            WindowEvent::RedrawRequested => {
                state.render();
            }
            _ => {}
        }
    }
}

fn main() {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,wgpu_core=warn,wgpu_hal=warn,naga=warn"),
    )
    .init();

    let (initial_text, file_path) = match std::env::args().nth(1) {
        Some(arg) => {
            let path = PathBuf::from(arg);
            // Directory argument → treat as "open this folder as the
            // workspace". `chdir` so the existing CWD-based workspace
            // root logic picks it up, then start with no file open.
            // Matches `code <dir>` / `cursor <dir>` muscle memory.
            if path.is_dir() {
                let canon = std::fs::canonicalize(&path).unwrap_or(path.clone());
                if let Err(e) = std::env::set_current_dir(&canon) {
                    log::warn!("could not chdir into workspace {}: {}", canon.display(), e);
                } else {
                    log::info!("workspace root: {}", canon.display());
                }
                (WELCOME_TEXT.to_string(), None)
            } else {
                match std::fs::read_to_string(&path) {
                    // Canonicalize so the rest of the app (LSP,
                    // file-watcher, tab labels) deals with an absolute
                    // path. file:// URLs require absolute, and
                    // rust-analyzer hangs its workspace lookup on the
                    // URI.
                    Ok(content) => (content, Some(std::fs::canonicalize(&path).unwrap_or(path))),
                    Err(e) => {
                        log::error!("could not read {}: {}", path.display(), e);
                        (WELCOME_TEXT.to_string(), None)
                    }
                }
            }
        }
        None => (WELCOME_TEXT.to_string(), None),
    };

    let settings_path = dirs::config_dir().map(|d| d.join(CONFIG_SUBDIR).join(CONFIG_FILENAME));
    let workspace_settings_path = std::env::current_dir()
        .ok()
        .map(|d| d.join(WORKSPACE_CONFIG_SUBDIR).join(CONFIG_FILENAME));
    let theme_path = dirs::config_dir().map(|d| d.join(CONFIG_SUBDIR).join(THEME_FILENAME));

    // Default → User → Workspace per spec §4.1.5.
    let mut settings = match settings_path.as_deref() {
        Some(p) => Settings::load_or_default(p),
        None => {
            log::warn!("no XDG config dir; using default settings");
            Settings::default()
        }
    };
    if let Some(p) = workspace_settings_path.as_deref() {
        settings.merge(&Settings::load_partial(p));
    }

    let theme = match theme_path.as_deref() {
        Some(p) => Theme::load_or_default(p),
        None => Theme::default(),
    };

    let event_loop = EventLoop::<AppEvent>::with_user_event()
        .build()
        .expect("failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    // Watch both settings.toml paths so font_size / line_height / tab_size
    // hot-reload without a restart. Either watcher firing triggers a full
    // re-merge, so the precedence rules stay consistent.
    let user_watcher = settings_path
        .as_deref()
        .and_then(|p| spawn_settings_watcher(p, event_loop.create_proxy()));
    let workspace_watcher = workspace_settings_path
        .as_deref()
        .and_then(|p| spawn_settings_watcher(p, event_loop.create_proxy()));
    let theme_watcher = theme_path
        .as_deref()
        .and_then(|p| spawn_theme_watcher(p, event_loop.create_proxy()));
    // Caret-blink heartbeat — one detached thread for the app lifetime.
    spawn_caret_blink_thread(event_loop.create_proxy());
    spawn_lsp_poll_thread(event_loop.create_proxy());

    let mut app = App::new(
        initial_text,
        file_path,
        settings,
        settings_path,
        workspace_settings_path,
        theme,
        theme_path,
        event_loop.create_proxy(),
        user_watcher,
        workspace_watcher,
        theme_watcher,
    );
    event_loop.run_app(&mut app).expect("event loop failed");
}

/// Spawn a recursive watcher over the file-tree's workspace root.
/// Filesystem events get filtered: anything inside a hidden dir
/// (`.git`, `node_modules`, `target`, …) is dropped at the watcher
/// callback so a `git checkout` or `npm install` doesn't drown the
/// app in events. Surviving events feed a debounce thread that
/// coalesces bursts into one `AppEvent::FileTreeChanged` per ~200 ms
/// quiet period — saving a file triggers exactly one reload.
fn spawn_file_tree_watcher(
    root: &Path,
    hidden_dirs: Vec<String>,
    proxy: EventLoopProxy<AppEvent>,
) -> Option<RecommendedWatcher> {
    if !root.exists() {
        return None;
    }

    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        // Forward only if at least one affected path is *outside* the
        // ignored set. Routine git / build / dependency operations
        // touch dozens of paths a second under those dirs and never
        // change anything the sidebar shows.
        let interesting = event
            .paths
            .iter()
            .any(|p| !path_under_hidden_dir(p, &hidden_dirs));
        if interesting {
            // The receiver thread closes on the event-loop dropping;
            // a `send` failure is the cue to stop forwarding.
            let _ = tx.send(());
        }
    })
    .map_err(|e| log::warn!("file-tree watcher init failed: {e}"))
    .ok()?;

    watcher
        .watch(root, RecursiveMode::Recursive)
        .map_err(|e| log::warn!("file-tree watcher attach failed: {e}"))
        .ok()?;

    // Debounce thread: block on the channel, then sleep through a
    // quiet window before firing. `try_recv` drains anything that
    // arrived during the sleep so a burst becomes one reload.
    let proxy_for_debounce = proxy;
    std::thread::spawn(move || loop {
        if rx.recv().is_err() {
            return;
        }
        std::thread::sleep(FILE_TREE_DEBOUNCE);
        while rx.try_recv().is_ok() {}
        if proxy_for_debounce
            .send_event(AppEvent::FileTreeChanged)
            .is_err()
        {
            return;
        }
    });

    log::info!("watching {} for file-tree changes", root.display());
    Some(watcher)
}

/// `true` if any path component is a hidden directory the file tree
/// filters out. `hidden_dirs` is the same list `FileTree` carries,
/// so the watcher's drop-set always tracks the user's setting.
fn path_under_hidden_dir(path: &Path, hidden_dirs: &[String]) -> bool {
    path.components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .any(|name| hidden_dirs.iter().any(|d| d == name))
}

/// Same shape as `spawn_settings_watcher`, but emits `ThemeChanged`.
fn spawn_theme_watcher(
    theme_path: &Path,
    proxy: EventLoopProxy<AppEvent>,
) -> Option<RecommendedWatcher> {
    let parent = theme_path.parent()?;
    if !parent.exists() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            log::warn!("could not create theme dir {}: {}", parent.display(), e);
            return None;
        }
    }
    let target = theme_path.file_name()?.to_os_string();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        if event
            .paths
            .iter()
            .any(|p| p.file_name().is_some_and(|n| n == target))
        {
            let _ = proxy.send_event(AppEvent::ThemeChanged);
        }
    })
    .map_err(|e| log::warn!("theme watcher init failed: {e}"))
    .ok()?;
    watcher
        .watch(parent, RecursiveMode::NonRecursive)
        .map_err(|e| log::warn!("theme watcher attach failed: {e}"))
        .ok()?;
    log::info!("watching {} for theme changes", parent.display());
    Some(watcher)
}

/// Detach a background thread that fires `AppEvent::CaretTick` every
/// `CARET_BLINK_INTERVAL`. The thread exits cleanly when the event loop
/// goes away (subsequent `send_event` calls return Err).
/// Detach a background thread that fires `AppEvent::LspPoll` every
/// `LSP_POLL_INTERVAL`. The handler is a no-op when no servers are
/// spawned — the cost when LSP is unused is the timer wakeup.
fn spawn_lsp_poll_thread(proxy: EventLoopProxy<AppEvent>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(LSP_POLL_INTERVAL);
        if proxy.send_event(AppEvent::LspPoll).is_err() {
            return;
        }
    });
}

fn spawn_caret_blink_thread(proxy: EventLoopProxy<AppEvent>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(CARET_BLINK_INTERVAL);
        if proxy.send_event(AppEvent::CaretTick).is_err() {
            return;
        }
    });
}

/// Spawn a file watcher that fires `AppEvent::SettingsChanged` on the
/// event-loop proxy every time `settings_path`'s filename gets touched.
/// Returns `None` if anything along the way fails — the editor still runs
/// with the loaded settings, just without hot-reload.
fn spawn_settings_watcher(
    settings_path: &Path,
    proxy: EventLoopProxy<AppEvent>,
) -> Option<RecommendedWatcher> {
    let parent = settings_path.parent()?;
    if !parent.exists() {
        // Create the dir so the watcher has something to attach to — the
        // user might write settings.toml *after* launch.
        if let Err(e) = std::fs::create_dir_all(parent) {
            log::warn!("could not create settings dir {}: {}", parent.display(), e);
            return None;
        }
    }
    let target = settings_path.file_name()?.to_os_string();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        if event
            .paths
            .iter()
            .any(|p| p.file_name().is_some_and(|n| n == target))
        {
            // Loop has gone away if this errors; the watcher is about to be
            // dropped anyway.
            let _ = proxy.send_event(AppEvent::SettingsChanged);
        }
    })
    .map_err(|e| log::warn!("settings watcher init failed: {e}"))
    .ok()?;
    watcher
        .watch(parent, RecursiveMode::NonRecursive)
        .map_err(|e| log::warn!("settings watcher attach failed: {e}"))
        .ok()?;
    log::info!("watching {} for settings changes", parent.display());
    Some(watcher)
}
