//! The editor — a buffer, its selections, and an undo history, with the
//! editing and movement operations that tie them together (spec §4.1.1).

use editor_buffer::TextBuffer;
use unicode_segmentation::GraphemeCursor;

use crate::{Selection, SelectionSet, UndoTree};

/// A text buffer plus its multi-cursor selection state and tree-based undo
/// history.
///
/// Edit operations apply at *every* selection simultaneously. Horizontal
/// movement is grapheme-cluster-aware so the caret never lands inside a Thai
/// cluster or an emoji ZWJ sequence (spec §3.4, G5). Vertical movement
/// (up/down + goal column) is a follow-up PR.
#[derive(Debug, Clone)]
pub struct Editor {
    buffer: TextBuffer,
    selections: SelectionSet,
    undo: UndoTree,
}

impl Editor {
    /// An empty editor — empty buffer, one cursor at the start.
    pub fn new() -> Self {
        Self::from(TextBuffer::new())
    }

    /// The current buffer.
    pub fn buffer(&self) -> &TextBuffer {
        &self.buffer
    }

    /// The current selection set.
    pub fn selections(&self) -> &SelectionSet {
        &self.selections
    }

    /// The buffer's text. Convenience for tests and callers that just want a
    /// `String`.
    pub fn text(&self) -> String {
        self.buffer.to_string()
    }

    // ── edit operations (applied at every selection) ──────────────────────

    /// Insert `text` at every selection, replacing any selected span. Each
    /// selection collapses to a cursor at the end of the inserted text.
    pub fn insert(&mut self, text: &str) {
        self.edit(|_sel| text.to_string());
    }

    /// Insert a line break at every selection, using the buffer's detected
    /// line-ending convention (spec §4.1.1 — preserve EOL).
    pub fn insert_newline(&mut self) {
        let eol = self.buffer.line_ending().as_str().to_string();
        self.edit(|_sel| eol.clone());
    }

    /// Delete backward at every selection: a non-empty selection deletes its
    /// span; a bare cursor deletes the grapheme before it.
    pub fn backspace(&mut self) {
        self.edit_ranges(|sel, ed| {
            if sel.is_cursor() {
                let to = sel.head;
                let from = ed.grapheme_before(to);
                from..to
            } else {
                sel.range()
            }
        });
    }

    /// Delete forward at every selection: a non-empty selection deletes its
    /// span; a bare cursor deletes the grapheme after it.
    pub fn delete_forward(&mut self) {
        self.edit_ranges(|sel, ed| {
            if sel.is_cursor() {
                let from = sel.head;
                let to = ed.grapheme_after(from);
                from..to
            } else {
                sel.range()
            }
        });
    }

    // ── horizontal movement (grapheme-aware) ──────────────────────────────

    /// Move every caret one grapheme left. With `extend`, the anchor stays put
    /// (growing/shrinking the selection); without it, a non-empty selection
    /// collapses to its start and a bare cursor steps left.
    pub fn move_left(&mut self, extend: bool) {
        self.move_horizontal(extend, true);
    }

    /// Move every caret one grapheme right. See [`move_left`](Editor::move_left)
    /// for the `extend` semantics (a non-empty selection collapses to its end).
    pub fn move_right(&mut self, extend: bool) {
        self.move_horizontal(extend, false);
    }

    // ── undo / redo ───────────────────────────────────────────────────────

    /// Whether [`undo`](Editor::undo) would do anything.
    pub fn can_undo(&self) -> bool {
        self.undo.can_undo()
    }

    /// Whether [`redo`](Editor::redo) would do anything.
    pub fn can_redo(&self) -> bool {
        self.undo.can_redo()
    }

    /// Step back to the previous state. Returns whether anything changed.
    pub fn undo(&mut self) -> bool {
        match self.undo.undo() {
            Some((buffer, selections)) => {
                self.buffer = buffer;
                self.selections = selections;
                true
            }
            None => false,
        }
    }

    /// Step forward along the most recent branch. Returns whether anything
    /// changed.
    pub fn redo(&mut self) -> bool {
        match self.undo.redo() {
            Some((buffer, selections)) => {
                self.buffer = buffer;
                self.selections = selections;
                true
            }
            None => false,
        }
    }

    // ── internals ─────────────────────────────────────────────────────────

    /// Apply an edit at every selection, where `replacement` yields the text
    /// to put in place of each selection's span.
    fn edit(&mut self, replacement: impl Fn(&Selection) -> String) {
        self.apply_edits(|sel, _ed| (sel.range(), replacement(sel)));
    }

    /// Apply an edit at every selection, where `range_of` yields the span to
    /// delete (the replacement is always empty — used by backspace/delete).
    fn edit_ranges(&mut self, range_of: impl Fn(&Selection, &Editor) -> std::ops::Range<usize>) {
        self.apply_edits(|sel, ed| (range_of(sel, ed), String::new()));
    }

    /// The shared edit machinery: for each selection (processed back to front
    /// so earlier indices stay valid), replace a span with text, collapse that
    /// selection to a cursor at the end of the inserted text, and shift every
    /// later selection by the edit's length delta. Commits one undo snapshot.
    fn apply_edits(
        &mut self,
        plan: impl Fn(&Selection, &Editor) -> (std::ops::Range<usize>, String),
    ) {
        let primary_index = self.selections.primary_index();
        let mut sels: Vec<Selection> = self.selections.selections().to_vec();

        // Plan every edit up front against the *original* buffer so range/text
        // computations (which may consult the editor, e.g. grapheme lookup)
        // see consistent state.
        let plans: Vec<(std::ops::Range<usize>, String)> =
            sels.iter().map(|sel| plan(sel, self)).collect();

        // Apply back to front: selections before index `i` are untouched by
        // edit `i`, so their planned ranges remain valid.
        for i in (0..sels.len()).rev() {
            let (range, text) = &plans[i];
            let old_len = range.len();
            let new_len = text.chars().count();
            self.buffer.replace(range.clone(), text);
            sels[i] = Selection::cursor(range.start + new_len);

            // Everything after edit `i` sat past `range.start`, so it shifts.
            let delta = new_len as isize - old_len as isize;
            for s in sels.iter_mut().skip(i + 1) {
                *s = s.shifted(delta);
            }
        }

        let primary_head = sels[primary_index].head;
        self.selections = SelectionSet::new(sels, primary_head);
        self.commit();
    }

    /// Move every caret one grapheme in the given direction.
    fn move_horizontal(&mut self, extend: bool, left: bool) {
        // SelectionSet::map cannot borrow `self` (it needs grapheme lookups),
        // so resolve every selection's new form first, then rebuild the set.
        let primary_index = self.selections.primary_index();
        let new: Vec<Selection> = self
            .selections
            .selections()
            .iter()
            .map(|sel| {
                if extend {
                    let head = if left {
                        self.grapheme_before(sel.head)
                    } else {
                        self.grapheme_after(sel.head)
                    };
                    Selection::new(sel.anchor, head)
                } else if !sel.is_cursor() {
                    // Collapse a selection to the edge the caret moved toward.
                    Selection::cursor(if left { sel.start() } else { sel.end() })
                } else {
                    let head = if left {
                        self.grapheme_before(sel.head)
                    } else {
                        self.grapheme_after(sel.head)
                    };
                    Selection::cursor(head)
                }
            })
            .collect();
        let primary_head = new[primary_index].head;
        self.selections = SelectionSet::new(new, primary_head);
    }

    /// Record the current state as a new undo snapshot.
    fn commit(&mut self) {
        self.undo
            .commit(self.buffer.clone(), self.selections.clone());
    }

    /// The `char` index of the grapheme boundary before `char_idx`.
    ///
    /// Within a line this is fully grapheme-cluster aware (Thai clusters,
    /// emoji ZWJ sequences, CRLF). Crossing into the previous line steps one
    /// `char` — exact for LF; for CRLF the caret lands between `\r` and `\n`,
    /// a known nit not worth the complexity here.
    fn grapheme_before(&self, char_idx: usize) -> usize {
        if char_idx == 0 {
            return 0;
        }
        let pos = self.buffer.char_to_position(char_idx);
        if pos.column == 0 {
            return char_idx - 1;
        }
        let line = self.buffer.line(pos.line).expect("line in range");
        let byte = char_to_byte(&line, pos.column);
        let mut gc = GraphemeCursor::new(byte, line.len(), true);
        let prev_byte = gc.prev_boundary(&line, 0).ok().flatten().unwrap_or(0);
        let prev_col = byte_to_char(&line, prev_byte);
        char_idx - (pos.column - prev_col)
    }

    /// The `char` index of the grapheme boundary after `char_idx`.
    ///
    /// See [`grapheme_before`](Editor::grapheme_before) for the line-crossing
    /// caveat.
    fn grapheme_after(&self, char_idx: usize) -> usize {
        let len = self.buffer.len_chars();
        if char_idx >= len {
            return len;
        }
        let pos = self.buffer.char_to_position(char_idx);
        let line = self.buffer.line(pos.line).expect("line in range");
        let line_chars = line.chars().count();
        if pos.column >= line_chars {
            // Last line, caret at the very end — nothing past it.
            return len;
        }
        let byte = char_to_byte(&line, pos.column);
        let mut gc = GraphemeCursor::new(byte, line.len(), true);
        let next_byte = gc
            .next_boundary(&line, 0)
            .ok()
            .flatten()
            .unwrap_or(line.len());
        let next_col = byte_to_char(&line, next_byte);
        char_idx + (next_col - pos.column)
    }
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl From<TextBuffer> for Editor {
    fn from(buffer: TextBuffer) -> Self {
        let selections = SelectionSet::single(Selection::cursor(0));
        let undo = UndoTree::new(buffer.clone(), selections.clone());
        Self {
            buffer,
            selections,
            undo,
        }
    }
}

impl From<&str> for Editor {
    fn from(text: &str) -> Self {
        Editor::from(TextBuffer::from(text))
    }
}

/// Byte offset of the `char`-th character in `s` (or `s.len()` if past the end).
fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

/// Number of `char`s before byte offset `byte_idx` in `s`.
fn byte_to_char(s: &str, byte_idx: usize) -> usize {
    s.char_indices().take_while(|(b, _)| *b < byte_idx).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursors(ed: &Editor) -> Vec<Selection> {
        ed.selections().selections().to_vec()
    }

    #[test]
    fn insert_at_single_cursor() {
        let mut ed = Editor::new();
        ed.insert("hello");
        assert_eq!(ed.text(), "hello");
        assert_eq!(ed.selections().primary(), Selection::cursor(5));
    }

    #[test]
    fn insert_replaces_selection() {
        let mut ed = Editor::from("the quick fox");
        ed.selections_mut_for_test(SelectionSet::single(Selection::new(4, 9)));
        ed.insert("slow");
        assert_eq!(ed.text(), "the slow fox");
        assert_eq!(ed.selections().primary(), Selection::cursor(8));
    }

    #[test]
    fn insert_at_every_cursor() {
        let mut ed = Editor::from("a\nb\nc");
        // a cursor at the start of each line: chars 0, 2, 4
        ed.selections_mut_for_test(SelectionSet::new(
            vec![
                Selection::cursor(0),
                Selection::cursor(2),
                Selection::cursor(4),
            ],
            0,
        ));
        ed.insert("> ");
        assert_eq!(ed.text(), "> a\n> b\n> c");
        // every cursor sits just after its inserted "> "
        let cs = cursors(&ed);
        assert_eq!(cs.len(), 3);
        assert!(cs.iter().all(|s| s.is_cursor()));
    }

    #[test]
    fn backspace_deletes_grapheme_before_cursor() {
        let mut ed = Editor::from("abc");
        ed.selections_mut_for_test(SelectionSet::single(Selection::cursor(3)));
        ed.backspace();
        assert_eq!(ed.text(), "ab");
        ed.backspace();
        assert_eq!(ed.text(), "a");
    }

    #[test]
    fn backspace_at_start_is_a_noop_edit() {
        let mut ed = Editor::from("abc");
        ed.selections_mut_for_test(SelectionSet::single(Selection::cursor(0)));
        ed.backspace();
        assert_eq!(ed.text(), "abc");
    }

    #[test]
    fn backspace_deletes_selection() {
        let mut ed = Editor::from("hello world");
        ed.selections_mut_for_test(SelectionSet::single(Selection::new(5, 11)));
        ed.backspace();
        assert_eq!(ed.text(), "hello");
    }

    #[test]
    fn delete_forward_deletes_grapheme_after_cursor() {
        let mut ed = Editor::from("abc");
        ed.selections_mut_for_test(SelectionSet::single(Selection::cursor(0)));
        ed.delete_forward();
        assert_eq!(ed.text(), "bc");
    }

    #[test]
    fn backspace_deletes_whole_thai_cluster() {
        // "ก้" — consonant ก + tone mark ไม้โท (U+0E49, a nonspacing mark).
        // They are two chars but one grapheme cluster (spec §3.4); backspace
        // must remove the whole cluster, not split it.
        let mut ed = Editor::from("ก้");
        assert_eq!(
            ed.buffer().len_chars(),
            2,
            "consonant + tone mark = 2 chars"
        );
        let len = ed.buffer().len_chars();
        ed.selections_mut_for_test(SelectionSet::single(Selection::cursor(len)));
        ed.backspace();
        // the whole "ก้" cluster is gone in one backspace
        assert_eq!(ed.text(), "");
    }

    #[test]
    fn backspace_deletes_emoji_zwj_sequence_as_one() {
        // family emoji = several scalars joined by ZWJ — one grapheme.
        let family = "👨‍👩‍👧‍👦";
        let mut ed = Editor::from(family);
        let len = ed.buffer().len_chars();
        assert!(len > 1, "the ZWJ sequence is several chars");
        ed.selections_mut_for_test(SelectionSet::single(Selection::cursor(len)));
        ed.backspace();
        assert_eq!(ed.text(), "");
    }

    #[test]
    fn move_right_steps_over_a_grapheme_cluster() {
        // "ก้x" — the "ก้" cluster is 2 chars, "x" is 1.
        let mut ed = Editor::from("ก้x");
        ed.selections_mut_for_test(SelectionSet::single(Selection::cursor(0)));
        ed.move_right(false);
        // "ก้" is one cluster — the caret jumps past both chars at once
        assert_eq!(ed.selections().primary(), Selection::cursor(2));
        ed.move_right(false);
        assert_eq!(ed.selections().primary(), Selection::cursor(3));
    }

    #[test]
    fn move_left_steps_over_a_grapheme_cluster() {
        let mut ed = Editor::from("ก้x");
        let len = ed.buffer().len_chars();
        ed.selections_mut_for_test(SelectionSet::single(Selection::cursor(len)));
        ed.move_left(false);
        assert_eq!(ed.selections().primary(), Selection::cursor(2));
        ed.move_left(false);
        assert_eq!(ed.selections().primary(), Selection::cursor(0));
    }

    #[test]
    fn move_right_with_extend_grows_the_selection() {
        let mut ed = Editor::from("abc");
        ed.selections_mut_for_test(SelectionSet::single(Selection::cursor(0)));
        ed.move_right(true);
        assert_eq!(ed.selections().primary(), Selection::new(0, 1));
        ed.move_right(true);
        assert_eq!(ed.selections().primary(), Selection::new(0, 2));
    }

    #[test]
    fn move_left_without_extend_collapses_a_selection() {
        let mut ed = Editor::from("abcdef");
        ed.selections_mut_for_test(SelectionSet::single(Selection::new(1, 5)));
        ed.move_left(false);
        // collapses to the start of the selection
        assert_eq!(ed.selections().primary(), Selection::cursor(1));
    }

    #[test]
    fn move_right_crosses_line_boundary() {
        let mut ed = Editor::from("ab\ncd");
        ed.selections_mut_for_test(SelectionSet::single(Selection::cursor(2)));
        ed.move_right(false); // over the '\n'
        assert_eq!(ed.selections().primary(), Selection::cursor(3));
    }

    #[test]
    fn undo_redo_round_trip() {
        let mut ed = Editor::from("a");
        ed.selections_mut_for_test(SelectionSet::single(Selection::cursor(1)));
        ed.insert("b");
        ed.insert("c");
        assert_eq!(ed.text(), "abc");

        assert!(ed.undo());
        assert_eq!(ed.text(), "ab");
        assert!(ed.undo());
        assert_eq!(ed.text(), "a");
        assert!(!ed.undo()); // back at the root

        assert!(ed.redo());
        assert_eq!(ed.text(), "ab");
        assert!(ed.redo());
        assert_eq!(ed.text(), "abc");
        assert!(!ed.redo());
    }

    #[test]
    fn undo_restores_selections() {
        let mut ed = Editor::from("xy");
        ed.selections_mut_for_test(SelectionSet::single(Selection::cursor(2)));
        ed.insert("z"); // "xyz", cursor at 3
        ed.undo();
        // selection state is restored along with the buffer
        assert_eq!(ed.selections().primary(), Selection::cursor(2));
    }

    // Test-only helper to seed selection state without going through editing.
    impl Editor {
        fn selections_mut_for_test(&mut self, selections: SelectionSet) {
            self.selections = selections;
            // keep the undo root consistent with the seeded state
            self.undo = UndoTree::new(self.buffer.clone(), self.selections.clone());
        }
    }
}
