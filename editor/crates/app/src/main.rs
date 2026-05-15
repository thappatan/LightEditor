// Light Editor — application entry point.
//
// An editable text surface: keyboard and mouse input drive editor-core, the
// editor state is turned into a scene-graph each change, and the scene is
// rendered — selection highlights + caret quads via QuadRenderer, buffer text
// via TextStack. The viewport scrolls (wheel, caret-follow, drag-past-edge)
// and everything is sized in physical pixels scaled by the window's DPI.
//
// Still missing for a "real" editor: multiple buffers/panes, a proper widget
// tree, find/replace, the file tree. Those are later M1 work.

mod find;
mod palette;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use editor_config::Settings;
use editor_core::{Editor, Position, Selection};
use editor_ui_render::{GpuContext, QuadRenderer};
use editor_ui_scene::{Color as SceneColor, Rect, Scene, SceneNode};
use editor_ui_text::glyphon::{Color, Resolution, TextArea, TextBounds};
use editor_ui_text::TextStack;
use find::FindBar;
use palette::{Command, CommandPalette};
use wgpu::{
    LoadOp, Operations, RenderPassColorAttachment, RenderPassDescriptor, StoreOp,
    TextureViewDescriptor,
};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

/// Initial window size, in logical pixels.
const WINDOW_WIDTH: u32 = 1280;
const WINDOW_HEIGHT: u32 = 720;

/// Inset of the text block from the window's top-left, in *logical* pixels.
/// Multiplied by the window scale factor to get physical pixels.
const TEXT_INSET_DIP: f32 = 28.0;
/// Total horizontal padding (inset on both sides), in logical pixels.
const TEXT_PADDING_DIP: f32 = 2.0 * TEXT_INSET_DIP;
/// Caret width, in logical pixels.
const CARET_WIDTH_DIP: f32 = 2.0;

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
    text: TextStack,

    /// Editing model — buffer, multi-cursor selections, undo tree.
    editor: Editor,
    /// The scene rebuilt from `editor` whenever it changes.
    scene: Scene,

    /// Window scale factor; physical = logical * scale.
    scale: f32,
    /// `TEXT_INSET_DIP` / `TEXT_PADDING_DIP` / `CARET_WIDTH_DIP`, in physical
    /// pixels for the current scale.
    text_inset: f32,
    text_padding: f32,
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
    /// Vertical scroll offset, in physical pixels. The text and everything
    /// positioned against it is drawn shifted up by this much.
    scroll_y: f32,
    /// Set when the change that dirtied the scene moved the caret (an edit,
    /// arrow key, or click) — the next rebuild scrolls it into view. Wheel
    /// scrolling and drag-scrolling leave this `false` so they aren't undone.
    follow_caret: bool,
    /// The buffer text changed — TextStack needs a reshape before the next frame.
    text_dirty: bool,
    /// The editor state changed — the scene needs rebuilding before the next frame.
    scene_dirty: bool,

    /// Path the buffer was loaded from / saves to. `None` for the welcome
    /// scratch buffer.
    file_path: Option<PathBuf>,
    /// Has the buffer changed since the last load or save? Shown as a "•"
    /// prefix in the window title.
    dirty: bool,

    /// Command-palette state when the popup is open.
    palette: Option<CommandPalette>,
    /// Second TextStack dedicated to the palette overlay so it can shape
    /// independently of the buffer.
    palette_text: TextStack,

    /// Find-bar state when the bar is visible.
    find: Option<FindBar>,
    /// Third TextStack for the find-bar caption ("Find: query   3/12").
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

        let text_padding = TEXT_PADDING_DIP * scale;
        let text = TextStack::new(
            &gpu.device,
            &gpu.queue,
            gpu.format(),
            size.width as f32 - text_padding,
            font_size_pt,
            line_height_pt,
            scale,
            initial_text,
        );
        let editor = Editor::from(initial_text);

        // The palette has its own TextStack so the editor's single shaped
        // buffer doesn't have to swap content every keystroke in the palette.
        let palette_width = (PALETTE_WIDTH_DIP - 2.0 * PALETTE_PAD_DIP) * scale;
        let palette_text = TextStack::new(
            &gpu.device,
            &gpu.queue,
            gpu.format(),
            palette_width,
            font_size_pt,
            line_height_pt,
            scale,
            "",
        );

        // Find-bar TextStack, same reasoning — single-row caption.
        let find_width = (FIND_WIDTH_DIP - 2.0 * FIND_PAD_DIP) * scale;
        let find_text = TextStack::new(
            &gpu.device,
            &gpu.queue,
            gpu.format(),
            find_width,
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
            text,
            editor,
            scene,
            scale,
            text_inset: TEXT_INSET_DIP * scale,
            text_padding,
            caret_width: CARET_WIDTH_DIP * scale,
            modifiers: ModifiersState::empty(),
            mouse_pos: None,
            drag_anchor: None,
            scroll_y: 0.0,
            follow_caret: false,
            text_dirty: false,
            scene_dirty: true,
            file_path,
            dirty: false,
            palette: None,
            palette_text,
            find: None,
            find_text,
            tab_spaces,
            pending_keystroke: None,
            frame_count: 0,
            last_report: Instant::now(),
            last_frame_us: 0,
            cold_start: Some(cold_start),
        };
        state.rebuild_scene();
        state
    }

    /// Line height in physical pixels — the single unit carets, highlights,
    /// and scroll math all work in. Sourced from TextStack so it can never
    /// drift from the actual font metrics.
    fn line_height(&self) -> f32 {
        self.text.line_height()
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        self.gpu.resize(size.width, size.height);
        self.text.set_width(size.width as f32 - self.text_padding);
        // Palette width is fixed; nothing to update on a window resize.
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
        self.text_inset = TEXT_INSET_DIP * scale;
        self.text_padding = TEXT_PADDING_DIP * scale;
        self.caret_width = CARET_WIDTH_DIP * scale;
        self.text.set_scale(scale);
        // Overlays have their own TextStacks — keep them in sync too.
        self.palette_text.set_scale(scale);
        self.palette_text
            .set_width((PALETTE_WIDTH_DIP - 2.0 * PALETTE_PAD_DIP) * scale);
        self.find_text.set_scale(scale);
        self.find_text
            .set_width((FIND_WIDTH_DIP - 2.0 * FIND_PAD_DIP) * scale);
        self.text_dirty = true;
        self.scene_dirty = true;
        self.window.request_redraw();
    }

    /// Route a key press into `editor`.
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
            if let Key::Character(c) = &event.logical_key {
                if c.as_str().eq_ignore_ascii_case("f") {
                    if self.find.is_some() {
                        self.close_find();
                    } else {
                        self.open_find();
                    }
                    return;
                }
                if c.as_str().eq_ignore_ascii_case("s") {
                    self.save_to_file();
                    return;
                }
                if c.as_str().eq_ignore_ascii_case("o") {
                    self.open_file_dialog();
                    return;
                }
                if c.as_str().eq_ignore_ascii_case("n") {
                    self.new_file();
                    return;
                }
            }
        }

        // When the find bar is open it captures every other key.
        if self.find.is_some() {
            self.handle_find_key(event);
            return;
        }

        let shift = self.modifiers.shift_key();

        let mut text_changed = true;
        let handled = match &event.logical_key {
            Key::Named(NamedKey::Backspace) => {
                self.editor.backspace();
                true
            }
            Key::Named(NamedKey::Delete) => {
                self.editor.delete_forward();
                true
            }
            Key::Named(NamedKey::Enter) => {
                self.editor.insert_newline();
                true
            }
            Key::Named(NamedKey::Space) => {
                self.editor.insert(" ");
                true
            }
            Key::Named(NamedKey::Tab) => {
                let spaces = self.tab_spaces.clone();
                self.editor.insert(&spaces);
                true
            }
            Key::Named(NamedKey::ArrowLeft) => {
                self.editor.move_left(shift);
                text_changed = false;
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.editor.move_right(shift);
                text_changed = false;
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.editor.move_up(shift);
                text_changed = false;
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.editor.move_down(shift);
                text_changed = false;
                true
            }
            // Printable character input — winit gives us the resolved text.
            _ => match &event.text {
                Some(text) if !text.is_empty() => {
                    self.editor.insert(text);
                    true
                }
                _ => false,
            },
        };

        if handled {
            self.text_dirty |= text_changed;
            self.scene_dirty = true;
            self.follow_caret = true;
            if text_changed && !self.dirty {
                self.dirty = true;
                self.update_title();
            }
            // If the find bar is open, the match list now reflects the old text.
            if text_changed && self.find.is_some() {
                let buffer_text = self.editor.text();
                if let Some(f) = self.find.as_mut() {
                    f.refresh(&buffer_text);
                }
                self.refresh_find_text();
            }
            self.pending_keystroke = Some(Instant::now());
            self.window.request_redraw();
        }
    }

    /// Left-button press: place a single cursor where the pointer is and start
    /// a potential drag (the anchor stays here while the head follows).
    fn handle_mouse_press(&mut self) {
        let Some((mx, my)) = self.mouse_pos else {
            return;
        };
        let Some(char_idx) = self.char_at_pixel(mx, my) else {
            return;
        };
        self.drag_anchor = Some(char_idx);
        self.editor.set_selection(Selection::cursor(char_idx));
        self.scene_dirty = true;
        self.follow_caret = true;
        self.window.request_redraw();
    }

    /// Left-button release: end any drag.
    fn handle_mouse_release(&mut self) {
        self.drag_anchor = None;
    }

    /// Write the buffer to `file_path`, or prompt for one with a Save As
    /// dialog when there is none.
    fn save_to_file(&mut self) {
        let path = match self.file_path.clone() {
            Some(p) => p,
            None => match rfd::FileDialog::new().save_file() {
                Some(p) => p,
                None => return, // cancelled
            },
        };
        match std::fs::write(&path, self.editor.text()) {
            Ok(()) => {
                log::info!("saved {}", path.display());
                self.file_path = Some(path);
                self.dirty = false;
                self.update_title();
            }
            Err(e) => log::error!("save failed for {}: {}", path.display(), e),
        }
    }

    /// Prompt for a file with an Open dialog and load it, replacing the
    /// editor. The user can cancel; on read failure we log and keep the
    /// current buffer.
    fn open_file_dialog(&mut self) {
        if !self.confirm_unsaved("Open") {
            return;
        }
        let Some(path) = rfd::FileDialog::new().pick_file() else {
            return;
        };
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                self.editor = Editor::from(content.as_str());
                self.file_path = Some(path);
                self.scroll_y = 0.0;
                self.dirty = false;
                self.text_dirty = true;
                self.scene_dirty = true;
                self.follow_caret = false;
                self.update_title();
                self.window.request_redraw();
            }
            Err(e) => log::error!("could not read {}: {}", path.display(), e),
        }
    }

    /// Sync the window title with the file path and dirty state.
    fn update_title(&self) {
        let base = window_title(self.file_path.as_deref());
        let title = if self.dirty {
            format!("• {base}")
        } else {
            base
        };
        self.window.set_title(&title);
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
        // blank row between the query and the list
        text.push_str("\n\n");
        for cmd in palette.visible() {
            text.push_str("  ");
            text.push_str(cmd.label());
            text.push('\n');
        }
        self.palette_text.set_content(&text);
    }

    /// Route a key while the palette is open. Movement / edit keys are
    /// captured by the palette instead of the buffer.
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
                // Treat the resolved text (a character or two for IME) as
                // query input. Drop control-modified key presses.
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
        }
    }

    /// Save As: always prompt for a path, even if the buffer already has one.
    fn save_as(&mut self) {
        let Some(path) = rfd::FileDialog::new().save_file() else {
            return;
        };
        match std::fs::write(&path, self.editor.text()) {
            Ok(()) => {
                log::info!("saved as {}", path.display());
                self.file_path = Some(path);
                self.dirty = false;
                self.update_title();
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
        // Header row (query) + blank row + one row per visible command.
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

    /// Where the palette TextStack should be drawn (the inner padded area).
    fn palette_text_origin(&self) -> (f32, f32) {
        let panel = self.palette_panel_rect();
        let pad = PALETTE_PAD_DIP * self.scale;
        (panel.min_x() + pad, panel.min_y() + pad)
    }

    /// The highlight rectangle for the currently-selected palette row.
    fn palette_selection_rect(&self) -> Option<Rect> {
        let palette = self.palette.as_ref()?;
        if palette.visible_count() == 0 {
            return None;
        }
        let lh = self.line_height();
        let (_ox, oy) = self.palette_text_origin();
        // Header takes one row, then a blank row; the list starts at row 2.
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

    /// Open the find bar with an empty query.
    fn open_find(&mut self) {
        self.find = Some(FindBar::new());
        self.refresh_find_text();
        self.scene_dirty = true;
        self.window.request_redraw();
    }

    /// Close the find bar (without changing the selection).
    fn close_find(&mut self) {
        self.find = None;
        self.scene_dirty = true;
        self.window.request_redraw();
    }

    /// Rebuild the find bar's caption "Find: query   3/12 / no matches".
    fn refresh_find_text(&mut self) {
        let Some(find) = self.find.as_ref() else {
            return;
        };
        let count = find.match_count();
        let suffix = if count == 0 {
            "no matches".to_string()
        } else {
            format!("{}/{}", find.current_index() + 1, count)
        };
        let caption = format!("Find: {}   {}", find.query(), suffix);
        self.find_text.set_content(&caption);
    }

    /// Move the editor selection to the find bar's current match, so the
    /// caret follows along (and the regular caret-follow scrolls it in).
    fn select_current_match(&mut self) {
        let Some(range) = self.find.as_ref().and_then(|f| f.current_match()) else {
            return;
        };
        self.editor
            .set_selection(Selection::new(range.start, range.end));
        self.scene_dirty = true;
        self.follow_caret = true;
    }

    /// Route a key while the find bar is open.
    fn handle_find_key(&mut self, event: KeyEvent) {
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => self.close_find(),
            Key::Named(NamedKey::Enter) => {
                if self.modifiers.shift_key() {
                    if let Some(f) = self.find.as_mut() {
                        f.prev_match();
                    }
                } else if let Some(f) = self.find.as_mut() {
                    f.next_match();
                }
                self.refresh_find_text();
                self.select_current_match();
                self.scene_dirty = true;
                self.window.request_redraw();
            }
            Key::Named(NamedKey::Backspace) => {
                let text = self.editor.text();
                if let Some(f) = self.find.as_mut() {
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
                    let buffer_text = self.editor.text();
                    if let Some(f) = self.find.as_mut() {
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

    /// The find bar's backdrop rectangle.
    fn find_panel_rect(&self) -> Rect {
        let pad = FIND_PAD_DIP * self.scale;
        let width = FIND_WIDTH_DIP * self.scale;
        let top = FIND_TOP_DIP * self.scale;
        let height = self.line_height() + 2.0 * pad;
        let surface_w = self.gpu.surface_config.width as f32;
        // Right-aligned with a margin so it doesn't sit on top of the caret.
        let left = surface_w - width - FIND_TOP_DIP * self.scale;
        Rect::new(left.max(0.0), top, width, height)
    }

    /// Where the find bar's caption is drawn.
    fn find_text_origin(&self) -> (f32, f32) {
        let panel = self.find_panel_rect();
        let pad = FIND_PAD_DIP * self.scale;
        (panel.min_x() + pad, panel.min_y() + pad)
    }

    /// Highlight rectangles for every match the find bar found, in the
    /// editor's coordinate system. Current match is omitted — the regular
    /// selection highlight already covers it (and a brighter overlay is
    /// added separately in `rebuild_scene`).
    fn match_highlight_rects(&self) -> Vec<Rect> {
        let Some(find) = self.find.as_ref() else {
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

    /// Reset the buffer to a blank scratch one with no file path. Asks first
    /// if the current buffer has unsaved changes.
    fn new_file(&mut self) {
        if !self.confirm_unsaved("New file") {
            return;
        }
        self.editor = Editor::from("");
        self.file_path = None;
        self.scroll_y = 0.0;
        self.dirty = false;
        self.text_dirty = true;
        self.scene_dirty = true;
        self.follow_caret = false;
        self.update_title();
        self.window.request_redraw();
    }

    /// If the buffer is dirty, ask the user what to do. Returns `true` if the
    /// caller may proceed (saved or discarded), `false` if it must abort
    /// (Cancel, or a Save As that was cancelled / failed). Clean buffers
    /// always return `true`.
    fn confirm_unsaved(&mut self, reason: &str) -> bool {
        if !self.dirty {
            return true;
        }
        let name = self
            .file_path
            .as_deref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "this buffer".to_string());
        let result = rfd::MessageDialog::new()
            .set_title(format!("{reason}: unsaved changes"))
            .set_description(format!("{name} has unsaved changes. Save them first?"))
            .set_level(rfd::MessageLevel::Warning)
            .set_buttons(rfd::MessageButtons::YesNoCancel)
            .show();
        match result {
            rfd::MessageDialogResult::Yes => {
                self.save_to_file();
                // Save As dialog may have been cancelled, or write may have
                // failed — both leave `dirty` true.
                !self.dirty
            }
            rfd::MessageDialogResult::No => true, // discard
            _ => false,                           // Cancel / closed dialog
        }
    }

    /// Pointer moved. During a drag, extend the selection from the drag anchor
    /// to the pointer; a drag past the top/bottom edge also scrolls the view.
    fn handle_mouse_move(&mut self, x: f32, y: f32) {
        self.mouse_pos = Some((x, y));
        let Some(anchor) = self.drag_anchor else {
            return;
        };

        // Dragging past a viewport edge scrolls the view one step per move
        // event. Continuous auto-scroll (without moving the mouse) would need
        // a timer — deferred.
        let surface_h = self.gpu.surface_config.height as f32;
        if y < self.text_inset {
            self.scroll_y = (self.scroll_y - (self.text_inset - y)).clamp(0.0, self.max_scroll());
        } else if y > surface_h {
            self.scroll_y = (self.scroll_y + (y - surface_h)).clamp(0.0, self.max_scroll());
        }

        let Some(head) = self.char_at_pixel(x, y) else {
            return;
        };
        self.editor.set_selection(Selection::new(anchor, head));
        self.scene_dirty = true;
        // We scrolled deliberately above; don't let caret-follow fight it.
        self.follow_caret = false;
        self.window.request_redraw();
    }

    /// Mouse wheel: scroll the viewport vertically. A positive `delta_y`
    /// scrolls toward the top of the document.
    fn handle_scroll(&mut self, delta_y: f32) {
        let new = (self.scroll_y - delta_y).clamp(0.0, self.max_scroll());
        if new != self.scroll_y {
            self.scroll_y = new;
            self.scene_dirty = true;
            self.window.request_redraw();
        }
    }

    /// Total shaped text height in physical pixels.
    ///
    /// Measured from the actual cosmic-text layout, so wrapped lines count
    /// once per *visual* row — `buffer.len_lines() * line_height` would
    /// undercount because it only sees logical lines.
    fn content_height(&self) -> f32 {
        self.text
            .buffer
            .layout_runs()
            .map(|run| run.line_top + run.line_height)
            .fold(0.0_f32, f32::max)
            .max(self.line_height())
    }

    /// The largest valid scroll offset — content height beyond the viewport.
    fn max_scroll(&self) -> f32 {
        let visible = self.gpu.surface_config.height as f32 - self.text_inset;
        (self.content_height() - visible).max(0.0)
    }

    /// Scroll the viewport the minimum amount needed to bring the primary
    /// caret fully into view. A no-op when it is already visible.
    fn ensure_caret_visible(&mut self) {
        let head = self.editor.selections().primary().head;
        let Some((_, caret_top)) = self.caret_pixel(head) else {
            return;
        };
        let caret_bottom = caret_top + self.line_height();
        let visible = self.gpu.surface_config.height as f32 - self.text_inset;

        if caret_top < self.scroll_y {
            self.scroll_y = caret_top;
        } else if caret_bottom > self.scroll_y + visible {
            self.scroll_y = caret_bottom - visible;
        }
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll());
    }

    /// Hit-test a physical-pixel point to a buffer `char` index.
    ///
    /// Goes through cosmic-text's shaped layout (`Buffer::hit`), so the
    /// mapping is correct for complex scripts — a click lands on a real
    /// grapheme boundary, not an assumed monospace column.
    fn char_at_pixel(&self, x: f32, y: f32) -> Option<usize> {
        // Into text-origin-relative coordinates, undoing the scroll offset.
        let tx = x - self.text_inset;
        let ty = y - self.text_inset + self.scroll_y;
        let cursor = self.text.buffer.hit(tx, ty)?;

        // cosmic-text gives (line, byte-in-line); convert to a char index.
        let line_str = self.editor.buffer().line(cursor.line)?;
        let column = line_str
            .char_indices()
            .take_while(|(b, _)| *b < cursor.index)
            .count();
        self.editor
            .buffer()
            .position_to_char(Position::new(cursor.line, column))
    }

    /// Rebuild `scene` from the current editor state: selection-highlight
    /// quads under the text, then a caret quad per selection head.
    ///
    /// The buffer text is drawn by TextStack, not via a scene `Text` node yet
    /// — TextStack is still single-buffer.
    fn rebuild_scene(&mut self) {
        let w = self.gpu.surface_config.width as f32;
        let h = self.gpu.surface_config.height as f32;
        let mut root = SceneNode::group(Rect::new(0.0, 0.0, w, h));

        // Selection highlights first — they sit behind the text and the carets.
        for selection in self.editor.selections().iter() {
            for rect in self.selection_rects(selection) {
                root.push_child(SceneNode::quad(rect, SceneColor::rgba(120, 160, 255, 64)));
            }
        }

        // Carets on top of the highlights.
        let line_height = self.line_height();
        for selection in self.editor.selections().iter() {
            if let Some((cx, cy)) = self.caret_pixel(selection.head) {
                root.push_child(SceneNode::quad(
                    Rect::new(
                        self.text_inset + cx,
                        self.text_inset + cy - self.scroll_y,
                        self.caret_width,
                        line_height,
                    ),
                    SceneColor::rgb(120, 160, 255),
                ));
            }
        }

        // Find-bar match highlights sit *behind* the editor's own selection
        // highlight + caret so the current hit (which is the active selection)
        // stays visually dominant.
        for rect in self.match_highlight_rects() {
            root.push_child(SceneNode::quad(rect, SceneColor::rgba(255, 200, 60, 64)));
        }

        // Find bar itself — a single-row panel near the top, on top of the
        // editor surface.
        if self.find.is_some() {
            root.push_child(SceneNode::quad(
                self.find_panel_rect(),
                SceneColor::rgba(38, 38, 48, 240),
            ));
        }

        // Palette overlay sits on top of everything else — a dim scrim over
        // the editor, the panel itself, and a highlight on the selected row.
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

    /// The absolute highlight rectangles for `selection` — empty for a bare
    /// cursor, one rect for a single-line span, or one rect per line for a
    /// multi-line span (first line from the start column to end-of-content,
    /// middle lines full content width, last line from column 0 to the end).
    fn selection_rects(&self, selection: &Selection) -> Vec<Rect> {
        if selection.is_cursor() {
            return Vec::new();
        }
        let buffer = self.editor.buffer();
        let start = buffer.char_to_position(selection.start());
        let end = buffer.char_to_position(selection.end());
        let mut rects = Vec::new();

        // A tiny minimum width so a zero-width line (e.g. a selected blank
        // line, or a selected trailing newline) is still visible.
        let inset = self.text_inset;
        let scroll = self.scroll_y;
        let line_height = self.line_height();
        let mut push = |x0: f32, y: f32, x1: f32| {
            rects.push(Rect::new(
                inset + x0,
                inset + y - scroll,
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

        // First line: start column to end of content.
        if let (Some((sx, sy)), Some((ex, _))) = (
            self.caret_pixel_at(start.line, start.column),
            self.caret_pixel_at(start.line, self.line_content_chars(start.line)),
        ) {
            push(sx, sy, ex);
        }
        // Middle lines: column 0 to end of content.
        for line in (start.line + 1)..end.line {
            if let (Some((x0, y)), Some((x1, _))) = (
                self.caret_pixel_at(line, 0),
                self.caret_pixel_at(line, self.line_content_chars(line)),
            ) {
                push(x0, y, x1);
            }
        }
        // Last line: column 0 to the end column.
        if let (Some((x0, y)), Some((ex, _))) = (
            self.caret_pixel_at(end.line, 0),
            self.caret_pixel_at(end.line, end.column),
        ) {
            push(x0, y, ex);
        }
        rects
    }

    /// Number of `char`s in `line`'s content, excluding any trailing newline.
    fn line_content_chars(&self, line: usize) -> usize {
        self.editor
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

    /// Pixel offset of the caret at `char_idx`, relative to the text origin.
    fn caret_pixel(&self, char_idx: usize) -> Option<(f32, f32)> {
        let pos = self.editor.buffer().char_to_position(char_idx);
        self.caret_pixel_at(pos.line, pos.column)
    }

    /// Pixel offset of the caret at `(line, column)`, relative to the text
    /// origin.
    ///
    /// Uses the shaped cosmic-text layout. A wrapped logical line spans
    /// several visual runs (each with the same `line_i`); this picks the run
    /// whose byte range actually contains the caret, so the caret stays on the
    /// right visual row. The x position comes from a real glyph boundary, so
    /// it is correct for complex scripts too.
    fn caret_pixel_at(&self, line: usize, column: usize) -> Option<(f32, f32)> {
        let line_str = self.editor.buffer().line(line)?;
        let byte_in_line = line_str
            .char_indices()
            .nth(column)
            .map(|(b, _)| b)
            .unwrap_or(line_str.len());

        // Visual end of the last run seen for this line — the fallback when
        // the caret's byte falls past every run (e.g. trailing position).
        let mut last_run_end: Option<(f32, f32)> = None;

        for run in self.text.buffer.layout_runs() {
            if run.line_i != line {
                continue;
            }
            let run_start = run.glyphs.first().map(|g| g.start).unwrap_or(0);
            let run_end = run.glyphs.last().map(|g| g.end).unwrap_or(run_start);
            let run_end_x = run.glyphs.last().map(|g| g.x + g.w).unwrap_or(0.0);
            last_run_end = Some((run_end_x, run.line_top));

            // Is the caret within this visual run?
            if byte_in_line >= run_start && byte_in_line <= run_end {
                let mut x = 0.0;
                for glyph in run.glyphs.iter() {
                    if glyph.start >= byte_in_line {
                        return Some((glyph.x, run.line_top));
                    }
                    x = glyph.x + glyph.w;
                }
                // Past the last glyph of this run — caret at the run's end.
                return Some((x, run.line_top));
            }
        }

        // The caret's byte is past every run of this line, or the line has no
        // runs at all (an empty line still gets one, so the latter is rare).
        last_run_end.or(Some((0.0, line as f32 * self.line_height())))
    }

    fn render(&mut self) {
        let frame_start = Instant::now();

        if self.text_dirty {
            self.text.set_content(&self.editor.text());
            self.text_dirty = false;
        }
        if self.scene_dirty {
            if self.follow_caret {
                self.ensure_caret_visible();
                self.follow_caret = false;
            }
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

        self.text.viewport.update(
            &self.gpu.queue,
            Resolution {
                width: self.gpu.surface_config.width,
                height: self.gpu.surface_config.height,
            },
        );
        let inset = self.text_inset;
        self.text
            .renderer
            .prepare(
                &self.gpu.device,
                &self.gpu.queue,
                &mut self.text.font_system,
                &mut self.text.atlas,
                &self.text.viewport,
                [TextArea {
                    buffer: &self.text.buffer,
                    left: inset,
                    top: inset - self.scroll_y,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: 0,
                        top: 0,
                        right: self.gpu.surface_config.width as i32,
                        bottom: self.gpu.surface_config.height as i32,
                    },
                    default_color: Color::rgb(238, 238, 238),
                    custom_glyphs: &[],
                }],
                &mut self.text.swash_cache,
            )
            .expect("text prepare failed");

        // Find bar caption — its own TextStack.
        let find_open = self.find.is_some();
        if find_open {
            let (fx, fy) = self.find_text_origin();
            self.find_text.viewport.update(
                &self.gpu.queue,
                Resolution {
                    width: self.gpu.surface_config.width,
                    height: self.gpu.surface_config.height,
                },
            );
            self.find_text
                .renderer
                .prepare(
                    &self.gpu.device,
                    &self.gpu.queue,
                    &mut self.find_text.font_system,
                    &mut self.find_text.atlas,
                    &self.find_text.viewport,
                    [TextArea {
                        buffer: &self.find_text.buffer,
                        left: fx,
                        top: fy,
                        scale: 1.0,
                        bounds: TextBounds {
                            left: 0,
                            top: 0,
                            right: self.gpu.surface_config.width as i32,
                            bottom: self.gpu.surface_config.height as i32,
                        },
                        default_color: Color::rgb(238, 238, 238),
                        custom_glyphs: &[],
                    }],
                    &mut self.find_text.swash_cache,
                )
                .expect("find text prepare failed");
        }

        // Palette text overlay — its own TextStack so it shapes independently.
        let palette_open = self.palette.is_some();
        if palette_open {
            let (px, py) = self.palette_text_origin();
            self.palette_text.viewport.update(
                &self.gpu.queue,
                Resolution {
                    width: self.gpu.surface_config.width,
                    height: self.gpu.surface_config.height,
                },
            );
            self.palette_text
                .renderer
                .prepare(
                    &self.gpu.device,
                    &self.gpu.queue,
                    &mut self.palette_text.font_system,
                    &mut self.palette_text.atlas,
                    &self.palette_text.viewport,
                    [TextArea {
                        buffer: &self.palette_text.buffer,
                        left: px,
                        top: py,
                        scale: 1.0,
                        bounds: TextBounds {
                            left: 0,
                            top: 0,
                            right: self.gpu.surface_config.width as i32,
                            bottom: self.gpu.surface_config.height as i32,
                        },
                        default_color: Color::rgb(238, 238, 238),
                        custom_glyphs: &[],
                    }],
                    &mut self.palette_text.swash_cache,
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
            // Editor quads first (highlights, carets, find/palette underlays
            // + panels); then editor text; then overlay text on top.
            self.quads.render(&mut pass);
            self.text
                .renderer
                .render(&self.text.atlas, &self.text.viewport, &mut pass)
                .expect("text render failed");
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

        // Keystroke latency: from the key press to this frame being presented
        // (spec §8 target 16ms / hard limit 33ms).
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
    state: Option<State>,
}

impl App {
    fn new(initial_text: String, file_path: Option<PathBuf>, settings: Settings) -> Self {
        Self {
            cold_start: Instant::now(),
            initial_text,
            file_path,
            settings,
            state: None,
        }
    }
}

impl ApplicationHandler for App {
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
        // ControlFlow::Wait only draws on demand — kick off the first frame.
        state.window.request_redraw();
        self.state = Some(state);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested if state.confirm_unsaved("Close") => {
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
            WindowEvent::MouseWheel { delta, .. } => {
                // Normalize both delta kinds to physical pixels.
                let delta_y = match delta {
                    MouseScrollDelta::LineDelta(_, lines) => lines * state.line_height(),
                    MouseScrollDelta::PixelDelta(pos) => pos.y as f32,
                };
                state.handle_scroll(delta_y);
            }
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

    // Optional file path as the first CLI arg; falls back to the welcome
    // scratch buffer if absent or unreadable.
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

    // Load settings from ~/.config/lighteditor/settings.toml (XDG path) —
    // missing or malformed files fall through to the defaults.
    let settings = match dirs::config_dir() {
        Some(dir) => Settings::load_or_default(&dir.join(CONFIG_SUBDIR).join(CONFIG_FILENAME)),
        None => {
            log::warn!("no XDG config dir; using default settings");
            Settings::default()
        }
    };

    let event_loop = EventLoop::new().expect("failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App::new(initial_text, file_path, settings);
    event_loop.run_app(&mut app).expect("event loop failed");
}
