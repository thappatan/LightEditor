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

/// Body text size and line height, in points. Readable on a ~720p window.
const FONT_SIZE: f32 = 24.0;
const LINE_HEIGHT: f32 = 32.0;

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
}

impl TextStack {
    /// Build the stack and shape `text` into a buffer `width` pixels wide.
    ///
    /// The buffer height is left unbounded (`None`): cosmic-text's
    /// `shape_until_scroll` only shapes lines that fit inside `height_opt`, so
    /// a bounded height would silently drop every line past the first
    /// screenful. The caller scrolls and clips the viewport itself. `text` is
    /// shaped with `Shaping::Advanced` so complex scripts (Thai, Arabic,
    /// Devanagari) cluster correctly.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        width: f32,
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

        let mut buffer = Buffer::new(&mut font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
        buffer.set_size(&mut font_system, Some(width), None);

        let mut stack = Self {
            font_system,
            swash_cache,
            viewport,
            atlas,
            renderer,
            buffer,
        };
        stack.set_content(text);
        stack
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
}
