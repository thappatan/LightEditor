//! The text buffer itself.

use std::fmt;
use std::ops::Range;

use ropey::Rope;

use crate::{BytePoint, LineEnding, Position};

/// A text buffer backed by a `ropey::Rope` (ADR-004).
///
/// Indices and offsets are in `char`s (Unicode scalar values) unless a method
/// name says `_byte`. The buffer is pure text storage plus edit primitives —
/// cursors, selections, undo/redo, and grapheme-cluster movement live in
/// editor-core.
///
/// Edit methods (`insert`, `remove`, `replace`) panic on out-of-bounds indices,
/// matching ropey. Callers are expected to clamp positions first; the
/// `position_to_char` / `char_to_position` conversions never produce an
/// out-of-bounds `char` index for this buffer.
#[derive(Debug, Clone)]
pub struct TextBuffer {
    rope: Rope,
    line_ending: LineEnding,
}

impl TextBuffer {
    /// An empty buffer with the default (LF) line ending.
    pub fn new() -> Self {
        Self {
            rope: Rope::new(),
            line_ending: LineEnding::default(),
        }
    }

    /// The buffer's line-ending convention, detected on construction from a
    /// string (see the `From` impls) or [`LineEnding::default`] for [`new`].
    ///
    /// [`new`]: TextBuffer::new
    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    // ── metrics ───────────────────────────────────────────────────────────

    /// Total length in bytes.
    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    /// Total length in `char`s.
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    /// Number of lines. A buffer always has at least one line, even when empty.
    /// A trailing newline does *not* add an extra empty line beyond ropey's
    /// own convention — `len_lines` mirrors `Rope::len_lines`.
    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    /// Whether the buffer contains no text.
    pub fn is_empty(&self) -> bool {
        self.rope.len_chars() == 0
    }

    // ── access ────────────────────────────────────────────────────────────

    /// The text of `line` (zero-based), including its trailing newline if any.
    /// `None` if `line` is out of range.
    pub fn line(&self, line: usize) -> Option<String> {
        (line < self.rope.len_lines()).then(|| self.rope.line(line).to_string())
    }

    /// Number of `char`s in `line`, including a trailing newline if present.
    /// `None` if `line` is out of range.
    pub fn line_len_chars(&self, line: usize) -> Option<usize> {
        (line < self.rope.len_lines()).then(|| self.rope.line(line).len_chars())
    }

    /// The text in `range` (a `char` range) as a `String`.
    ///
    /// Panics if the range is out of bounds.
    pub fn slice(&self, range: Range<usize>) -> String {
        self.rope.slice(range).to_string()
    }

    // ── conversions ───────────────────────────────────────────────────────

    /// Convert a [`Position`] to an absolute `char` index.
    ///
    /// `None` if `pos.line` is out of range, or `pos.column` is past the end
    /// of that line (including its newline).
    pub fn position_to_char(&self, pos: Position) -> Option<usize> {
        if pos.line >= self.rope.len_lines() {
            return None;
        }
        let line_start = self.rope.line_to_char(pos.line);
        let line_len = self.rope.line(pos.line).len_chars();
        (pos.column <= line_len).then_some(line_start + pos.column)
    }

    /// Convert an absolute `char` index to a [`Position`].
    ///
    /// Saturates at the end of the buffer if `char_idx` is past it.
    pub fn char_to_position(&self, char_idx: usize) -> Position {
        let char_idx = char_idx.min(self.rope.len_chars());
        let line = self.rope.char_to_line(char_idx);
        let line_start = self.rope.line_to_char(line);
        Position {
            line,
            column: char_idx - line_start,
        }
    }

    /// Convert a `char` index to its UTF-8 byte offset. Panics if `char_idx`
    /// is past the end of the buffer.
    pub fn char_to_byte(&self, char_idx: usize) -> usize {
        self.rope.char_to_byte(char_idx)
    }

    /// `char_idx` as a (row, byte-column) point — matches tree-sitter's
    /// `Point` convention so downstream parsers can use it directly. Past
    /// the end of the buffer returns the end-of-buffer point.
    pub fn byte_point(&self, char_idx: usize) -> BytePoint {
        let char_idx = char_idx.min(self.rope.len_chars());
        let row = self.rope.char_to_line(char_idx);
        let line_byte_start = self.rope.line_to_byte(row);
        let byte = self.rope.char_to_byte(char_idx);
        BytePoint {
            row,
            column: byte - line_byte_start,
        }
    }

    // ── edit ──────────────────────────────────────────────────────────────

    /// Insert `text` at `char_idx`.
    ///
    /// Panics if `char_idx > len_chars()`.
    pub fn insert(&mut self, char_idx: usize, text: &str) {
        self.rope.insert(char_idx, text);
    }

    /// Remove the `char`s in `range`.
    ///
    /// Panics if the range is out of bounds or `start > end`.
    pub fn remove(&mut self, range: Range<usize>) {
        self.rope.remove(range);
    }

    /// Replace the `char`s in `range` with `text`.
    ///
    /// Panics if the range is out of bounds or `start > end`.
    pub fn replace(&mut self, range: Range<usize>, text: &str) {
        let start = range.start;
        self.rope.remove(range);
        self.rope.insert(start, text);
    }
}

impl Default for TextBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl From<&str> for TextBuffer {
    fn from(text: &str) -> Self {
        Self {
            rope: Rope::from_str(text),
            line_ending: LineEnding::detect(text),
        }
    }
}

impl From<String> for TextBuffer {
    fn from(text: String) -> Self {
        TextBuffer::from(text.as_str())
    }
}

impl fmt::Display for TextBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // ropey's Rope writes itself chunk by chunk — no full materialization.
        write!(f, "{}", self.rope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_buffer_is_empty_with_one_line() {
        let buf = TextBuffer::new();
        assert!(buf.is_empty());
        assert_eq!(buf.len_chars(), 0);
        assert_eq!(buf.len_bytes(), 0);
        // ropey: an empty rope still has one (empty) line.
        assert_eq!(buf.len_lines(), 1);
        assert_eq!(buf.line_ending(), LineEnding::Lf);
    }

    #[test]
    fn from_str_detects_line_ending() {
        assert_eq!(TextBuffer::from("a\nb").line_ending(), LineEnding::Lf);
        assert_eq!(TextBuffer::from("a\r\nb").line_ending(), LineEnding::CrLf);
        assert_eq!(
            TextBuffer::from(String::from("x\r\ny")).line_ending(),
            LineEnding::CrLf
        );
    }

    #[test]
    fn display_round_trips() {
        let text = "first\nsecond\nthird";
        assert_eq!(TextBuffer::from(text).to_string(), text);
    }

    #[test]
    fn metrics_count_chars_not_bytes() {
        // "สวัสดี" is 6 chars but many more bytes; "🚀" is 1 char, 4 bytes.
        let buf = TextBuffer::from("สวัสดี🚀");
        assert_eq!(buf.len_chars(), 7);
        assert!(buf.len_bytes() > buf.len_chars());
        assert!(!buf.is_empty());
    }

    #[test]
    fn line_access() {
        let buf = TextBuffer::from("alpha\nbeta\ngamma");
        assert_eq!(buf.line(0).as_deref(), Some("alpha\n"));
        assert_eq!(buf.line(1).as_deref(), Some("beta\n"));
        assert_eq!(buf.line(2).as_deref(), Some("gamma"));
        assert_eq!(buf.line(3), None);
        assert_eq!(buf.line_len_chars(0), Some(6)); // "alpha\n"
        assert_eq!(buf.line_len_chars(2), Some(5)); // "gamma"
        assert_eq!(buf.line_len_chars(99), None);
    }

    #[test]
    fn insert_at_boundaries() {
        let mut buf = TextBuffer::from("world");
        buf.insert(0, "hello ");
        assert_eq!(buf.to_string(), "hello world");
        buf.insert(buf.len_chars(), "!");
        assert_eq!(buf.to_string(), "hello world!");
    }

    #[test]
    fn remove_range() {
        let mut buf = TextBuffer::from("hello world");
        buf.remove(5..11); // " world"
        assert_eq!(buf.to_string(), "hello");
        buf.remove(0..buf.len_chars());
        assert!(buf.is_empty());
    }

    #[test]
    fn replace_range() {
        let mut buf = TextBuffer::from("the quick fox");
        buf.replace(4..9, "slow"); // "quick" -> "slow"
        assert_eq!(buf.to_string(), "the slow fox");
        // replace with empty == remove
        buf.replace(3..8, "");
        assert_eq!(buf.to_string(), "the fox");
        // replace empty range == insert
        buf.replace(0..0, "see ");
        assert_eq!(buf.to_string(), "see the fox");
    }

    #[test]
    fn edits_on_multibyte_text() {
        // Thai clusters: indexing is by char, so an edit at a char boundary
        // inside a cluster is still a valid char index even if visually mid-glyph.
        let mut buf = TextBuffer::from("สวัสดีชาวโลก");
        let original_chars = buf.len_chars();
        buf.insert(0, "👋 ");
        assert_eq!(buf.len_chars(), original_chars + 2);
        assert!(buf.to_string().starts_with("👋 ส"));
    }

    #[test]
    fn position_to_char_round_trip() {
        let buf = TextBuffer::from("alpha\nbeta\ngamma");
        for (line, column) in [(0, 0), (0, 5), (1, 0), (1, 4), (2, 0), (2, 5)] {
            let pos = Position::new(line, column);
            let idx = buf.position_to_char(pos).expect("in range");
            assert_eq!(buf.char_to_position(idx), pos, "round trip for {pos:?}");
        }
    }

    #[test]
    fn position_to_char_out_of_range() {
        let buf = TextBuffer::from("alpha\nbeta");
        // line out of range
        assert_eq!(buf.position_to_char(Position::new(5, 0)), None);
        // column past end of line ("alpha\n" is 6 chars, col 7 is invalid)
        assert_eq!(buf.position_to_char(Position::new(0, 7)), None);
        // column exactly at line length (on the newline) is valid
        assert_eq!(buf.position_to_char(Position::new(0, 6)), Some(6));
    }

    #[test]
    fn char_to_position_saturates_past_end() {
        let buf = TextBuffer::from("ab\ncd");
        let end = buf.char_to_position(buf.len_chars());
        // past-the-end clamps to the last valid position
        assert_eq!(buf.char_to_position(9999), end);
        assert_eq!(end, Position::new(1, 2));
    }

    #[test]
    fn char_to_position_on_empty_buffer() {
        let buf = TextBuffer::new();
        assert_eq!(buf.char_to_position(0), Position::ZERO);
        assert_eq!(buf.char_to_position(100), Position::ZERO);
    }

    #[test]
    fn char_to_byte_counts_utf8_bytes() {
        let buf = TextBuffer::from("aก๋b");
        // "a" — 1 byte, "ก" — 3 bytes, "๋" — 3 bytes, "b" — 1 byte.
        assert_eq!(buf.char_to_byte(0), 0);
        assert_eq!(buf.char_to_byte(1), 1);
        assert_eq!(buf.char_to_byte(2), 4);
        assert_eq!(buf.char_to_byte(3), 7);
        assert_eq!(buf.char_to_byte(4), 8);
        assert_eq!(buf.char_to_byte(buf.len_chars()), buf.len_bytes());
    }

    #[test]
    fn byte_point_reports_row_and_byte_column() {
        let buf = TextBuffer::from("alpha\nกข\nz");
        // "alpha\n" — first line, then "กข\n" — second.
        assert_eq!(buf.byte_point(0), BytePoint { row: 0, column: 0 });
        assert_eq!(buf.byte_point(5), BytePoint { row: 0, column: 5 });
        // Start of line 1 (right after the newline).
        assert_eq!(buf.byte_point(6), BytePoint { row: 1, column: 0 });
        // After "ก" — that's 3 bytes.
        assert_eq!(buf.byte_point(7), BytePoint { row: 1, column: 3 });
        // End of buffer.
        let end = buf.byte_point(buf.len_chars());
        assert_eq!(end.row, 2);
    }

    #[test]
    fn slice_extracts_text() {
        let buf = TextBuffer::from("hello world");
        assert_eq!(buf.slice(0..5), "hello");
        assert_eq!(buf.slice(6..11), "world");
        assert_eq!(buf.slice(0..buf.len_chars()), "hello world");
        assert_eq!(buf.slice(3..3), "");
    }
}
