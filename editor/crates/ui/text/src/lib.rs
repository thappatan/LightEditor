//! Text rendering stack — cosmic-text shaping + swash raster + glyphon GPU draw.
//!
//! Spec §3.3 (text pipeline). This crate owns the font system, the shaped
//! buffer, and the glyphon objects (atlas, renderer, viewport). Laying out
//! *what* text to draw where is the caller's job.

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

/// Owns everything needed to shape and GPU-render one text buffer.
///
/// Fields are public: until the scene graph lands the caller drives
/// `prepare`/`render` directly against `renderer`, `atlas`, and `viewport`.
pub struct TextStack {
    pub font_system: FontSystem,
    pub swash_cache: SwashCache,
    pub viewport: Viewport,
    pub atlas: TextAtlas,
    pub renderer: TextRenderer,
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
    // A builder would be tidier but every caller passes every argument; a
    // refactor is a follow-up.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        width: f32,
        font_size_pt: f32,
        line_height_pt: f32,
        scale: f32,
        text: &str,
    ) -> Self {
        // Glyphon owns the GPU-side glyph atlas (spec §3.3 step 5).
        let cache = Cache::new(device);
        let viewport = Viewport::new(device, &cache);
        let mut atlas = TextAtlas::new(device, queue, &cache, format);
        let renderer =
            TextRenderer::new(&mut atlas, device, wgpu::MultisampleState::default(), None);

        // cosmic-text handles shape + cluster (spec §3.3 step 2-3).
        let mut font_system = FontSystem::new();
        let swash_cache = SwashCache::new();

        let metrics = Metrics::new(font_size_pt * scale, line_height_pt * scale);
        let mut buffer = Buffer::new(&mut font_system, metrics);
        buffer.set_size(&mut font_system, Some(width), None);

        let mut stack = Self {
            font_system,
            swash_cache,
            viewport,
            atlas,
            renderer,
            buffer,
            font_size_pt,
            line_height_pt,
            scale,
        };
        stack.set_content(text);
        stack
    }

    /// The current line height, in physical pixels — `line_height_pt × scale`.
    /// Callers use this to position carets, highlights, and scroll math so
    /// everything stays in one unit.
    pub fn line_height(&self) -> f32 {
        self.line_height_pt * self.scale
    }

    /// Reshape the buffer to `text`, using the stack's standard font attributes
    /// and `Shaping::Advanced`. This is the only path that shapes text — `new`
    /// uses it too — so the font can never drift between the initial render
    /// and a later edit.
    pub fn set_content(&mut self, text: &str) {
        self.buffer.set_text(
            &mut self.font_system,
            text,
            &default_attrs(),
            Shaping::Advanced,
            None, // default alignment
        );
        self.buffer.shape_until_scroll(&mut self.font_system, false);
    }

    /// Set the wrap width (physical pixels). Height stays unbounded — see
    /// [`new`](TextStack::new). A non-positive width is ignored.
    pub fn set_width(&mut self, width: f32) {
        if width <= 0.0 {
            return;
        }
        self.buffer
            .set_size(&mut self.font_system, Some(width), None);
    }

    /// Re-size the font metrics for a new window scale factor (e.g. the window
    /// moved to a display with different DPI). A no-op if the scale is
    /// unchanged.
    pub fn set_scale(&mut self, scale: f32) {
        if scale == self.scale || scale <= 0.0 {
            return;
        }
        self.scale = scale;
        self.reapply_metrics();
    }

    /// Re-size the font itself (point size + line height in logical points).
    /// Used to pick up settings changes.
    pub fn set_font_size(&mut self, font_size_pt: f32, line_height_pt: f32) {
        if font_size_pt <= 0.0 || line_height_pt <= 0.0 {
            return;
        }
        if font_size_pt == self.font_size_pt && line_height_pt == self.line_height_pt {
            return;
        }
        self.font_size_pt = font_size_pt;
        self.line_height_pt = line_height_pt;
        self.reapply_metrics();
    }

    /// Push the current `(font_size_pt, line_height_pt, scale)` triple into
    /// the cosmic-text buffer and reshape.
    fn reapply_metrics(&mut self) {
        let metrics = Metrics::new(
            self.font_size_pt * self.scale,
            self.line_height_pt * self.scale,
        );
        self.buffer.set_metrics(&mut self.font_system, metrics);
        self.buffer.shape_until_scroll(&mut self.font_system, false);
    }
}
