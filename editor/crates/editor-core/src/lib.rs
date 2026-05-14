//! Editing operations for the editor (spec §4.1.1).
//!
//! Builds on `editor-buffer`: this crate adds cursors, multi-cursor selection
//! sets, grapheme-cluster-aware movement, and tree-based undo/redo. It is pure
//! logic — no GUI, no async — so it is exhaustively unit-tested.
//!
//! The top-level type is [`Editor`]: a buffer, its [`SelectionSet`], and an
//! [`UndoTree`], with the edit and movement methods that keep them in sync.

mod editor;
mod selection;
mod selection_set;
mod undo;

pub use editor::Editor;
pub use selection::Selection;
pub use selection_set::SelectionSet;
pub use undo::UndoTree;

// Re-exported: callers placing selections from pixel coordinates (mouse input)
// need `Position` to talk to the buffer.
pub use editor_buffer::Position;
