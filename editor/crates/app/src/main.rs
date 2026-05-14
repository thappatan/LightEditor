// Light Editor — application entry point.
//
// An editable text surface: keyboard input drives editor-core, the editor
// state is turned into a scene-graph each change, and the scene is rendered —
// background panel + caret quads via QuadRenderer, buffer text via TextStack.
// Keystroke latency (key press -> frame presented) is logged against the
// spec §8 16ms target.
//
// Still missing for a "real" editor: selection highlight, mouse input,
// scrolling, multiple buffers/panes, a proper widget tree. Those are later
// M1 PRs.

use std::sync::Arc;
use std::time::Instant;

use editor_core::{Editor, Position, Selection};
use editor_ui_render::{GpuContext, QuadRenderer};
use editor_ui_scene::{Color as SceneColor, Rect, Scene, SceneNode};
use editor_ui_text::glyphon::{Color, Resolution, TextArea, TextBounds};
use editor_ui_text::TextStack;
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

const WINDOW_WIDTH: u32 = 1280;
const WINDOW_HEIGHT: u32 = 720;

/// Inset of the text block from the window's top-left, in physical pixels.
const TEXT_INSET: f32 = 40.0;
/// Padding subtracted from the window size to get the text-wrap width/height.
const TEXT_PADDING: f32 = 80.0;
/// Line height — must match `editor-ui-text`'s metrics (24pt / 32px).
const LINE_HEIGHT: f32 = 32.0;
/// Caret width in physical pixels.
const CARET_WIDTH: f32 = 2.0;

/// A spaces-per-tab stand-in until config lands.
const TAB_AS_SPACES: &str = "    ";

/// Initial buffer content — the spec §3.4 multilingual matrix doubles as a
/// smoke test that editing keeps complex scripts intact.
const WELCOME_TEXT: &str = "\
LightEditor — editable surface\n\
\n\
Type to edit. Arrows move; Shift+Arrows select.\n\
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
    /// Latched keyboard modifiers (Shift for selection-extending movement).
    modifiers: ModifiersState,
    /// Last known pointer position, in physical pixels.
    mouse_pos: (f32, f32),
    /// While a left-button drag is in progress, the `char` index where it
    /// started (the selection's anchor).
    drag_anchor: Option<usize>,
    /// Vertical scroll offset, in physical pixels. The text and everything
    /// positioned against it is drawn shifted up by this much.
    scroll_y: f32,
    /// Set when the change that dirtied the scene moved the caret (an edit,
    /// arrow key, click, or drag) — the next rebuild scrolls it into view.
    /// Wheel scrolling leaves this `false` so it isn't yanked back.
    follow_caret: bool,
    /// The buffer text changed — TextStack needs a reshape before the next frame.
    text_dirty: bool,
    /// The editor state changed — the scene needs rebuilding before the next frame.
    scene_dirty: bool,

    /// When the last unhandled key press happened, for keystroke-latency timing.
    pending_keystroke: Option<Instant>,
    /// Rolling 1-second frame-time window (spec §8).
    frame_count: u64,
    last_report: Instant,
    last_frame_us: u128,
    cold_start: Option<Instant>,
}

impl State {
    fn new(window: Arc<Window>, cold_start: Instant) -> Self {
        let size = window.inner_size();
        let gpu = GpuContext::new(window.clone());
        let quads = QuadRenderer::new(&gpu.device, gpu.format());
        let text = TextStack::new(
            &gpu.device,
            &gpu.queue,
            gpu.format(),
            size.width as f32 - TEXT_PADDING,
            WELCOME_TEXT,
        );
        let editor = Editor::from(WELCOME_TEXT);
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
            modifiers: ModifiersState::empty(),
            mouse_pos: (0.0, 0.0),
            drag_anchor: None,
            scroll_y: 0.0,
            follow_caret: false,
            text_dirty: false,
            scene_dirty: true,
            pending_keystroke: None,
            frame_count: 0,
            last_report: Instant::now(),
            last_frame_us: 0,
            cold_start: Some(cold_start),
        };
        state.rebuild_scene();
        state
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        self.gpu.resize(size.width, size.height);
        self.text.set_width(size.width as f32 - TEXT_PADDING);
        self.text_dirty = true;
        self.scene_dirty = true;
        // ControlFlow::Wait won't redraw on its own — a resize must ask.
        self.window.request_redraw();
    }

    /// Route a key press into `editor`. Returns whether the editor changed.
    fn handle_key(&mut self, event: KeyEvent) {
        if event.state != ElementState::Pressed {
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
                self.editor.insert(TAB_AS_SPACES);
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
            self.pending_keystroke = Some(Instant::now());
            self.window.request_redraw();
        }
    }

    /// Left-button press: place a single cursor where the pointer is and start
    /// a potential drag (the anchor stays here while the head follows).
    fn handle_mouse_press(&mut self) {
        let Some(char_idx) = self.char_at_pixel(self.mouse_pos.0, self.mouse_pos.1) else {
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
    /// once per *visual* row — `buffer.len_lines() * LINE_HEIGHT` would
    /// undercount because it only sees logical lines.
    fn content_height(&self) -> f32 {
        self.text
            .buffer
            .layout_runs()
            .map(|run| run.line_top + run.line_height)
            .fold(0.0_f32, f32::max)
            .max(LINE_HEIGHT)
    }

    /// The largest valid scroll offset — content height beyond the viewport.
    fn max_scroll(&self) -> f32 {
        let visible = self.gpu.surface_config.height as f32 - TEXT_INSET;
        (self.content_height() - visible).max(0.0)
    }

    /// Scroll the viewport the minimum amount needed to bring the primary
    /// caret fully into view. A no-op when it is already visible.
    fn ensure_caret_visible(&mut self) {
        let head = self.editor.selections().primary().head;
        let Some((_, caret_top)) = self.caret_pixel(head) else {
            return;
        };
        let caret_bottom = caret_top + LINE_HEIGHT;
        let visible = self.gpu.surface_config.height as f32 - TEXT_INSET;

        if caret_top < self.scroll_y {
            self.scroll_y = caret_top;
        } else if caret_bottom > self.scroll_y + visible {
            self.scroll_y = caret_bottom - visible;
        }
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll());
    }

    /// Pointer moved. During a drag, extend the selection from the drag anchor
    /// to the pointer; otherwise just remember the position.
    fn handle_mouse_move(&mut self, x: f32, y: f32) {
        self.mouse_pos = (x, y);
        let Some(anchor) = self.drag_anchor else {
            return;
        };
        let Some(head) = self.char_at_pixel(x, y) else {
            return;
        };
        self.editor.set_selection(Selection::new(anchor, head));
        self.scene_dirty = true;
        self.follow_caret = true;
        self.window.request_redraw();
    }

    /// Hit-test a physical-pixel point to a buffer `char` index.
    ///
    /// Goes through cosmic-text's shaped layout (`Buffer::hit`), so the
    /// mapping is correct for complex scripts — a click lands on a real
    /// grapheme boundary, not an assumed monospace column.
    fn char_at_pixel(&self, x: f32, y: f32) -> Option<usize> {
        // Into text-origin-relative coordinates, undoing the scroll offset.
        let tx = x - TEXT_INSET;
        let ty = y - TEXT_INSET + self.scroll_y;
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
        for selection in self.editor.selections().iter() {
            if let Some((cx, cy)) = self.caret_pixel(selection.head) {
                root.push_child(SceneNode::quad(
                    Rect::new(
                        TEXT_INSET + cx,
                        TEXT_INSET + cy - self.scroll_y,
                        CARET_WIDTH,
                        LINE_HEIGHT,
                    ),
                    SceneColor::rgb(120, 160, 255),
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
        let scroll = self.scroll_y;
        let mut push = |x0: f32, y: f32, x1: f32| {
            rects.push(Rect::new(
                TEXT_INSET + x0,
                TEXT_INSET + y - scroll,
                (x1 - x0).max(3.0),
                LINE_HEIGHT,
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
    /// Uses the shaped cosmic-text layout, so the x position is correct for
    /// complex scripts — the caret lands on a real glyph boundary, not an
    /// assumed monospace column.
    fn caret_pixel_at(&self, line: usize, column: usize) -> Option<(f32, f32)> {
        let line_str = self.editor.buffer().line(line)?;
        let byte_in_line = line_str
            .char_indices()
            .nth(column)
            .map(|(b, _)| b)
            .unwrap_or(line_str.len());

        for run in self.text.buffer.layout_runs() {
            if run.line_i != line {
                continue;
            }
            let y = run.line_top;
            let mut x = 0.0;
            for glyph in run.glyphs.iter() {
                if glyph.start >= byte_in_line {
                    return Some((glyph.x, y));
                }
                x = glyph.x + glyph.w;
            }
            // Past the last glyph — caret sits at the end of the line.
            return Some((x, y));
        }
        // Line has no layout run (e.g. an empty trailing line).
        Some((0.0, line as f32 * LINE_HEIGHT))
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
                    left: TEXT_INSET,
                    top: TEXT_INSET - self.scroll_y,
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
            // Caret quads first, text on top.
            self.quads.render(&mut pass);
            self.text
                .renderer
                .render(&self.text.atlas, &self.text.viewport, &mut pass)
                .expect("text render failed");
        }
        self.gpu.queue.submit(Some(encoder.finish()));
        frame.present();
        self.text.atlas.trim();

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
    state: Option<State>,
}

impl App {
    fn new() -> Self {
        Self {
            cold_start: Instant::now(),
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
            .with_title("LightEditor — editable surface")
            .with_inner_size(PhysicalSize::new(WINDOW_WIDTH, WINDOW_HEIGHT));
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("failed to create window"),
        );
        let state = State::new(window, self.cold_start);
        // ControlFlow::Wait only draws on demand — kick off the first frame.
        state.window.request_redraw();
        self.state = Some(state);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => {
                log::info!("close requested — exiting");
                event_loop.exit();
            }
            WindowEvent::Resized(size) => state.resize(size),
            WindowEvent::ScaleFactorChanged { .. } => {
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
                    MouseScrollDelta::LineDelta(_, lines) => lines * LINE_HEIGHT,
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

    let event_loop = EventLoop::new().expect("failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App::new();
    event_loop.run_app(&mut app).expect("event loop failed");
}
