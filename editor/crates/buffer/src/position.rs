//! Buffer positions.

/// A position in a text buffer, as a zero-based line and column.
///
/// `column` counts `char`s (Unicode scalar values) from the start of the
/// line — not bytes, and not grapheme clusters. Grapheme-cluster-aware cursor
/// movement is editor-core's job; the buffer works in `char`s because that is
/// ropey's native unit.
///
/// Ordering is lexicographic (line, then column), which matches reading order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct Position {
    pub line: usize,
    pub column: usize,
}

impl Position {
    /// The start of the buffer — line 0, column 0.
    pub const ZERO: Position = Position { line: 0, column: 0 };

    pub fn new(line: usize, column: usize) -> Self {
        Self { line, column }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_is_lexicographic() {
        assert!(Position::new(0, 5) < Position::new(1, 0));
        assert!(Position::new(1, 2) < Position::new(1, 3));
        assert_eq!(Position::new(2, 4), Position::new(2, 4));
        assert!(Position::ZERO < Position::new(0, 1));
    }

    #[test]
    fn zero_is_default() {
        assert_eq!(Position::default(), Position::ZERO);
    }
}
