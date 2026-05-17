//! Text buffer for the editor (spec §4.1.1, ADR-004).
//!
//! A thin, well-tested layer over [`ropey`]. This crate is pure text storage
//! plus edit primitives and position arithmetic — it has no concept of
//! cursors, selections, undo/redo, or grapheme clusters. Those belong to
//! `editor-core`, which builds on top of this.
//!
//! Everything works in `char`s (Unicode scalar values), ropey's native unit,
//! except where a name says `_byte`.

mod buffer;
mod delta;
mod line_ending;
mod position;

pub use buffer::TextBuffer;
pub use delta::{BufferDelta, BytePoint};
pub use line_ending::LineEnding;
pub use position::Position;
