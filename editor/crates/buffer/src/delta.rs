//! Edit-delta type that downstream incremental parsers (notably tree-sitter)
//! need to update a cached parse tree without re-parsing the whole buffer.
//!
//! Everything is in bytes — `tree_sitter::InputEdit` works in bytes, and so
//! does its `Point` (row + byte column). Keeping the editor-core / editor-
//! syntax interface tied to bytes avoids per-edit char→byte conversions in
//! the hot path.

/// A (row, byte-column) point inside the buffer. The byte column is the
/// offset from the start of `row` in UTF-8 bytes — matches tree-sitter's
/// `Point` exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BytePoint {
    pub row: usize,
    pub column: usize,
}

/// One contiguous edit to the buffer, captured at the moment it happened so
/// downstream can replay it onto a cached parse tree.
///
/// The three byte offsets line up with `tree_sitter::InputEdit`: the edit
/// replaces bytes `start_byte..old_end_byte` (in the buffer *before* the
/// edit) with bytes `start_byte..new_end_byte` (after the edit). For a pure
/// insertion `old_end_byte == start_byte`; for a pure deletion
/// `new_end_byte == start_byte`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferDelta {
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    pub start_point: BytePoint,
    pub old_end_point: BytePoint,
    pub new_end_point: BytePoint,
}
