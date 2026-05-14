//! A single selection / cursor.

/// A selection: an `anchor` and a `head`, both absolute `char` indices into
/// the buffer.
///
/// The `head` is the moving end (where the caret is); the `anchor` is the
/// fixed end. When `anchor == head` the selection is a bare cursor with no
/// selected text. `head < anchor` is a valid backward selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    /// A bare cursor at `at` — anchor and head coincide.
    pub fn cursor(at: usize) -> Self {
        Self {
            anchor: at,
            head: at,
        }
    }

    /// A selection from `anchor` to `head`. Either order is allowed.
    pub fn new(anchor: usize, head: usize) -> Self {
        Self { anchor, head }
    }

    /// Whether this is a bare cursor (no selected text).
    pub fn is_cursor(&self) -> bool {
        self.anchor == self.head
    }

    /// Alias for [`is_cursor`](Selection::is_cursor) — the selected span is empty.
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// The lower of anchor/head — the start of the selected span.
    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }

    /// The higher of anchor/head — the end of the selected span.
    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }

    /// The selected span as a half-open `char` range.
    pub fn range(&self) -> std::ops::Range<usize> {
        self.start()..self.end()
    }

    /// Length of the selected span in `char`s (0 for a bare cursor).
    pub fn len(&self) -> usize {
        self.end() - self.start()
    }

    /// Shift both ends by a signed `delta`. Saturates at 0.
    ///
    /// Used when an edit elsewhere in the buffer moves this selection's text.
    pub fn shifted(&self, delta: isize) -> Selection {
        let shift = |v: usize| (v as isize + delta).max(0) as usize;
        Selection {
            anchor: shift(self.anchor),
            head: shift(self.head),
        }
    }

    /// Collapse to a bare cursor at `head`, discarding the anchor.
    pub fn collapsed(&self) -> Selection {
        Selection::cursor(self.head)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_is_empty() {
        let c = Selection::cursor(5);
        assert!(c.is_cursor());
        assert!(c.is_empty());
        assert_eq!(c.len(), 0);
        assert_eq!(c.range(), 5..5);
    }

    #[test]
    fn forward_and_backward_selections_have_same_span() {
        let fwd = Selection::new(2, 7);
        let bwd = Selection::new(7, 2);
        assert_eq!(fwd.start(), 2);
        assert_eq!(fwd.end(), 7);
        assert_eq!(bwd.start(), 2);
        assert_eq!(bwd.end(), 7);
        assert_eq!(fwd.range(), bwd.range());
        assert_eq!(fwd.len(), 5);
        assert!(!fwd.is_cursor());
    }

    #[test]
    fn shifted_moves_both_ends() {
        assert_eq!(Selection::new(3, 8).shifted(2), Selection::new(5, 10));
        assert_eq!(Selection::new(3, 8).shifted(-3), Selection::new(0, 5));
    }

    #[test]
    fn shifted_saturates_at_zero() {
        assert_eq!(Selection::new(1, 4).shifted(-10), Selection::new(0, 0));
    }

    #[test]
    fn collapsed_keeps_head() {
        assert_eq!(Selection::new(2, 9).collapsed(), Selection::cursor(9));
        assert_eq!(Selection::new(9, 2).collapsed(), Selection::cursor(2));
    }
}
