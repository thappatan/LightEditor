//! Text rendering stack — cosmic-text shaping + swash raster + glyphon GPU draw.
//!
//! Spec §3.3 (text pipeline). One `TextStack` shapes a single piece of text
//! into a cosmic-text `Buffer`; the GPU side (atlas, renderer, viewport) is
//! shared across stacks via [`TextGpu`] so the app pays the wgpu setup cost
//! once and submits all text in a single `prepare` + `render` per frame.

use glyphon::{
    cosmic_text::LineEnding, Attrs, AttrsList, Buffer, BufferLine, Cache, Family, FontSystem,
    Metrics, Shaping, SwashCache, TextAtlas, TextRenderer, Viewport,
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

    /// Logical font size (points / DIP). Multiply by the current window scale
    /// for the physical-pixel size.
    pub fn font_size_pt(&self) -> f32 {
        self.font_size_pt
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

    /// Reshape the buffer to a sequence of attributed `(slice, attrs)` spans.
    /// Used by the syntax-highlighter path so per-token colors land in the
    /// shaped glyphs. Concatenating every span's slice MUST equal the full
    /// document text — the caller is responsible for that.
    ///
    /// This path diff-updates the underlying `BufferLine`s using a
    /// longest-common-prefix + longest-common-suffix match. Only the
    /// middle range — the lines that actually changed — gets rebuilt;
    /// everything else keeps its shape cache and `shape_until_scroll`
    /// re-shapes only the dirty lines.
    ///
    /// This is what keeps Enter cheap on a long file: positional
    /// `BufferLine::set_text` would reshape every line after the cursor
    /// (because their indices all shift down by one), but a suffix match
    /// notices the trailing lines are identical content at a higher
    /// index and a single `Vec::splice` does the structural insert
    /// without touching them.
    ///
    /// On a 4000-line buffer:
    /// - one-char edit inside a line → 1 reshape
    /// - Enter at line N → 2 reshapes (the split line becomes two)
    /// - deleting a line → 1 reshape (the surviving merged line)
    pub fn set_content_rich<'a, I>(&mut self, font_system: &mut FontSystem, spans: I)
    where
        I: IntoIterator<Item = (&'a str, Attrs<'a>)>,
    {
        let default = default_attrs();
        // Walk the span stream once, accumulating per-line entries.
        // Building the full list up front lets the prefix/suffix scan
        // compare against the existing `buffer.lines` without re-walking
        // the spans.
        let mut new_lines: Vec<NewLine> = Vec::with_capacity(self.buffer.lines.len() + 1);
        let mut line_text = String::new();
        let mut line_attrs = AttrsList::new(&default);

        for (slice, attrs) in spans {
            let mut remainder = slice;
            while let Some(nl) = remainder.find('\n') {
                let (head, rest) = remainder.split_at(nl);
                append_span(&mut line_text, &mut line_attrs, head, &attrs, &default);
                let ending = if line_text.ends_with('\r') {
                    line_text.pop();
                    LineEnding::CrLf
                } else {
                    LineEnding::Lf
                };
                new_lines.push(NewLine {
                    text: std::mem::take(&mut line_text),
                    ending,
                    attrs: std::mem::replace(&mut line_attrs, AttrsList::new(&default)),
                });
                remainder = &rest[1..]; // skip the '\n'
            }
            if !remainder.is_empty() {
                append_span(&mut line_text, &mut line_attrs, remainder, &attrs, &default);
            }
        }
        // The trailing line (no terminating newline) — always emitted,
        // even when empty, so a doc ending in '\n' keeps cosmic-text's
        // "one empty line follows" invariant.
        new_lines.push(NewLine {
            text: line_text,
            ending: LineEnding::None,
            attrs: line_attrs,
        });

        diff_apply(&mut self.buffer.lines, new_lines);
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

/// One line ready to slot into `buffer.lines` — owned text + attrs +
/// line-ending choice. Built up by walking the caller's span stream and
/// then compared against the existing `BufferLine`s for diffing.
struct NewLine {
    text: String,
    ending: LineEnding,
    attrs: AttrsList,
}

impl NewLine {
    /// Does this line match an existing `BufferLine` byte-for-byte?
    fn matches(&self, line: &BufferLine) -> bool {
        line.text() == self.text && line.ending() == self.ending && line.attrs_list() == &self.attrs
    }

    fn into_buffer_line(self) -> BufferLine {
        BufferLine::new(self.text, self.ending, self.attrs, Shaping::Advanced)
    }
}

/// Append `slice` to the in-progress line, recording an attrs span when
/// the slice's attrs differ from the line's default.
fn append_span(
    line_text: &mut String,
    line_attrs: &mut AttrsList,
    slice: &str,
    attrs: &Attrs<'_>,
    default: &Attrs<'_>,
) {
    if slice.is_empty() {
        return;
    }
    let start = line_text.len();
    line_text.push_str(slice);
    if attrs != default {
        line_attrs.add_span(start..line_text.len(), attrs);
    }
}

/// Replace `old` with the smallest `Vec::splice` that turns it into the
/// content of `new`. The prefix + suffix scans find the largest matching
/// regions at both ends; the middle range is what actually gets shaped on
/// the next `shape_until_scroll` call.
fn diff_apply(old: &mut Vec<BufferLine>, mut new: Vec<NewLine>) {
    let mut prefix = 0;
    let prefix_max = old.len().min(new.len());
    while prefix < prefix_max && new[prefix].matches(&old[prefix]) {
        prefix += 1;
    }
    let mut suffix = 0;
    let suffix_max = old.len().min(new.len()) - prefix;
    while suffix < suffix_max && new[new.len() - 1 - suffix].matches(&old[old.len() - 1 - suffix]) {
        suffix += 1;
    }
    let old_end = old.len() - suffix;
    let new_end = new.len() - suffix;
    let replacement: Vec<BufferLine> = new
        .drain(prefix..new_end)
        .map(NewLine::into_buffer_line)
        .collect();
    old.splice(prefix..old_end, replacement);
}
