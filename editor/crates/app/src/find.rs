//! Find-in-buffer (spec §4.1.3) — the search bar that highlights every
//! literal match of a query and lets the caller jump between them.
//!
//! Pure logic: the query string, the collected match ranges, and the current
//! cursor into that list. The app wires this to keyboard input, the editor
//! selection, and the overlay rendering.
//!
//! Regex / case-insensitive / whole-word are deliberate follow-ups — this
//! pass does literal substring search only.

use std::ops::Range;

/// State of the find bar.
pub struct FindBar {
    query: String,
    /// Char-index ranges (half-open) of every literal match against the
    /// buffer text the bar was last updated against. In buffer order.
    matches: Vec<Range<usize>>,
    /// Index into `matches` of the currently-highlighted hit, or 0 when
    /// `matches` is empty.
    current: usize,
}

impl FindBar {
    /// A find bar with no query, no matches.
    pub fn new() -> Self {
        Self {
            query: String::new(),
            matches: Vec::new(),
            current: 0,
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// Every match, in buffer order.
    pub fn matches(&self) -> &[Range<usize>] {
        &self.matches
    }

    /// The range of the currently-selected match, or `None` if there are no
    /// matches.
    pub fn current_match(&self) -> Option<Range<usize>> {
        self.matches.get(self.current).cloned()
    }

    /// Zero-based index of the currently-selected match (0 when there are
    /// no matches).
    pub fn current_index(&self) -> usize {
        self.current
    }

    /// Total number of matches.
    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// Append `c` to the query and recompute matches against `text`.
    pub fn push_char(&mut self, c: char, text: &str) {
        self.query.push(c);
        self.recompute(text);
    }

    /// Drop the last char of the query (no-op if empty) and recompute.
    pub fn backspace(&mut self, text: &str) {
        if self.query.pop().is_some() {
            self.recompute(text);
        }
    }

    /// Move to the next match, wrapping at the end. No-op without matches.
    pub fn next_match(&mut self) {
        if !self.matches.is_empty() {
            self.current = (self.current + 1) % self.matches.len();
        }
    }

    /// Move to the previous match, wrapping at the start.
    pub fn prev_match(&mut self) {
        if let Some(last) = self.matches.len().checked_sub(1) {
            self.current = if self.current == 0 {
                last
            } else {
                self.current - 1
            };
        }
    }

    /// Re-scan `text` for the current query, replacing the match list.
    /// Caller invokes this whenever the buffer's text changes.
    pub fn refresh(&mut self, text: &str) {
        self.recompute(text);
    }

    /// Walk `text` and collect non-overlapping matches of `self.query` as
    /// half-open char ranges. Resets `current` to 0.
    fn recompute(&mut self, text: &str) {
        self.matches.clear();
        self.current = 0;
        if self.query.is_empty() {
            return;
        }
        // Char-based scan so the recorded ranges are char indices, matching
        // editor-core's `Selection` model. `chars().collect()` is O(n) for
        // the buffer; for a real editor this would shape against the rope
        // directly, but it's fine for M1.
        let haystack: Vec<char> = text.chars().collect();
        let needle: Vec<char> = self.query.chars().collect();
        if needle.is_empty() || needle.len() > haystack.len() {
            return;
        }
        let mut i = 0;
        while i + needle.len() <= haystack.len() {
            if haystack[i..i + needle.len()] == needle[..] {
                self.matches.push(i..i + needle.len());
                i += needle.len(); // non-overlapping
            } else {
                i += 1;
            }
        }
    }
}

impl Default for FindBar {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(f: &FindBar) -> Vec<Range<usize>> {
        f.matches().to_vec()
    }

    #[test]
    fn new_has_no_matches() {
        let f = FindBar::new();
        assert_eq!(f.match_count(), 0);
        assert!(f.current_match().is_none());
    }

    #[test]
    fn empty_query_clears_matches() {
        let mut f = FindBar::new();
        f.push_char('a', "abc abc");
        assert!(f.match_count() > 0);
        f.backspace("abc abc");
        // back to empty query → no matches
        assert_eq!(f.match_count(), 0);
    }

    #[test]
    fn collects_every_non_overlapping_match() {
        let mut f = FindBar::new();
        f.push_char('a', "ababab");
        // 'a' appears at chars 0, 2, 4
        assert_eq!(ranges(&f), vec![0..1, 2..3, 4..5]);
        f.push_char('b', "ababab"); // query "ab"
        assert_eq!(ranges(&f), vec![0..2, 2..4, 4..6]);
    }

    #[test]
    fn ranges_are_char_indices_not_bytes() {
        // "สวัสดี" is 6 chars but several more bytes; the match for "วั" is
        // chars 1..3, not bytes.
        let text = "สวัสดี";
        let mut f = FindBar::new();
        for c in "วั".chars() {
            f.push_char(c, text);
        }
        assert_eq!(ranges(&f), vec![1..3]);
    }

    #[test]
    fn next_prev_wrap_around() {
        let mut f = FindBar::new();
        f.push_char('a', "a a a"); // 3 matches at chars 0, 2, 4
        assert_eq!(f.current_index(), 0);
        f.next_match();
        f.next_match();
        assert_eq!(f.current_index(), 2);
        f.next_match(); // wraps
        assert_eq!(f.current_index(), 0);
        f.prev_match(); // wraps backwards
        assert_eq!(f.current_index(), 2);
    }

    #[test]
    fn navigation_on_empty_matches_is_a_noop() {
        let mut f = FindBar::new();
        f.push_char('z', "abc");
        assert_eq!(f.match_count(), 0);
        f.next_match();
        f.prev_match();
        assert_eq!(f.current_index(), 0);
        assert!(f.current_match().is_none());
    }

    #[test]
    fn refresh_picks_up_buffer_changes() {
        let mut f = FindBar::new();
        f.push_char('x', "no match here");
        assert_eq!(f.match_count(), 0);
        f.refresh("xxx");
        assert_eq!(f.match_count(), 3);
    }
}
