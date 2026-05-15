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
use std::time::Instant;

use document::Document;
use editor_config::Settings;
use editor_core::{LineEnding, Position, Selection};
use editor_ui_render::{GpuContext, QuadRenderer};
use editor_ui_scene::{Color as SceneColor, Point, Rect, Scene, SceneNode};
use editor_ui_text::glyphon::{Color, FontSystem, Resolution, SwashCache, TextArea, TextBounds};
use editor_ui_text::TextStack;
use find::FindBar;
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
/// helpers — currently just the settings file watcher.
#[derive(Debug, Clone, Copy)]
enum AppEvent {
    /// `settings.toml` changed on disk; reload and reapply.
    SettingsChanged,
}

/// Initial window size, in logical pixels.
const WINDOW_WIDTH: u32 = 1280;
const WINDOW_HEIGHT: u32 = 720;

/// Horizontal inset of the text block from the window's left edge, in logical
/// pixels. Multiplied by the window scale factor to get physical pixels.
const TEXT_INSET_DIP: f32 = 28.0;
/// Total horizontal padding (inset on both sides), in logical pixels.
const TEXT_PADDING_DIP: f32 = 2.0 * TEXT_INSET_DIP;
/// Gap between the bottom of the tab strip and the top of the editor text,
/// in logical pixels.
const TEXT_TOP_GAP_DIP: f32 = 8.0;
/// Caret width, in logical pixels.
const CARET_WIDTH_DIP: f32 = 2.0;

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
    /// Shared across every `TextStack` (editor, palette, find, tabs, close).
    /// `FontSystem::new()` walks the OS font directory once — ~80ms cold —
    /// so building one and lending it out is much cheaper than five.
    font_system: FontSystem,
    /// Shared swash glyph cache. Per-stack `TextRenderer::prepare` borrows it
    /// in turn (calls are sequential, so the single `&mut` rotates fine).
    swash_cache: SwashCache,
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

    /// The scene rebuilt from `editor` whenever it changes.
    scene: Scene,

    /// Window scale factor; physical = logical * scale.
    scale: f32,
    /// `TEXT_INSET_DIP × scale` — left/right inset of the text block.
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

        let text_padding = TEXT_PADDING_DIP * scale;
        let text = TextStack::new(
            &gpu.device,
            &gpu.queue,
            gpu.format(),
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

        // Each overlay TextStack reuses the shared FontSystem above.
        let palette_width = (PALETTE_WIDTH_DIP - 2.0 * PALETTE_PAD_DIP) * scale;
        let palette_text = TextStack::new(
            &gpu.device,
            &gpu.queue,
            gpu.format(),
            &mut font_system,
            palette_width,
            font_size_pt,
            line_height_pt,
            scale,
            "",
        );

        let find_width = (FIND_WIDTH_DIP - 2.0 * FIND_PAD_DIP) * scale;
        let find_text = TextStack::new(
            &gpu.device,
            &gpu.queue,
            gpu.format(),
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
            &gpu.device,
            &gpu.queue,
            gpu.format(),
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
            &gpu.device,
            &gpu.queue,
            gpu.format(),
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
            &gpu.device,
            &gpu.queue,
            gpu.format(),
            &mut font_system,
            status_width,
            font_size_pt,
            line_height_pt,
            scale,
            "",
        );
        let status_right = TextStack::new(
            &gpu.device,
            &gpu.queue,
            gpu.format(),
            &mut font_system,
            status_width,
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
            text,
            docs: vec![doc],
            active: 0,
            tabs_text,
            close_text,
            status_left,
            status_right,
            scene,
            scale,
            text_inset_x: TEXT_INSET_DIP * scale,
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
        self.text_inset_x = TEXT_INSET_DIP * scale;
        self.text_inset_y = (TAB_BAR_HEIGHT_DIP + TEXT_TOP_GAP_DIP) * scale;
        self.text_padding = TEXT_PADDING_DIP * scale;
        self.caret_width = CARET_WIDTH_DIP * scale;
        let fs = &mut self.font_system;
        self.text.set_scale(fs, scale);
        self.palette_text.set_scale(fs, scale);
        self.palette_text
            .set_width(fs, (PALETTE_WIDTH_DIP - 2.0 * PALETTE_PAD_DIP) * scale);
        self.find_text.set_scale(fs, scale);
        self.find_text
            .set_width(fs, (FIND_WIDTH_DIP - 2.0 * FIND_PAD_DIP) * scale);
        self.tabs_text.set_scale(fs, scale);
        self.tabs_text
            .set_width(fs, self.gpu.surface_config.width as f32);
        self.close_text.set_scale(fs, scale);
        self.close_text.set_width(fs, TAB_CLOSE_W_DIP * scale);
        let status_width = self.gpu.surface_config.width as f32 - 2.0 * STATUS_PAD_X_DIP * scale;
        self.status_left.set_scale(fs, scale);
        self.status_left.set_width(fs, status_width);
        self.status_right.set_scale(fs, scale);
        self.status_right.set_width(fs, status_width);
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
        self.tab_spaces = " ".repeat(settings.editor.tab_size);
        self.text_dirty = true;
        self.scene_dirty = true;
        log::info!(
            "settings reloaded: font_size={font} line_height={lh} tab_size={}",
            settings.editor.tab_size
        );
        self.window.request_redraw();
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
            if let Key::Character(c) = &event.logical_key {
                let lower = c.to_lowercase();
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
                        if self.modifiers.alt_key() {
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
                    "w" => {
                        self.close_active_tab();
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

        let mut text_changed = true;
        let handled = match &event.logical_key {
            Key::Named(NamedKey::Backspace) => {
                self.doc_mut().editor.backspace();
                true
            }
            Key::Named(NamedKey::Delete) => {
                self.doc_mut().editor.delete_forward();
                true
            }
            Key::Named(NamedKey::Enter) => {
                self.doc_mut().editor.insert_newline();
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
                self.doc_mut().editor.move_left(shift);
                text_changed = false;
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.doc_mut().editor.move_right(shift);
                text_changed = false;
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.doc_mut().editor.move_up(shift);
                text_changed = false;
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.doc_mut().editor.move_down(shift);
                text_changed = false;
                true
            }
            // Printable character input — winit gives us the resolved text.
            _ => match &event.text {
                Some(text) if !text.is_empty() => {
                    self.doc_mut().editor.insert(text);
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
    /// when the press lands on its "×"); below the strip it places a caret
    /// and starts a potential drag.
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
        let Some(char_idx) = self.char_at_pixel(mx, my) else {
            return;
        };
        self.drag_anchor = Some(char_idx);
        self.doc_mut()
            .editor
            .set_selection(Selection::cursor(char_idx));
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
                {
                    let d = self.doc_mut();
                    d.file_path = Some(path);
                    d.dirty = false;
                }
                self.update_title();
                self.refresh_tabs_text();
                self.scene_dirty = true;
                self.window.request_redraw();
            }
            Err(e) => log::error!("save failed for {}: {}", path.display(), e),
        }
    }

    /// Read `path` and place it in the editor. If the active document is a
    /// pristine, never-saved, never-edited scratch buffer, replace it in-place
    /// (matches VSCode's behaviour for untouched "Untitled-1"); otherwise
    /// push a new tab and activate it. I/O failure is logged and the state
    /// is left intact.
    fn open_path(&mut self, path: PathBuf) {
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
                self.window.request_redraw();
            }
            Err(e) => log::error!("could not read {}: {}", path.display(), e),
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
        // Keep the new tab's stored scroll position rather than recentering.
        self.follow_caret = false;
        self.update_title();
        self.refresh_tabs_text();
        self.refresh_find_text();
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

    /// Re-shape both halves of the status bar.
    ///
    /// Left half: `path  ·  language  ·  LE`.
    /// Right half: `Ln L, Col C  ·  N lines`.
    fn refresh_status(&mut self) {
        let doc = self.doc();
        let label = doc.label();
        let language = language_for(doc.file_path.as_deref());
        let le = line_ending_label(doc.editor.buffer().line_ending());
        let head = doc.editor.selections().primary().head;
        let pos = doc.editor.buffer().char_to_position(head);
        let lines = doc.editor.buffer().len_lines();

        let left = format!("{label}  ·  {language}  ·  {le}");
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
    /// the walk we return to the document the user was on.
    fn save_all(&mut self) {
        let original_active = self.active;
        for i in 0..self.docs.len() {
            if !self.docs[i].dirty {
                continue;
            }
            if i != self.active {
                self.active = i;
            }
            self.save_to_file();
        }
        if original_active < self.docs.len() {
            self.active = original_active;
        }
        self.update_title();
        self.refresh_tabs_text();
        self.refresh_find_text();
        self.scene_dirty = true;
        self.text_dirty = true;
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
                {
                    let d = self.doc_mut();
                    d.file_path = Some(path);
                    d.dirty = false;
                }
                self.update_title();
                self.refresh_tabs_text();
                self.scene_dirty = true;
                self.window.request_redraw();
            }
            Err(e) => log::error!("save failed: {}", e),
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
        let suffix = if count == 0 {
            "no matches".to_string()
        } else {
            format!("{}/{}", find.current_index() + 1, count)
        };
        let caption = format!("Find: {}   {}", find.query(), suffix);
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
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => self.close_find(),
            Key::Named(NamedKey::Enter) => {
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
            }
        }
    }

    /// The find bar's backdrop rectangle. Tucked under the tab strip.
    fn find_panel_rect(&self) -> Rect {
        let pad = FIND_PAD_DIP * self.scale;
        let width = FIND_WIDTH_DIP * self.scale;
        // Sit just below the tab strip rather than at the very top.
        let top = (TAB_BAR_HEIGHT_DIP + FIND_TOP_DIP) * self.scale;
        let height = self.line_height() + 2.0 * pad;
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
    fn handle_mouse_move(&mut self, x: f32, y: f32) {
        self.mouse_pos = Some((x, y));
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

        // Selection highlights sit behind text and carets.
        for selection in self.doc().editor.selections().iter() {
            for rect in self.selection_rects(selection) {
                root.push_child(SceneNode::quad(rect, SceneColor::rgba(120, 160, 255, 64)));
            }
        }

        // Carets on top of the highlights.
        let line_height = self.line_height();
        let scroll = self.doc().scroll_y;
        for selection in self.doc().editor.selections().iter() {
            if let Some((cx, cy)) = self.caret_pixel(selection.head) {
                root.push_child(SceneNode::quad(
                    Rect::new(
                        self.text_inset_x + cx,
                        self.text_inset_y + cy - scroll,
                        self.caret_width,
                        line_height,
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
            self.text.set_content(&mut self.font_system, &new_text);
            self.text_dirty = false;
        }
        if self.scene_dirty {
            if self.follow_caret {
                self.ensure_caret_visible();
                self.follow_caret = false;
            }
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

        // Editor text.
        let inset_x = self.text_inset_x;
        let inset_y = self.text_inset_y;
        let scroll = self.docs[self.active].scroll_y;
        self.text.viewport.update(&self.gpu.queue, resolution);
        self.text
            .renderer
            .prepare(
                &self.gpu.device,
                &self.gpu.queue,
                &mut self.font_system,
                &mut self.text.atlas,
                &self.text.viewport,
                [TextArea {
                    buffer: &self.text.buffer,
                    left: inset_x,
                    top: inset_y - scroll,
                    scale: 1.0,
                    bounds: editor_text_bounds,
                    default_color: Color::rgb(238, 238, 238),
                    custom_glyphs: &[],
                }],
                &mut self.swash_cache,
            )
            .expect("text prepare failed");

        // Tab strip labels. Centred vertically inside the strip; one cell per
        // label, sized roughly to TAB_LABEL_CHARS monospace chars.
        let tab_strip_h = TAB_BAR_HEIGHT_DIP * self.scale;
        let tab_text_pad_x = TAB_PAD_X_DIP * self.scale;
        let tab_text_y = (tab_strip_h - self.line_height()) * 0.5;
        let strip_bounds = TextBounds {
            left: 0,
            top: 0,
            right: surface_w as i32,
            bottom: tab_strip_h as i32,
        };
        self.tabs_text.viewport.update(&self.gpu.queue, resolution);
        self.tabs_text
            .renderer
            .prepare(
                &self.gpu.device,
                &self.gpu.queue,
                &mut self.font_system,
                &mut self.tabs_text.atlas,
                &self.tabs_text.viewport,
                [TextArea {
                    buffer: &self.tabs_text.buffer,
                    left: tab_text_pad_x,
                    top: tab_text_y,
                    scale: 1.0,
                    bounds: strip_bounds,
                    default_color: Color::rgb(220, 220, 220),
                    custom_glyphs: &[],
                }],
                &mut self.swash_cache,
            )
            .expect("tabs text prepare failed");

        // Close "×" glyph — shaped once, drawn at every tab's close-button
        // position via N TextAreas pointing at the same Buffer.
        let docs_len = self.docs.len();
        let scale_factor = self.scale;
        let slot_w = TAB_WIDTH_DIP * scale_factor;
        let close_w = TAB_CLOSE_W_DIP * scale_factor;
        let close_pad = TAB_CLOSE_PAD_DIP * scale_factor;
        // Center the glyph horizontally inside the close-rect — the "×"
        // glyph is narrower than the rect, so push it in by a quarter.
        let close_glyph_offset_x = close_w * 0.25;
        self.close_text.viewport.update(&self.gpu.queue, resolution);
        self.close_text
            .renderer
            .prepare(
                &self.gpu.device,
                &self.gpu.queue,
                &mut self.font_system,
                &mut self.close_text.atlas,
                &self.close_text.viewport,
                (0..docs_len).map(|i| {
                    let slot_x = i as f32 * slot_w;
                    let close_x = slot_x + slot_w - close_w - close_pad + close_glyph_offset_x;
                    TextArea {
                        buffer: &self.close_text.buffer,
                        left: close_x,
                        top: tab_text_y,
                        scale: 1.0,
                        bounds: strip_bounds,
                        default_color: Color::rgb(180, 180, 190),
                        custom_glyphs: &[],
                    }
                }),
                &mut self.swash_cache,
            )
            .expect("close text prepare failed");

        // Status bar text — left half is `path · lang · LE`, right half is
        // `Ln L, Col C · N lines`. Both are single-line and centred
        // vertically inside the bar; the right half is positioned by the
        // measured width of its shaped buffer so the caption ends one
        // STATUS_PAD_X from the window's right edge.
        let status_bar = self.status_bar_rect();
        let status_y = status_bar.min_y() + (status_bar.size.height - self.line_height()) * 0.5;
        let status_pad_x = STATUS_PAD_X_DIP * self.scale;
        let status_left_x = status_bar.min_x() + status_pad_x;
        let status_right_x = (status_bar.min_x() + status_bar.size.width
            - status_pad_x
            - shaped_width(&self.status_right))
        .max(status_left_x);
        let status_color = Color::rgb(180, 180, 190);
        let status_bounds = TextBounds {
            left: 0,
            top: status_bar.min_y() as i32,
            right: surface_w as i32,
            bottom: surface_h as i32,
        };
        self.status_left
            .viewport
            .update(&self.gpu.queue, resolution);
        self.status_left
            .renderer
            .prepare(
                &self.gpu.device,
                &self.gpu.queue,
                &mut self.font_system,
                &mut self.status_left.atlas,
                &self.status_left.viewport,
                [TextArea {
                    buffer: &self.status_left.buffer,
                    left: status_left_x,
                    top: status_y,
                    scale: 1.0,
                    bounds: status_bounds,
                    default_color: status_color,
                    custom_glyphs: &[],
                }],
                &mut self.swash_cache,
            )
            .expect("status-left prepare failed");
        self.status_right
            .viewport
            .update(&self.gpu.queue, resolution);
        self.status_right
            .renderer
            .prepare(
                &self.gpu.device,
                &self.gpu.queue,
                &mut self.font_system,
                &mut self.status_right.atlas,
                &self.status_right.viewport,
                [TextArea {
                    buffer: &self.status_right.buffer,
                    left: status_right_x,
                    top: status_y,
                    scale: 1.0,
                    bounds: status_bounds,
                    default_color: status_color,
                    custom_glyphs: &[],
                }],
                &mut self.swash_cache,
            )
            .expect("status-right prepare failed");

        let find_open = self.doc().find.is_some();
        if find_open {
            let (fx, fy) = self.find_text_origin();
            self.find_text.viewport.update(&self.gpu.queue, resolution);
            self.find_text
                .renderer
                .prepare(
                    &self.gpu.device,
                    &self.gpu.queue,
                    &mut self.font_system,
                    &mut self.find_text.atlas,
                    &self.find_text.viewport,
                    [TextArea {
                        buffer: &self.find_text.buffer,
                        left: fx,
                        top: fy,
                        scale: 1.0,
                        bounds: full_bounds,
                        default_color: Color::rgb(238, 238, 238),
                        custom_glyphs: &[],
                    }],
                    &mut self.swash_cache,
                )
                .expect("find text prepare failed");
        }

        let palette_open = self.palette.is_some();
        if palette_open {
            let (px, py) = self.palette_text_origin();
            self.palette_text
                .viewport
                .update(&self.gpu.queue, resolution);
            self.palette_text
                .renderer
                .prepare(
                    &self.gpu.device,
                    &self.gpu.queue,
                    &mut self.font_system,
                    &mut self.palette_text.atlas,
                    &self.palette_text.viewport,
                    [TextArea {
                        buffer: &self.palette_text.buffer,
                        left: px,
                        top: py,
                        scale: 1.0,
                        bounds: full_bounds,
                        default_color: Color::rgb(238, 238, 238),
                        custom_glyphs: &[],
                    }],
                    &mut self.swash_cache,
                )
                .expect("palette text prepare failed");
        }

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
            // overlay panels); then editor text; then tab labels; then
            // overlay text on top.
            self.quads.render(&mut pass);
            self.text
                .renderer
                .render(&self.text.atlas, &self.text.viewport, &mut pass)
                .expect("text render failed");
            self.tabs_text
                .renderer
                .render(&self.tabs_text.atlas, &self.tabs_text.viewport, &mut pass)
                .expect("tabs text render failed");
            self.close_text
                .renderer
                .render(&self.close_text.atlas, &self.close_text.viewport, &mut pass)
                .expect("close text render failed");
            self.status_left
                .renderer
                .render(
                    &self.status_left.atlas,
                    &self.status_left.viewport,
                    &mut pass,
                )
                .expect("status-left render failed");
            self.status_right
                .renderer
                .render(
                    &self.status_right.atlas,
                    &self.status_right.viewport,
                    &mut pass,
                )
                .expect("status-right render failed");
            if find_open {
                self.find_text
                    .renderer
                    .render(&self.find_text.atlas, &self.find_text.viewport, &mut pass)
                    .expect("find text render failed");
            }
            if palette_open {
                self.palette_text
                    .renderer
                    .render(
                        &self.palette_text.atlas,
                        &self.palette_text.viewport,
                        &mut pass,
                    )
                    .expect("palette text render failed");
            }
        }
        self.gpu.queue.submit(Some(encoder.finish()));
        frame.present();
        self.text.atlas.trim();
        self.tabs_text.atlas.trim();
        self.close_text.atlas.trim();
        self.status_left.atlas.trim();
        self.status_right.atlas.trim();
        if palette_open {
            self.palette_text.atlas.trim();
        }
        if find_open {
            self.find_text.atlas.trim();
        }

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
    /// Kept alive so the watcher thread doesn't shut down; consulted only
    /// via the user-event proxy so the field itself is otherwise unused.
    _settings_watcher: Option<RecommendedWatcher>,
    state: Option<State>,
}

impl App {
    fn new(
        initial_text: String,
        file_path: Option<PathBuf>,
        settings: Settings,
        settings_path: Option<PathBuf>,
        settings_watcher: Option<RecommendedWatcher>,
    ) -> Self {
        Self {
            cold_start: Instant::now(),
            initial_text,
            file_path,
            settings,
            settings_path,
            _settings_watcher: settings_watcher,
            state: None,
        }
    }
}

impl ApplicationHandler<AppEvent> for App {
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: AppEvent) {
        match event {
            AppEvent::SettingsChanged => {
                let Some(path) = self.settings_path.as_ref() else {
                    return;
                };
                let new_settings = Settings::load_or_default(path);
                // macOS fsevent fires several events for one save (write
                // tmp, rename, attribute change) — bail when the parsed
                // contents haven't actually changed so the log isn't N×.
                if new_settings == self.settings {
                    return;
                }
                if let Some(state) = self.state.as_mut() {
                    state.reload_settings(&new_settings);
                }
                self.settings = new_settings;
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
    let settings = match settings_path.as_deref() {
        Some(p) => Settings::load_or_default(p),
        None => {
            log::warn!("no XDG config dir; using default settings");
            Settings::default()
        }
    };

    let event_loop = EventLoop::<AppEvent>::with_user_event()
        .build()
        .expect("failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    // Watch settings.toml so font_size / line_height / tab_size hot-reload
    // without a restart. Skipped if the OS has no config dir, the parent
    // directory can't be created, or notify rejects the path.
    let watcher = settings_path
        .as_deref()
        .and_then(|p| spawn_settings_watcher(p, event_loop.create_proxy()));

    let mut app = App::new(initial_text, file_path, settings, settings_path, watcher);
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
