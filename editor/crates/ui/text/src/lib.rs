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
    /// Build the stack and shape `text` into a buffer sized `width` x `height`
    /// (in physical pixels). `text` is shaped with `Shaping::Advanced` so
    /// complex scripts (Thai, Arabic, Devanagari) cluster correctly.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        width: f32,
        height: f32,
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
        buffer.set_size(&mut font_system, Some(width), Some(height));
        buffer.set_text(
            &mut font_system,
            text,
            &Attrs::new().family(Family::SansSerif),
            Shaping::Advanced,
            None, // default alignment
        );
        buffer.shape_until_scroll(&mut font_system, false);

        Self {
            font_system,
            swash_cache,
            viewport,
            atlas,
            renderer,
            buffer,
        }
    }

    /// Resize the shaped buffer to a new area (physical pixels). A zero
    /// dimension is ignored.
    pub fn set_size(&mut self, width: f32, height: f32) {
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        self.buffer
            .set_size(&mut self.font_system, Some(width), Some(height));
    }
}
