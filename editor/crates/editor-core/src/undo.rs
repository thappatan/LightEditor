//! Tree-based undo history (spec §4.1.1).
//!
//! Each node is a full `(TextBuffer, SelectionSet)` snapshot. Snapshots are
//! cheap because `ropey` clones share structure — an edit only copies the
//! path from root to the changed leaf.
//!
//! Editing after an undo does *not* discard the redo history: it creates a
//! new branch. Linear undo/redo is just the single-branch case. `redo` always
//! follows the most recently created branch.

use editor_buffer::TextBuffer;

use crate::SelectionSet;

/// One reachable editor state.
#[derive(Debug, Clone)]
struct UndoNode {
    buffer: TextBuffer,
    selections: SelectionSet,
    parent: Option<usize>,
    /// Child node indices in creation order — the last is the newest branch.
    children: Vec<usize>,
}

/// A tree of `(buffer, selections)` snapshots with a cursor at the current node.
#[derive(Debug, Clone)]
pub struct UndoTree {
    nodes: Vec<UndoNode>,
    current: usize,
}

impl UndoTree {
    /// Start a history rooted at the given state.
    pub fn new(buffer: TextBuffer, selections: SelectionSet) -> Self {
        Self {
            nodes: vec![UndoNode {
                buffer,
                selections,
                parent: None,
                children: Vec::new(),
            }],
            current: 0,
        }
    }

    /// The buffer at the current node.
    pub fn current_buffer(&self) -> &TextBuffer {
        &self.nodes[self.current].buffer
    }

    /// The selections at the current node.
    pub fn current_selections(&self) -> &SelectionSet {
        &self.nodes[self.current].selections
    }

    /// Record a new state as a child of the current node and move to it.
    ///
    /// `pre_selections` is where the user's cursor *was* right before
    /// the edit that produced this snapshot — i.e. where they should
    /// land after a future undo. We refresh the *current* (about-to-
    /// become-parent) node's selections to that value so the parent
    /// accurately reflects "the state the user could undo back to",
    /// even after the user moved the cursor between commits.
    ///
    /// If the current node already has children (i.e. we are here after an
    /// undo), this adds a *new branch* rather than replacing the old one.
    pub fn commit(
        &mut self,
        buffer: TextBuffer,
        selections: SelectionSet,
        pre_selections: SelectionSet,
    ) {
        self.nodes[self.current].selections = pre_selections;
        let new_index = self.nodes.len();
        self.nodes.push(UndoNode {
            buffer,
            selections,
            parent: Some(self.current),
            children: Vec::new(),
        });
        self.nodes[self.current].children.push(new_index);
        self.current = new_index;
    }

    /// Whether [`undo`](UndoTree::undo) would move.
    pub fn can_undo(&self) -> bool {
        self.nodes[self.current].parent.is_some()
    }

    /// Whether [`redo`](UndoTree::redo) would move.
    pub fn can_redo(&self) -> bool {
        !self.nodes[self.current].children.is_empty()
    }

    /// Move to the parent node and return its snapshot. `None` at the root.
    pub fn undo(&mut self) -> Option<(TextBuffer, SelectionSet)> {
        let parent = self.nodes[self.current].parent?;
        self.current = parent;
        Some(self.snapshot())
    }

    /// Move to the most recently created child and return its snapshot.
    /// `None` at a leaf.
    pub fn redo(&mut self) -> Option<(TextBuffer, SelectionSet)> {
        let &child = self.nodes[self.current].children.last()?;
        self.current = child;
        Some(self.snapshot())
    }

    fn snapshot(&self) -> (TextBuffer, SelectionSet) {
        let node = &self.nodes[self.current];
        (node.buffer.clone(), node.selections.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Selection;

    fn state(text: &str, cursor: usize) -> (TextBuffer, SelectionSet) {
        (
            TextBuffer::from(text),
            SelectionSet::single(Selection::cursor(cursor)),
        )
    }

    fn tree(text: &str, cursor: usize) -> UndoTree {
        let (b, s) = state(text, cursor);
        UndoTree::new(b, s)
    }

    #[test]
    fn root_cannot_undo_or_redo() {
        let t = tree("hello", 0);
        assert!(!t.can_undo());
        assert!(!t.can_redo());
        assert!(t.current_buffer().to_string() == "hello");
    }

    #[test]
    fn linear_undo_redo() {
        let mut t = tree("a", 1);
        let (b, s) = state("ab", 2);
        t.commit(b, s, SelectionSet::single(Selection::cursor(1)));
        let (b, s) = state("abc", 3);
        t.commit(b, s, SelectionSet::single(Selection::cursor(2)));

        assert_eq!(t.current_buffer().to_string(), "abc");
        assert!(t.can_undo());
        assert!(!t.can_redo());

        let (buf, _) = t.undo().unwrap();
        assert_eq!(buf.to_string(), "ab");
        let (buf, _) = t.undo().unwrap();
        assert_eq!(buf.to_string(), "a");
        assert!(t.undo().is_none()); // at root

        let (buf, _) = t.redo().unwrap();
        assert_eq!(buf.to_string(), "ab");
        let (buf, _) = t.redo().unwrap();
        assert_eq!(buf.to_string(), "abc");
        assert!(t.redo().is_none()); // at leaf
    }

    #[test]
    fn editing_after_undo_branches_without_losing_redo() {
        let mut t = tree("a", 1);
        t.commit(
            TextBuffer::from("ab"),
            SelectionSet::single(Selection::cursor(2)),
            SelectionSet::single(Selection::cursor(1)),
        );
        t.commit(
            TextBuffer::from("abc"),
            SelectionSet::single(Selection::cursor(3)),
            SelectionSet::single(Selection::cursor(2)),
        );

        // back to "ab"
        t.undo();
        assert_eq!(t.current_buffer().to_string(), "ab");

        // edit here — creates a second branch off "ab"
        t.commit(
            TextBuffer::from("abX"),
            SelectionSet::single(Selection::cursor(3)),
            SelectionSet::single(Selection::cursor(2)),
        );
        assert_eq!(t.current_buffer().to_string(), "abX");

        // undo goes back to "ab"; redo now follows the *newest* branch (abX)
        t.undo();
        assert_eq!(t.current_buffer().to_string(), "ab");
        let (buf, _) = t.redo().unwrap();
        assert_eq!(buf.to_string(), "abX");

        // the "abc" branch is still in the tree — reachable by undoing back
        // to "ab" and... it is the *older* child, so redo won't reach it,
        // but it was not destroyed. Confirm the tree still holds 4 nodes.
        assert_eq!(t.nodes.len(), 4);
    }

    #[test]
    fn snapshot_carries_selections() {
        let mut t = tree("x", 0);
        // commit also refreshes the parent's selections to `pre`, so
        // an undo lands us at the cursor location passed as the
        // third argument — not the value the parent was constructed
        // with.
        t.commit(
            TextBuffer::from("xy"),
            SelectionSet::single(Selection::new(0, 2)),
            SelectionSet::single(Selection::cursor(1)),
        );
        t.undo();
        assert_eq!(t.current_selections().primary(), Selection::cursor(1));
        let (_, sels) = t.redo().unwrap();
        assert_eq!(sels.primary(), Selection::new(0, 2));
    }

    #[test]
    fn pre_selections_overwrite_parent_at_commit() {
        // Even though `tree("x", 0)` seeds the root with cursor=0, a
        // commit whose `pre` says cursor=3 should leave the root
        // *with* cursor=3 — that's the whole point: parent records
        // where the user was just before the edit, not where they
        // happened to be at construction.
        let mut t = tree("xxx", 0);
        t.commit(
            TextBuffer::from("xxxy"),
            SelectionSet::single(Selection::cursor(4)),
            SelectionSet::single(Selection::cursor(3)),
        );
        let (_, sels) = t.undo().unwrap();
        assert_eq!(sels.primary(), Selection::cursor(3));
    }
}
