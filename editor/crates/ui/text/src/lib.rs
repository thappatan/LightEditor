//! Text rendering stack — cosmic-text shaping + swash raster + glyphon GPU draw.
//!
//! Spec §3.3 (text pipeline). One `TextStack` shapes a single piece of text
//! into a cosmic-text `Buffer`; the GPU side (atlas, renderer, viewport) is
//! shared across stacks via [`TextGpu`] so the app pays the wgpu setup cost
//! once and submits all text in a single `prepare` + `render` per frame.

use glyphon::{
    Attrs, Buffer, Cache, Family, FontSystem, Metrics, Shaping, SwashCache, TextAtlas,
    TextRenderer, Viewport,
};

// Re-exported so callers driving prepare()/render() don't depend on glyphon
// directly — they reach the types through this crate.
pub use glyphon;

/// The editor uses a monospace family. `Family::SansSerif` resolves
/// inconsistently across platforms once complex-script fallback kicks in (on
/// macOS the generic sans-serif and the Thai fallback font don't visually
/// match, so Latin runs next to Thai look like a different typeface).
/// `Monospace` gives Latin a stable face; Thai still falls back to a
/// Thai-capable font, but consistently.
const FONT_FAMILY: Family<'static> = Family::Monospace;

/// The default text attributes — one place so `new` and `set_content` always
/// shape with the same font, weight, and style.
fn default_attrs() -> Attrs<'static> {
    Attrs::new().family(FONT_FAMILY)
}

/// GPU-side text resources shared across every `TextStack` in an app.
///
/// Creating these for each stack burns ~50ms/stack of wgpu setup *and*
/// duplicates the glyph atlas — one shared instance lets the editor batch
/// all text into a single `prepare`/`render` per frame.
pub struct TextGpu {
    pub viewport: Viewport,
    pub atlas: TextAtlas,
    pub renderer: TextRenderer,
}

impl TextGpu {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let cache = Cache::new(device);
        let viewport = Viewport::new(device, &cache);
        let mut atlas = TextAtlas::new(device, queue, &cache, format);
        let renderer =
            TextRenderer::new(&mut atlas, device, wgpu::MultisampleState::default(), None);
        Self {
            viewport,
            atlas,
            renderer,
        }
    }
}

/// Owns the shaped cosmic-text `Buffer` for one piece of text.
///
/// Has no GPU resources of its own — drawing happens via [`TextGpu`] and an
/// app-supplied `TextArea` referencing [`TextStack::buffer`].
pub struct TextStack {
    pub buffer: Buffer,
    /// Font size in *logical* points (DIPs). Physical metrics = pt × scale.
    font_size_pt: f32,
    /// Line height in logical points.
    line_height_pt: f32,
    /// Window scale factor the buffer's metrics are currently sized for.
    scale: f32,
}

impl TextStack {
    /// Build the stack and shape `text` into a buffer `width` physical pixels
    /// wide, with metrics derived from `font_size_pt × scale` and
    /// `line_height_pt × scale`.
    ///
    /// The buffer height is left unbounded (`None`): cosmic-text's
    /// `shape_until_scroll` only shapes lines that fit inside `height_opt`, so
    /// a bounded height would silently drop every line past the first
    /// screenful. The caller scrolls and clips the viewport itself. `text` is
    /// shaped with `Shaping::Advanced` so complex scripts (Thai, Arabic,
    /// Devanagari) cluster correctly.
    pub fn new(
        font_system: &mut FontSystem,
        width: f32,
        font_size_pt: f32,
        line_height_pt: f32,
        scale: f32,
        text: &str,
    ) -> Self {
        let metrics = Metrics::new(font_size_pt * scale, line_height_pt * scale);
        let mut buffer = Buffer::new(font_system, metrics);
        buffer.set_size(font_system, Some(width), None);
        let mut stack = Self {
            buffer,
            font_size_pt,
            line_height_pt,
            scale,
        };
        stack.set_content(font_system, text);
        stack
    }

    /// The current line height, in physical pixels — `line_height_pt × scale`.
    pub fn line_height(&self) -> f32 {
        self.line_height_pt * self.scale
    }

    /// Reshape the buffer to `text`, using the stack's standard font attributes
    /// and `Shaping::Advanced`. This is the only path that shapes text — `new`
    /// uses it too — so the font can never drift between the initial render
    /// and a later edit.
    pub fn set_content(&mut self, font_system: &mut FontSystem, text: &str) {
        self.buffer
            .set_text(font_system, text, &default_attrs(), Shaping::Advanced, None);
        self.buffer.shape_until_scroll(font_system, false);
    }

    /// Set the wrap width (physical pixels). Height stays unbounded — see
    /// [`new`](TextStack::new). A non-positive width is ignored.
    pub fn set_width(&mut self, font_system: &mut FontSystem, width: f32) {
        if width <= 0.0 {
            return;
        }
        self.buffer.set_size(font_system, Some(width), None);
    }

    /// Re-size the font metrics for a new window scale factor. No-op if
    /// unchanged.
    pub fn set_scale(&mut self, font_system: &mut FontSystem, scale: f32) {
        if scale == self.scale || scale <= 0.0 {
            return;
        }
        self.scale = scale;
        self.reapply_metrics(font_system);
    }

    /// Re-size the font itself (point size + line height in logical points).
    pub fn set_font_size(
        &mut self,
        font_system: &mut FontSystem,
        font_size_pt: f32,
        line_height_pt: f32,
    ) {
        if font_size_pt <= 0.0 || line_height_pt <= 0.0 {
            return;
        }
        if font_size_pt == self.font_size_pt && line_height_pt == self.line_height_pt {
            return;
        }
        self.font_size_pt = font_size_pt;
        self.line_height_pt = line_height_pt;
        self.reapply_metrics(font_system);
    }

    fn reapply_metrics(&mut self, font_system: &mut FontSystem) {
        let metrics = Metrics::new(
            self.font_size_pt * self.scale,
            self.line_height_pt * self.scale,
        );
        self.buffer.set_metrics(font_system, metrics);
        self.buffer.shape_until_scroll(font_system, false);
    }
}

/// Construct a fresh `FontSystem` — re-exported so apps don't have to depend
/// directly on `glyphon` just to build one to share across stacks.
pub fn new_font_system() -> FontSystem {
    FontSystem::new()
}

/// Construct a fresh `SwashCache` — same reasoning as `new_font_system`.
pub fn new_swash_cache() -> SwashCache {
    SwashCache::new()
}
