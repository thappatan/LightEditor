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
mod find;
mod palette;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use document::Document;
use editor_config::Settings;
use editor_core::{LineEnding, Position, Selection};
use editor_ui_render::{GpuContext, QuadRenderer};
use editor_ui_scene::{Color as SceneColor, Point, Rect, Scene, SceneNode};
use editor_ui_text::glyphon::{Color, FontSystem, Resolution, SwashCache, TextArea, TextBounds};
use editor_ui_text::{TextGpu, TextStack};
use find::{FindBar, FindFocus};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use palette::{Command, CommandPalette};
use wgpu::{
    LoadOp, Operations, RenderPassColorAttachment, RenderPassDescriptor, StoreOp,
    TextureViewDescriptor,
};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

/// Cross-thread events posted into the winit event loop from background
/// helpers — file watcher + flash-clear timer.
#[derive(Debug, Clone, Copy)]
enum AppEvent {
    /// `settings.toml` (user or workspace) changed on disk; reload and reapply.
    SettingsChanged,
    /// `FLASH_DURATION` has elapsed since a transient status-bar message was
    /// set — clear it.
    ClearFlash,
}

/// How long a "settings reloaded" flash stays on the status bar.
const FLASH_DURATION: Duration = Duration::from_millis(2000);

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

/// Find-bar overlay dimensions (single-row), in logical pixels.
const FIND_WIDTH_DIP: f32 = 480.0;
const FIND_TOP_DIP: f32 = 16.0;
const FIND_PAD_DIP: f32 = 8.0;

/// Subdirectory under the user's XDG config dir that holds settings.toml.
const CONFIG_SUBDIR: &str = "lighteditor";
const CONFIG_FILENAME: &str = "settings.toml";
/// Subdirectory under the current working directory that may hold a
/// workspace-scoped settings override (spec §4.1.5 — Workspace ranks above
/// User in the precedence Default → User → Workspace).
const WORKSPACE_CONFIG_SUBDIR: &str = ".lighteditor";

/// Whether the platform's "primary" modifier (Cmd on macOS, Ctrl on
/// Linux/Windows) is held. Used to gate shortcuts like Cmd-S.
fn is_cmd_or_ctrl(mods: ModifiersState) -> bool {
    mods.super_key() || mods.control_key()
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

/// Short label suitable for a status-bar flash — just the filename (with
/// extension), falling back to the full path's stringified form for weird
/// edge cases where there is no terminal component.
fn filename_for_flash(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
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
    quads: QuadRenderer,
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

    /// The scene rebuilt from `editor` whenever it changes.
    scene: Scene,

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
    /// Used by `set_status_flash` to schedule its own `ClearFlash` event
    /// from a detached sleeper thread. Clone-cheap.
    flash_proxy: EventLoopProxy<AppEvent>,

    /// When the last unhandled key press happened, for keystroke-latency timing.
    pending_keystroke: Option<Instant>,
    /// Rolling 1-second frame-time window (spec §8).
    frame_count: u64,
    last_report: Instant,
    last_frame_us: u128,
    cold_start: Option<Instant>,
}

impl State {
    fn new(
        window: Arc<Window>,
        cold_start: Instant,
        initial_text: &str,
        file_path: Option<PathBuf>,
        settings: &Settings,
        flash_proxy: EventLoopProxy<AppEvent>,
    ) -> Self {
        let scale = window.scale_factor() as f32;
        let size = window.inner_size();
        let gpu = GpuContext::new(window.clone());
        let quads = QuadRenderer::new(&gpu.device, gpu.format());

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

        let palette_width = (PALETTE_WIDTH_DIP - 2.0 * PALETTE_PAD_DIP) * scale;
        let palette_text = TextStack::new(
            &mut font_system,
            palette_width,
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

        let mut state = Self {
            window,
            gpu,
            quads,
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
            flash_proxy,
            tab_spaces,
            pending_keystroke: None,
            frame_count: 0,
            last_report: Instant::now(),
            last_frame_us: 0,
            cold_start: Some(cold_start),
        };
        state.refresh_tabs_text();
        state.rebuild_scene();
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
        // Palette / find widths are fixed; nothing to update on a window resize.
        self.text_dirty = true;
        self.scene_dirty = true;
        // ControlFlow::Wait won't redraw on its own — a resize must ask.
        self.window.request_redraw();
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
        self.palette_text
            .set_width(fs, (PALETTE_WIDTH_DIP - 2.0 * PALETTE_PAD_DIP) * scale);
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

        // Cmd-Shift-P toggles the palette regardless of whether it is open,
        // so it stays a single muscle-memory key combo.
        if is_cmd_or_ctrl(self.modifiers) && self.modifiers.shift_key() {
            if let Key::Character(c) = &event.logical_key {
                if c.as_str().eq_ignore_ascii_case("p") {
                    if self.palette.is_some() {
                        self.close_palette();
                    } else {
                        self.open_palette();
                    }
                    return;
                }
            }
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
            if let Key::Character(c) = &event.logical_key {
                let lower = c.to_lowercase();
                let alt = self.modifiers.alt_key();
                match lower.as_str() {
                    "f" => {
                        if self.doc().find.is_some() {
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
                        self.collapse_selection_to_primary();
                        return;
                    }
                    _ => {}
                }
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

        let mut text_changed = true;
        let handled = match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.collapse_selection_to_primary();
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
                self.doc_mut().editor.insert(" ");
                true
            }
            Key::Named(NamedKey::Tab) => {
                let spaces = self.tab_spaces.clone();
                self.doc_mut().editor.insert(&spaces);
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
            // Printable character input — winit gives us the resolved text.
            _ => match &event.text {
                Some(text) if !text.is_empty() => {
                    if let Some(pair) = auto_pair(text) {
                        self.doc_mut().editor.insert(pair);
                        // Caret sits one char back so the user is *inside*
                        // the pair, ready to type the wrapped content.
                        self.doc_mut().editor.move_left(false);
                    } else {
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
        if let Some(idx) = self.tab_close_at_pixel(mx, my) {
            self.close_tab_at(idx);
            return;
        }
        if let Some(idx) = self.tab_at_pixel(mx, my) {
            self.switch_tab(idx);
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
        x >= 0.0 && x < self.text_inset_x && y >= self.text_inset_y && y < bottom
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
    }

    /// Write the active document to its path, or prompt for one with a Save As
    /// dialog when there is none.
    fn save_to_file(&mut self) {
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
                {
                    let d = self.doc_mut();
                    d.file_path = Some(path);
                    d.dirty = false;
                }
                self.update_title();
                self.refresh_tabs_text();
                self.set_status_flash(format!("saved · {label}"));
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
        match std::fs::read_to_string(&path) {
            Ok(content) => {
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

        let flash = self
            .status_flash
            .as_ref()
            .filter(|(_, t)| t.elapsed() < FLASH_DURATION)
            .map(|(s, _)| s.clone());
        let left = flash
            .unwrap_or_else(|| format!("{label}  ·  {language}  ·  Spaces: {indent}  ·  {le}"));
        let right = format!(
            "Ln {}, Col {}  ·  {} lines",
            pos.line + 1,
            pos.column + 1,
            lines
        );

        self.status_left.set_content(&mut self.font_system, &left);
        self.status_right.set_content(&mut self.font_system, &right);
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

    /// Open the command palette, populated with every registered command.
    fn open_palette(&mut self) {
        self.palette = Some(CommandPalette::new());
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
        for cmd in palette.visible() {
            text.push_str("  ");
            text.push_str(cmd.label());
            text.push('\n');
        }
        self.palette_text.set_content(&mut self.font_system, &text);
    }

    /// Route a key while the palette is open.
    fn handle_palette_key(&mut self, event: KeyEvent) {
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => self.close_palette(),
            Key::Named(NamedKey::ArrowUp) => {
                if let Some(p) = self.palette.as_mut() {
                    p.prev();
                }
                self.refresh_palette_text();
                self.scene_dirty = true;
                self.window.request_redraw();
            }
            Key::Named(NamedKey::ArrowDown) => {
                if let Some(p) = self.palette.as_mut() {
                    p.next();
                }
                self.refresh_palette_text();
                self.scene_dirty = true;
                self.window.request_redraw();
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(cmd) = self.palette.as_ref().and_then(|p| p.selected()) {
                    self.execute_command(cmd);
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
    fn execute_command(&mut self, cmd: Command) {
        self.close_palette();
        match cmd {
            Command::NewFile => self.new_file(),
            Command::OpenFile => self.open_file_dialog(),
            Command::SaveFile => self.save_to_file(),
            Command::SaveFileAs => self.save_as(),
            Command::SaveAll => self.save_all(),
            Command::CloseOtherTabs => self.close_other_tabs(),
            Command::CloseAllTabs => self.close_all_tabs(),
        }
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
            .map(|p| p.visible_count())
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
        if palette.visible_count() == 0 {
            return None;
        }
        let lh = self.line_height();
        let (_ox, oy) = self.palette_text_origin();
        let row_y = oy + (2 + palette.selected_row()) as f32 * lh;
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
        // Cmd-Alt-C / Cmd-Alt-W toggle match-case / whole-word — checked
        // before the regular character branch so the chord doesn't insert
        // 'c' or 'w' into the query.
        if is_cmd_or_ctrl(self.modifiers) && self.modifiers.alt_key() {
            if let Key::Character(c) = &event.logical_key {
                let buffer_text = self.doc().editor.text();
                match c.to_lowercase().as_str() {
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

    /// Mouse wheel: scroll the active document vertically.
    fn handle_scroll(&mut self, delta_y: f32) {
        let max = self.max_scroll();
        let current = self.doc().scroll_y;
        let new = (current - delta_y).clamp(0.0, max);
        if new != current {
            self.doc_mut().scroll_y = new;
            self.scene_dirty = true;
            self.window.request_redraw();
        }
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
    /// minus the tab strip on top and the status bar on the bottom.
    fn visible_height(&self) -> f32 {
        self.gpu.surface_config.height as f32
            - self.text_inset_y
            - STATUS_BAR_HEIGHT_DIP * self.scale
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

        // Tab strip backdrop — sits behind every tab slot.
        root.push_child(SceneNode::quad(
            self.tab_strip_rect(),
            SceneColor::rgba(22, 22, 28, 255),
        ));
        for (i, _) in self.docs.iter().enumerate() {
            let slot = self.tab_slot_rect(i);
            let bg = if i == self.active {
                SceneColor::rgba(48, 48, 60, 255)
            } else {
                SceneColor::rgba(30, 30, 38, 255)
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
                root.push_child(SceneNode::quad(sep, SceneColor::rgba(60, 60, 70, 255)));
            }
        }

        // Gutter backdrop — a slim column on the left, slightly darker than
        // the editor surface so the line numbers read as belonging to a
        // chrome region rather than the buffer.
        let gutter_h = (h - self.text_inset_y - STATUS_BAR_HEIGHT_DIP * self.scale).max(0.0);
        root.push_child(SceneNode::quad(
            Rect::new(0.0, self.text_inset_y, self.text_inset_x, gutter_h),
            SceneColor::rgba(14, 14, 20, 255),
        ));

        // Active line backdrop — a faint full-width row at every visual run
        // of the logical line where the primary caret sits. Behind the
        // selection so the selection's brighter blue still reads.
        let active_logical = {
            let head = self.doc().editor.selections().primary().head;
            self.doc().editor.buffer().char_to_position(head).line
        };
        let scroll = self.doc().scroll_y;
        let line_h = self.line_height();
        let active_color = SceneColor::rgba(255, 255, 255, 12);
        for run in self.text.buffer.layout_runs() {
            if run.line_i != active_logical {
                continue;
            }
            let y = self.text_inset_y + run.line_top - scroll;
            root.push_child(SceneNode::quad(Rect::new(0.0, y, w, line_h), active_color));
        }

        // Selection highlights sit behind text and carets.
        for selection in self.doc().editor.selections().iter() {
            for rect in self.selection_rects(selection) {
                root.push_child(SceneNode::quad(rect, SceneColor::rgba(120, 160, 255, 64)));
            }
        }

        // Carets on top of the highlights.
        for selection in self.doc().editor.selections().iter() {
            if let Some((cx, cy)) = self.caret_pixel(selection.head) {
                root.push_child(SceneNode::quad(
                    Rect::new(
                        self.text_inset_x + cx,
                        self.text_inset_y + cy - scroll,
                        self.caret_width,
                        line_h,
                    ),
                    SceneColor::rgb(120, 160, 255),
                ));
            }
        }

        // Find-bar match highlights.
        for rect in self.match_highlight_rects() {
            root.push_child(SceneNode::quad(rect, SceneColor::rgba(255, 200, 60, 64)));
        }

        // Status bar backdrop — opaque so it covers any text that scrolled
        // behind it (text bounds also clip, but defence in depth is cheap).
        root.push_child(SceneNode::quad(
            self.status_bar_rect(),
            SceneColor::rgba(22, 22, 28, 255),
        ));

        // Find bar panel.
        if self.doc().find.is_some() {
            root.push_child(SceneNode::quad(
                self.find_panel_rect(),
                SceneColor::rgba(38, 38, 48, 240),
            ));
        }

        // Palette overlay on top of everything else.
        if self.palette.is_some() {
            root.push_child(SceneNode::quad(
                Rect::new(0.0, 0.0, w, h),
                SceneColor::rgba(0, 0, 0, 96),
            ));
            root.push_child(SceneNode::quad(
                self.palette_panel_rect(),
                SceneColor::rgba(38, 38, 48, 240),
            ));
            if let Some(highlight) = self.palette_selection_rect() {
                root.push_child(SceneNode::quad(
                    highlight,
                    SceneColor::rgba(120, 160, 255, 80),
                ));
            }
        }

        self.scene = Scene::new(root);
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

        if self.text_dirty {
            let new_text = self.docs[self.active].editor.text();
            let shaped = if self.visible_whitespace {
                substitute_whitespace(&new_text)
            } else {
                new_text
            };
            self.text.set_content(&mut self.font_system, &shaped);
            self.text_dirty = false;
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

        self.quads.prepare(
            &self.gpu.device,
            &self.gpu.queue,
            &self.scene,
            self.gpu.surface_config.width as f32,
            self.gpu.surface_config.height as f32,
        );

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
        let status_bar_h = STATUS_BAR_HEIGHT_DIP * self.scale;
        let editor_text_bounds = TextBounds {
            left: 0,
            top: self.text_inset_y as i32,
            right: surface_w as i32,
            bottom: (surface_h as f32 - status_bar_h) as i32,
        };

        // All text — editor / tabs / close ×s / status / find / palette —
        // batched into a single `prepare` + `render`. The order TextAreas
        // appear in the vec is the draw order, so overlays go last.
        let inset_x = self.text_inset_x;
        let inset_y = self.text_inset_y;
        let scroll = self.docs[self.active].scroll_y;
        let editor_color = Color::rgb(238, 238, 238);
        let label_color = Color::rgb(220, 220, 220);
        let dim_color = Color::rgb(180, 180, 190);
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
        let gutter_left = GUTTER_PAD_LEFT_DIP * self.scale;
        let line_height = self.line_height();
        let viewport_top = self.text_inset_y;
        let viewport_bottom = surface_h as f32 - status_bar_h;

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
                left: 0,
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
                default_color: dim_color,
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
                default_color: editor_color,
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
        text_areas.push(TextArea {
            buffer: &self.status_left.buffer,
            left: status_left_x,
            top: status_y,
            scale: 1.0,
            bounds: status_bounds,
            default_color: dim_color,
            custom_glyphs: &[],
        });
        text_areas.push(TextArea {
            buffer: &self.status_right.buffer,
            left: status_right_x,
            top: status_y,
            scale: 1.0,
            bounds: status_bounds,
            default_color: dim_color,
            custom_glyphs: &[],
        });
        if let Some((fx, fy)) = find_xy {
            text_areas.push(TextArea {
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
            text_areas.push(TextArea {
                buffer: &self.palette_text.buffer,
                left: px,
                top: py,
                scale: 1.0,
                bounds: full_bounds,
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

        let Some(frame) = self.gpu.acquire() else {
            return;
        };
        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("clear + carets + text"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: Operations {
                        load: LoadOp::Clear(wgpu::Color {
                            r: 0.02,
                            g: 0.02,
                            b: 0.04,
                            a: 1.0,
                        }),
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
            });
            // Editor quads first (tab strip backdrops + selection + carets +
            // overlay panels); then every text region in one render call.
            self.quads.render(&mut pass);
            self.text_gpu
                .renderer
                .render(&self.text_gpu.atlas, &self.text_gpu.viewport, &mut pass)
                .expect("text render failed");
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
            log::info!(
                "keystroke latency {:.2}ms (target 16ms / hard 33ms)",
                key_at.elapsed().as_secs_f32() * 1000.0
            );
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
    /// Used to schedule `AppEvent::ClearFlash` from a sleeper thread.
    proxy: EventLoopProxy<AppEvent>,
    /// Kept alive so the watcher threads don't shut down; consulted only
    /// via the user-event proxy so the fields themselves are otherwise unused.
    _user_watcher: Option<RecommendedWatcher>,
    _workspace_watcher: Option<RecommendedWatcher>,
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
        proxy: EventLoopProxy<AppEvent>,
        user_watcher: Option<RecommendedWatcher>,
        workspace_watcher: Option<RecommendedWatcher>,
    ) -> Self {
        Self {
            cold_start: Instant::now(),
            initial_text,
            file_path,
            settings,
            settings_path,
            workspace_settings_path,
            proxy,
            _user_watcher: user_watcher,
            _workspace_watcher: workspace_watcher,
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
            AppEvent::ClearFlash => {
                if let Some(state) = self.state.as_mut() {
                    state.clear_status_flash();
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
            match std::fs::read_to_string(&path) {
                Ok(content) => (content, Some(path)),
                Err(e) => {
                    log::error!("could not read {}: {}", path.display(), e);
                    (WELCOME_TEXT.to_string(), None)
                }
            }
        }
        None => (WELCOME_TEXT.to_string(), None),
    };

    let settings_path = dirs::config_dir().map(|d| d.join(CONFIG_SUBDIR).join(CONFIG_FILENAME));
    let workspace_settings_path = std::env::current_dir()
        .ok()
        .map(|d| d.join(WORKSPACE_CONFIG_SUBDIR).join(CONFIG_FILENAME));

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

    let mut app = App::new(
        initial_text,
        file_path,
        settings,
        settings_path,
        workspace_settings_path,
        event_loop.create_proxy(),
        user_watcher,
        workspace_watcher,
    );
    event_loop.run_app(&mut app).expect("event loop failed");
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
