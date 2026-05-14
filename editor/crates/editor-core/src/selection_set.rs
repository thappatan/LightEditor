//! The set of active selections — multi-cursor support (spec §4.1.1).

use crate::Selection;

/// All active selections in a buffer.
///
/// Invariants, upheld by every constructor and mutator:
/// - **non-empty** — there is always at least one selection;
/// - **sorted** — selections are ordered by [`Selection::start`];
/// - **non-overlapping** — overlapping or touching selections are merged.
///
/// One selection is the *primary*: the one the viewport follows and the status
/// bar reports. After a merge the primary is whichever resulting selection
/// covers the old primary's `head`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionSet {
    selections: Vec<Selection>,
    primary: usize,
}

impl SelectionSet {
    /// A set with a single selection.
    pub fn single(selection: Selection) -> Self {
        Self {
            selections: vec![selection],
            primary: 0,
        }
    }

    /// Build a normalized set from `selections`, with the primary being
    /// whichever resulting selection covers `primary_head`.
    ///
    /// An empty input falls back to a single cursor at 0 (the non-empty
    /// invariant is never violated).
    pub fn new(mut selections: Vec<Selection>, primary_head: usize) -> Self {
        if selections.is_empty() {
            return Self::single(Selection::cursor(0));
        }
        selections.sort_by_key(Selection::start);

        let mut merged: Vec<Selection> = Vec::with_capacity(selections.len());
        for sel in selections {
            match merged.last_mut() {
                // Overlapping or touching — merge into one forward selection.
                Some(last) if sel.start() <= last.end() => {
                    let start = last.start();
                    let end = last.end().max(sel.end());
                    *last = Selection::new(start, end);
                }
                _ => merged.push(sel),
            }
        }

        let primary = merged
            .iter()
            .position(|s| s.start() <= primary_head && primary_head <= s.end())
            .unwrap_or(0);

        Self {
            selections: merged,
            primary,
        }
    }

    /// The primary selection.
    pub fn primary(&self) -> Selection {
        self.selections[self.primary]
    }

    /// Index of the primary selection within [`selections`](SelectionSet::selections).
    pub fn primary_index(&self) -> usize {
        self.primary
    }

    /// All selections, sorted by start.
    pub fn selections(&self) -> &[Selection] {
        &self.selections
    }

    /// Iterate the selections, sorted by start.
    pub fn iter(&self) -> std::slice::Iter<'_, Selection> {
        self.selections.iter()
    }

    /// Number of selections (always ≥ 1).
    pub fn len(&self) -> usize {
        self.selections.len()
    }

    /// Always `false` — kept so the type reads naturally; the non-empty
    /// invariant guarantees it.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Whether more than one selection is active.
    pub fn has_multiple(&self) -> bool {
        self.selections.len() > 1
    }

    /// Replace the whole set with one selection.
    pub fn replace_all(&mut self, selection: Selection) {
        *self = Self::single(selection);
    }

    /// Collapse to just the primary selection.
    pub fn collapse_to_primary(&mut self) {
        *self = Self::single(self.primary());
    }

    /// Add another selection, re-normalizing (sort + merge). The primary is
    /// preserved by its `head`.
    pub fn push(&mut self, selection: Selection) {
        let primary_head = self.primary().head;
        let mut all = self.selections.clone();
        all.push(selection);
        *self = Self::new(all, primary_head);
    }

    /// Apply `f` to every selection, then re-normalize. The primary tracks the
    /// `head` of `f` applied to the old primary — so movement and edits keep
    /// the viewport on the right cursor.
    pub fn map(&mut self, f: impl Fn(Selection) -> Selection) {
        let new_primary_head = f(self.primary()).head;
        let mapped: Vec<Selection> = self.selections.iter().copied().map(&f).collect();
        *self = Self::new(mapped, new_primary_head);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heads(set: &SelectionSet) -> Vec<(usize, usize)> {
        set.iter().map(|s| (s.start(), s.end())).collect()
    }

    #[test]
    fn single_has_one_selection() {
        let set = SelectionSet::single(Selection::cursor(3));
        assert_eq!(set.len(), 1);
        assert!(!set.has_multiple());
        assert_eq!(set.primary(), Selection::cursor(3));
    }

    #[test]
    fn new_sorts_by_start() {
        let set = SelectionSet::new(
            vec![
                Selection::cursor(9),
                Selection::cursor(1),
                Selection::cursor(5),
            ],
            1,
        );
        assert_eq!(heads(&set), [(1, 1), (5, 5), (9, 9)]);
    }

    #[test]
    fn overlapping_selections_merge() {
        let set = SelectionSet::new(vec![Selection::new(2, 6), Selection::new(4, 9)], 4);
        assert_eq!(heads(&set), [(2, 9)]);
    }

    #[test]
    fn touching_selections_merge() {
        // 2..5 and 5..8 touch at 5 — merge into 2..8
        let set = SelectionSet::new(vec![Selection::new(2, 5), Selection::new(5, 8)], 5);
        assert_eq!(heads(&set), [(2, 8)]);
    }

    #[test]
    fn duplicate_cursors_merge() {
        let set = SelectionSet::new(vec![Selection::cursor(4), Selection::cursor(4)], 4);
        assert_eq!(set.len(), 1);
        assert_eq!(set.primary(), Selection::cursor(4));
    }

    #[test]
    fn distinct_cursors_stay_separate() {
        let set = SelectionSet::new(vec![Selection::cursor(1), Selection::cursor(3)], 3);
        assert_eq!(set.len(), 2);
        assert_eq!(set.primary(), Selection::cursor(3));
    }

    #[test]
    fn primary_follows_into_a_merge() {
        // primary was the cursor at 7; it merges into 5..9 — primary should be
        // that merged selection.
        let set = SelectionSet::new(vec![Selection::new(5, 9), Selection::cursor(7)], 7);
        assert_eq!(set.len(), 1);
        assert_eq!(set.primary(), Selection::new(5, 9));
    }

    #[test]
    fn push_renormalizes() {
        let mut set = SelectionSet::single(Selection::cursor(2));
        set.push(Selection::cursor(8));
        set.push(Selection::cursor(5));
        assert_eq!(heads(&set), [(2, 2), (5, 5), (8, 8)]);
        // pushing an overlap merges
        set.push(Selection::new(1, 6));
        assert_eq!(heads(&set), [(1, 6), (8, 8)]);
    }

    #[test]
    fn map_applies_and_renormalizes() {
        let mut set = SelectionSet::new(vec![Selection::cursor(2), Selection::cursor(5)], 5);
        // shift both right by 3 — still distinct
        set.map(|s| s.shifted(3));
        assert_eq!(heads(&set), [(5, 5), (8, 8)]);
        // collapse both to the same point — they merge
        set.map(|_| Selection::cursor(0));
        assert_eq!(set.len(), 1);
        assert_eq!(set.primary(), Selection::cursor(0));
    }

    #[test]
    fn collapse_to_primary_drops_the_rest() {
        let mut set = SelectionSet::new(
            vec![
                Selection::cursor(1),
                Selection::cursor(4),
                Selection::cursor(9),
            ],
            4,
        );
        set.collapse_to_primary();
        assert_eq!(set.len(), 1);
        assert_eq!(set.primary(), Selection::cursor(4));
    }
}
