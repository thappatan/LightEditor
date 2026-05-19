//! The editor — a buffer, its selections, and an undo history, with the
//! editing and movement operations that tie them together (spec §4.1.1).

use editor_buffer::{BufferDelta, Position, TextBuffer};
use unicode_segmentation::GraphemeCursor;

use crate::{Selection, SelectionSet, UndoTree};

/// Edits accumulated since the last drain, plus a flag set when the buffer
/// was replaced wholesale (undo/redo). A `tree_invalidated == true` means
/// the deltas can be ignored — the caller should reset any cached parse
/// tree and reparse from scratch.
#[derive(Debug, Clone, Default)]
pub struct PendingEdits {
    pub tree_invalidated: bool,
    pub edits: Vec<BufferDelta>,
}

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
    /// Monotonic counter — bumped every time `apply_edits` (or undo/redo)
    /// changes the buffer text. Callers cache derived data keyed on this
    /// number and skip recomputation when it hasn't moved.
    revision: u64,
    /// Edits accumulated since the last [`take_pending_edits`] call. Drained
    /// by the syntax highlighter so its cached parse tree can be updated
    /// incrementally instead of re-parsed from scratch.
    ///
    /// [`take_pending_edits`]: Editor::take_pending_edits
    pending_edits: Vec<BufferDelta>,
    /// Set when the buffer was replaced wholesale (undo/redo): callers
    /// should drop any cached parse tree because `pending_edits` no longer
    /// describes the path from the previously-parsed text to the current
    /// text.
    tree_invalidated: bool,
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

    /// Monotonic revision counter. Bumped on every successful edit (and on
    /// each undo / redo step). Useful for caching parse trees, syntax
    /// highlights, scroll-position-derived data — anything that depends
    /// on the buffer's current text.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Drain edits accumulated since the last call (and whether the cached
    /// parse tree should be reset). Callers feed the deltas into the syntax
    /// highlighter so it can update its tree incrementally; if
    /// `tree_invalidated` is `true` the deltas are discarded and the tree
    /// is rebuilt from scratch.
    pub fn take_pending_edits(&mut self) -> PendingEdits {
        PendingEdits {
            tree_invalidated: std::mem::take(&mut self.tree_invalidated),
            edits: std::mem::take(&mut self.pending_edits),
        }
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

    // ── vertical movement (column-preserving) ─────────────────────────────

    /// Move every caret up one line, keeping its goal column (so moving up
    /// through a short line and back down does not lose the column). With
    /// `extend`, the anchor stays put. A caret already on the first line does
    /// not move.
    ///
    /// Columns here are `char` columns, not grapheme or visual columns —
    /// good enough for M1; visual-column vertical movement is a follow-up.
    pub fn move_up(&mut self, extend: bool) {
        self.move_vertical(extend, true);
    }

    /// Move every caret down one line. See [`move_up`](Editor::move_up) for
    /// the `extend` and goal-column semantics. A caret already on the last
    /// line does not move.
    pub fn move_down(&mut self, extend: bool) {
        self.move_vertical(extend, false);
    }

    // ── word / line / buffer movement ─────────────────────────────────────

    /// Move every caret to the previous word boundary. A "word" here is a
    /// run of alphanumeric or underscore chars; non-word chars (whitespace,
    /// punctuation, brackets) are skipped over together. See
    /// [`move_left`](Editor::move_left) for the `extend` semantics.
    pub fn move_word_left(&mut self, extend: bool) {
        self.move_head(extend, |ed, sel| ed.prev_word_start(sel.head));
    }

    /// Move every caret to the next word boundary. Mirror of
    /// [`move_word_left`](Editor::move_word_left).
    pub fn move_word_right(&mut self, extend: bool) {
        self.move_head(extend, |ed, sel| ed.next_word_end(sel.head));
    }

    /// Move every caret to column 0 of its current line.
    pub fn move_line_start(&mut self, extend: bool) {
        self.move_head(extend, |ed, sel| {
            let pos = ed.buffer.char_to_position(sel.head);
            ed.buffer
                .position_to_char(Position::new(pos.line, 0))
                .unwrap_or(sel.head)
        });
    }

    /// Move every caret to the end of its current line (before any
    /// trailing newline).
    pub fn move_line_end(&mut self, extend: bool) {
        self.move_head(extend, |ed, sel| {
            let pos = ed.buffer.char_to_position(sel.head);
            let col = ed.line_content_chars(pos.line);
            ed.buffer
                .position_to_char(Position::new(pos.line, col))
                .unwrap_or(sel.head)
        });
    }

    /// Move every caret to the very start of the buffer.
    pub fn move_buffer_start(&mut self, extend: bool) {
        self.move_head(extend, |_ed, _sel| 0);
    }

    /// Move every caret to the very end of the buffer.
    pub fn move_buffer_end(&mut self, extend: bool) {
        let len = self.buffer.len_chars();
        self.move_head(extend, move |_ed, _sel| len);
    }

    /// Delete from each cursor back to the previous word boundary; a
    /// non-empty selection just deletes its span.
    pub fn delete_word_left(&mut self) {
        self.edit_ranges(|sel, ed| {
            if sel.is_cursor() {
                let to = sel.head;
                let from = ed.prev_word_start(to);
                from..to
            } else {
                sel.range()
            }
        });
    }

    /// Delete from each cursor forward to the next word boundary; a
    /// non-empty selection just deletes its span.
    pub fn delete_word_right(&mut self) {
        self.edit_ranges(|sel, ed| {
            if sel.is_cursor() {
                let from = sel.head;
                let to = ed.next_word_end(from);
                from..to
            } else {
                sel.range()
            }
        });
    }

    /// Delete from each cursor back to the start of its current line.
    pub fn delete_to_line_start(&mut self) {
        self.edit_ranges(|sel, ed| {
            if sel.is_cursor() {
                let to = sel.head;
                let pos = ed.buffer.char_to_position(to);
                let from = ed
                    .buffer
                    .position_to_char(Position::new(pos.line, 0))
                    .unwrap_or(to);
                from..to
            } else {
                sel.range()
            }
        });
    }

    /// Swap the primary selection's line range with the line above. No-op
    /// when the range already starts at line 0.
    pub fn move_lines_up(&mut self) {
        self.move_lines(true);
    }

    /// Swap the primary selection's line range with the line below. No-op
    /// when the range already ends at the last line.
    pub fn move_lines_down(&mut self) {
        self.move_lines(false);
    }

    fn move_lines(&mut self, up: bool) {
        let pre = self.selections.clone();
        let primary = self.selections.primary();
        let (start_line, end_line) = self.line_range_for_selection(&primary);
        let total_lines = self.buffer.len_lines();
        if up && start_line == 0 {
            return;
        }
        if !up && end_line + 1 >= total_lines {
            return;
        }
        // Compute the two regions to swap: the selected block (lines
        // start_line..=end_line, including their trailing newlines), and the
        // adjacent line just above or below it.
        let block_start = self
            .buffer
            .position_to_char(Position::new(start_line, 0))
            .unwrap_or(0);
        let block_end = (block_start
            + (start_line..=end_line)
                .map(|l| self.buffer.line_len_chars(l).unwrap_or(0))
                .sum::<usize>())
        .min(self.buffer.len_chars());
        let block_text = self.buffer.slice(block_start..block_end);

        let adj_line = if up { start_line - 1 } else { end_line + 1 };
        let adj_start = self
            .buffer
            .position_to_char(Position::new(adj_line, 0))
            .unwrap_or(0);
        let adj_end = (adj_start + self.buffer.line_len_chars(adj_line).unwrap_or(0))
            .min(self.buffer.len_chars());
        let adj_text = self.buffer.slice(adj_start..adj_end);

        // For the last-line-doesn't-end-with-newline case, both the block
        // and the adjacent line might be missing a trailing newline. Ensure
        // the rebuilt order keeps one terminating newline per "row" so the
        // line count doesn't change.
        let block_has_eol = block_text.ends_with('\n') || block_text.ends_with("\r\n");
        let adj_has_eol = adj_text.ends_with('\n') || adj_text.ends_with("\r\n");
        let le = self.buffer.line_ending().as_str();
        let (block_norm, adj_norm) = match (up, block_has_eol, adj_has_eol) {
            // Moving the last block (no trailing EOL) up means the
            // previously-adjacent line becomes the new last; the *block*
            // gains an EOL, the new last (adj) loses one.
            (true, false, true) => {
                let mut b = block_text.clone();
                b.push_str(le);
                let a = adj_text
                    .trim_end_matches('\n')
                    .trim_end_matches('\r')
                    .to_string();
                (b, a)
            }
            // Moving a block down past the previously-last line: symmetric.
            (false, true, false) => {
                let mut a = adj_text.clone();
                a.push_str(le);
                let b = block_text
                    .trim_end_matches('\n')
                    .trim_end_matches('\r')
                    .to_string();
                (b, a)
            }
            _ => (block_text.clone(), adj_text.clone()),
        };

        let replacement = if up {
            format!("{block_norm}{adj_norm}")
        } else {
            format!("{adj_norm}{block_norm}")
        };
        let region_start = block_start.min(adj_start);
        let region_end = block_end.max(adj_end);
        self.record_replace(region_start..region_end, &replacement);

        // Track the selection: shift anchor and head by (signed) the
        // adjacent line's length in the right direction.
        let adj_len = adj_text.chars().count();
        let (new_anchor, new_head) = if up {
            (
                primary.anchor.saturating_sub(adj_len),
                primary.head.saturating_sub(adj_len),
            )
        } else {
            (primary.anchor + adj_len, primary.head + adj_len)
        };
        self.selections = SelectionSet::single(Selection::new(new_anchor, new_head));
        self.commit(pre);
    }

    /// Delete every logical line touched by any selection, plus its
    /// trailing newline. Multi-cursor aware via `edit_ranges`. The cursor
    /// collapses to the line that took the deleted line's place.
    pub fn delete_line(&mut self) {
        self.edit_ranges(|sel, ed| {
            let start_line = ed.buffer.char_to_position(sel.start()).line;
            let end_line = ed.buffer.char_to_position(sel.end()).line;
            let start = ed
                .buffer
                .position_to_char(Position::new(start_line, 0))
                .unwrap_or(sel.start());
            let line_start = ed
                .buffer
                .position_to_char(Position::new(end_line, 0))
                .unwrap_or(sel.end());
            let line_len = ed.buffer.line_len_chars(end_line).unwrap_or(0);
            let end = (line_start + line_len).min(ed.buffer.len_chars());
            start..end
        });
    }

    /// Prepend `indent` to each line the primary selection touches. A
    /// selection ending exactly at column 0 of a line excludes that line
    /// (so the last "anchor" line isn't dragged into the indent). Multi-
    /// cursor edits on the *same* line are not supported in v1 — the
    /// primary's selection drives the range.
    pub fn indent_lines(&mut self, indent: &str) {
        let pre = self.selections.clone();
        let primary = self.selections.primary();
        let (start_line, end_line) = self.line_range_for_selection(&primary);
        if end_line < start_line {
            return;
        }
        let indent_len = indent.chars().count();
        let mut anchor = primary.anchor;
        let mut head = primary.head;
        for line in (start_line..=end_line).rev() {
            let Some(line_start) = self.buffer.position_to_char(Position::new(line, 0)) else {
                continue;
            };
            self.record_insert(line_start, indent);
            if anchor >= line_start {
                anchor += indent_len;
            }
            if head >= line_start {
                head += indent_len;
            }
        }
        self.selections = SelectionSet::single(Selection::new(anchor, head));
        self.commit(pre);
    }

    /// Remove up to `max_spaces` leading spaces (or a single leading tab)
    /// from each line touched by the primary selection.
    pub fn outdent_lines(&mut self, max_spaces: usize) {
        let pre = self.selections.clone();
        let primary = self.selections.primary();
        let (start_line, end_line) = self.line_range_for_selection(&primary);
        if end_line < start_line {
            return;
        }
        let mut anchor = primary.anchor;
        let mut head = primary.head;
        for line in (start_line..=end_line).rev() {
            let Some(line_text) = self.buffer.line(line) else {
                continue;
            };
            let mut to_remove = 0;
            for c in line_text.chars().take(max_spaces) {
                if c == ' ' {
                    to_remove += 1;
                } else if c == '\t' && to_remove == 0 {
                    to_remove = 1;
                    break;
                } else {
                    break;
                }
            }
            if to_remove == 0 {
                continue;
            }
            let Some(line_start) = self.buffer.position_to_char(Position::new(line, 0)) else {
                continue;
            };
            self.record_remove(line_start..line_start + to_remove);
            if anchor > line_start {
                anchor = anchor.saturating_sub(to_remove);
            }
            if head > line_start {
                head = head.saturating_sub(to_remove);
            }
        }
        self.selections = SelectionSet::single(Selection::new(anchor, head));
        self.commit(pre);
    }

    /// Toggle a line comment (`prefix + space`) on every line touched by
    /// the primary selection. If every non-blank line already starts with
    /// the prefix (after leading whitespace), the prefix is stripped from
    /// each; otherwise it's added to each. Blank lines are skipped when
    /// adding so they don't grow ragged trailing markers.
    pub fn toggle_comment_lines(&mut self, prefix: &str) {
        let pre = self.selections.clone();
        let primary = self.selections.primary();
        let (start_line, end_line) = self.line_range_for_selection(&primary);
        if end_line < start_line {
            return;
        }
        let payload = format!("{prefix} ");
        let prefix_chars = prefix.chars().count();
        let payload_chars = payload.chars().count();

        let all_commented = (start_line..=end_line).all(|line| {
            let s = self.buffer.line(line).unwrap_or_default();
            let trimmed = s.trim_start_matches([' ', '\t']);
            // Blank lines don't count against the "all commented" check.
            let body = trimmed.trim_end_matches('\n').trim_end_matches('\r');
            body.is_empty() || body.starts_with(prefix)
        });

        let mut anchor = primary.anchor;
        let mut head = primary.head;

        if all_commented {
            for line in (start_line..=end_line).rev() {
                let Some(line_text) = self.buffer.line(line) else {
                    continue;
                };
                let leading_ws = line_text
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .count();
                let line_chars: Vec<char> = line_text.chars().collect();
                if !line_chars
                    .iter()
                    .skip(leading_ws)
                    .take(prefix_chars)
                    .copied()
                    .eq(prefix.chars())
                {
                    continue; // line was blank — nothing to strip
                }
                let after_prefix = leading_ws + prefix_chars;
                let extra_space = if line_chars.get(after_prefix) == Some(&' ') {
                    1
                } else {
                    0
                };
                let total = prefix_chars + extra_space;
                let Some(line_start) = self.buffer.position_to_char(Position::new(line, 0)) else {
                    continue;
                };
                let remove_at = line_start + leading_ws;
                self.record_remove(remove_at..remove_at + total);
                if anchor > remove_at {
                    anchor = anchor.saturating_sub(total);
                }
                if head > remove_at {
                    head = head.saturating_sub(total);
                }
            }
        } else {
            for line in (start_line..=end_line).rev() {
                let Some(line_text) = self.buffer.line(line) else {
                    continue;
                };
                if line_text
                    .chars()
                    .all(|c| c == ' ' || c == '\t' || c == '\n' || c == '\r')
                {
                    continue; // blank — skip when adding
                }
                let leading_ws = line_text
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .count();
                let Some(line_start) = self.buffer.position_to_char(Position::new(line, 0)) else {
                    continue;
                };
                let insert_at = line_start + leading_ws;
                self.record_insert(insert_at, &payload);
                if anchor >= insert_at {
                    anchor += payload_chars;
                }
                if head >= insert_at {
                    head += payload_chars;
                }
            }
        }
        self.selections = SelectionSet::single(Selection::new(anchor, head));
        self.commit(pre);
    }

    /// Range of lines touched by a selection. If the selection ends at
    /// column 0 of a line and spans more than that one line, that final
    /// line is excluded — it matches VSCode's "don't drag the cursor's
    /// landing line into a multi-line operation" intuition.
    fn line_range_for_selection(&self, sel: &Selection) -> (usize, usize) {
        let start_line = self.buffer.char_to_position(sel.start()).line;
        let end_pos = self.buffer.char_to_position(sel.end());
        let end_line = if end_pos.column == 0 && end_pos.line > start_line {
            end_pos.line - 1
        } else {
            end_pos.line
        };
        (start_line, end_line)
    }

    /// Delete from each cursor forward to the end of its current line
    /// (before any trailing newline).
    pub fn delete_to_line_end(&mut self) {
        self.edit_ranges(|sel, ed| {
            if sel.is_cursor() {
                let from = sel.head;
                let pos = ed.buffer.char_to_position(from);
                let col = ed.line_content_chars(pos.line);
                let to = ed
                    .buffer
                    .position_to_char(Position::new(pos.line, col))
                    .unwrap_or(from);
                from..to
            } else {
                sel.range()
            }
        });
    }

    // ── direct selection placement ────────────────────────────────────────

    /// Replace all selections with a single one — e.g. a mouse click, a drag,
    /// or jumping to a search result.
    ///
    /// This is not an edit, so it records no undo step. The caller is
    /// responsible for passing in-bounds `char` indices; positions past the
    /// buffer end are clamped.
    pub fn set_selection(&mut self, selection: Selection) {
        let len = self.buffer.len_chars();
        let clamped = Selection::new(selection.anchor.min(len), selection.head.min(len));
        self.selections = SelectionSet::single(clamped);
    }

    /// Add a selection alongside the existing ones (multi-cursor). The
    /// [`SelectionSet`] sorts and merges overlapping ranges; the new
    /// selection becomes the primary if it does not get merged away.
    pub fn add_selection(&mut self, selection: Selection) {
        let len = self.buffer.len_chars();
        let clamped = Selection::new(selection.anchor.min(len), selection.head.min(len));
        self.selections.push(clamped);
    }

    /// Drop every selection except the primary.
    pub fn collapse_to_primary(&mut self) {
        self.selections.collapse_to_primary();
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
                self.revision = self.revision.wrapping_add(1);
                self.invalidate_tree();
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
                self.revision = self.revision.wrapping_add(1);
                self.invalidate_tree();
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
        // Snapshot the pre-edit selections so the undo system can
        // restore the user's cursor on undo — see `commit`.
        let pre = self.selections.clone();
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
            self.record_replace(range.clone(), text);
            sels[i] = Selection::cursor(range.start + new_len);

            // Everything after edit `i` sat past `range.start`, so it shifts.
            let delta = new_len as isize - old_len as isize;
            for s in sels.iter_mut().skip(i + 1) {
                *s = s.shifted(delta);
            }
        }

        let primary_head = sels[primary_index].head;
        self.selections = SelectionSet::new(sels, primary_head);
        self.commit(pre);
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

    /// Move every caret up or down one line, preserving its goal column.
    fn move_vertical(&mut self, extend: bool, up: bool) {
        let primary_index = self.selections.primary_index();
        let new: Vec<Selection> = self
            .selections
            .selections()
            .iter()
            .map(|sel| {
                let head_pos = self.buffer.char_to_position(sel.head);
                // Start a run from the current column; continue one already going.
                let goal = sel.goal_column().unwrap_or(head_pos.column);

                let target_line = if up {
                    head_pos.line.checked_sub(1)
                } else if head_pos.line + 1 < self.buffer.len_lines() {
                    Some(head_pos.line + 1)
                } else {
                    None
                };

                let new_head = match target_line {
                    Some(line) => {
                        let col = goal.min(self.line_content_chars(line));
                        self.buffer
                            .position_to_char(Position::new(line, col))
                            .expect("clamped position is in range")
                    }
                    // No line in that direction — the caret stays, but the
                    // goal is kept so a reverse move resumes the column.
                    None => sel.head,
                };

                let moved = if extend {
                    Selection::new(sel.anchor, new_head)
                } else {
                    Selection::cursor(new_head)
                };
                moved.with_goal_column(goal)
            })
            .collect();
        let primary_head = new[primary_index].head;
        self.selections = SelectionSet::new(new, primary_head);
    }

    /// Record the current state as a new undo snapshot and bump the
    /// revision counter — every edit funnels through here. `pre`
    /// is where the cursor was right *before* the edit that produced
    /// the current state; the UndoTree uses it to refresh the
    /// soon-to-be-parent node so a future undo lands the user back
    /// at the edit site instead of wherever the cursor happened to
    /// be at the last commit.
    fn commit(&mut self, pre: SelectionSet) {
        self.undo
            .commit(self.buffer.clone(), self.selections.clone(), pre);
        self.revision = self.revision.wrapping_add(1);
    }

    /// Apply `text` over `range` (in `char`s) on the buffer and capture the
    /// resulting `BufferDelta` so callers (the syntax highlighter) can
    /// update a cached parse tree without reparsing the whole text. Every
    /// direct buffer write funnels through here.
    fn record_replace(&mut self, range: std::ops::Range<usize>, text: &str) {
        let start_char = range.start;
        let start_byte = self.buffer.char_to_byte(start_char);
        let start_point = self.buffer.byte_point(start_char);
        let old_end_byte = self.buffer.char_to_byte(range.end);
        let old_end_point = self.buffer.byte_point(range.end);
        self.buffer.replace(range, text);
        let new_end_char = start_char + text.chars().count();
        let new_end_byte = self.buffer.char_to_byte(new_end_char);
        let new_end_point = self.buffer.byte_point(new_end_char);
        self.pending_edits.push(BufferDelta {
            start_byte,
            old_end_byte,
            new_end_byte,
            start_point,
            old_end_point,
            new_end_point,
        });
    }

    /// Convenience wrapper for a pure insertion.
    fn record_insert(&mut self, char_idx: usize, text: &str) {
        self.record_replace(char_idx..char_idx, text);
    }

    /// Convenience wrapper for a pure deletion.
    fn record_remove(&mut self, range: std::ops::Range<usize>) {
        self.record_replace(range, "");
    }

    /// Mark the cached parse tree as invalid (used by undo/redo when the
    /// whole buffer is replaced). Any unsent edits are dropped because they
    /// no longer describe the path to the current text.
    fn invalidate_tree(&mut self) {
        self.pending_edits.clear();
        self.tree_invalidated = true;
    }

    /// Number of `char`s in `line`, excluding any trailing `\n` or `\r\n`.
    /// 0 if `line` is out of range.
    /// Common machinery for the word/line/buffer movement methods: for
    /// every selection, derive a new head from `head_of(self, sel)`. With
    /// `extend`, the anchor stays put; without, the selection collapses to
    /// a cursor at the new head.
    fn move_head(&mut self, extend: bool, head_of: impl Fn(&Editor, &Selection) -> usize) {
        let primary_index = self.selections.primary_index();
        let new: Vec<Selection> = self
            .selections
            .selections()
            .iter()
            .map(|sel| {
                let head = head_of(self, sel);
                if extend {
                    Selection::new(sel.anchor, head)
                } else {
                    Selection::cursor(head)
                }
            })
            .collect();
        let primary_head = new[primary_index].head;
        self.selections = SelectionSet::new(new, primary_head);
    }

    /// The `char` index at the start of the word at or before `from`. Walks
    /// left over non-word chars then over word chars; "word" is alphanumeric
    /// or underscore. Returns `0` when no word is found going back.
    fn prev_word_start(&self, from: usize) -> usize {
        let text = self.buffer.to_string();
        let chars: Vec<char> = text.chars().collect();
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let mut i = from.min(chars.len());
        while i > 0 && !is_word(chars[i - 1]) {
            i -= 1;
        }
        while i > 0 && is_word(chars[i - 1]) {
            i -= 1;
        }
        i
    }

    /// The `char` index at the end of the word at or after `from`. Walks
    /// right over non-word chars then over word chars. Returns
    /// `buffer.len_chars()` when no word is found going forward.
    fn next_word_end(&self, from: usize) -> usize {
        let text = self.buffer.to_string();
        let chars: Vec<char> = text.chars().collect();
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let mut i = from.min(chars.len());
        while i < chars.len() && !is_word(chars[i]) {
            i += 1;
        }
        while i < chars.len() && is_word(chars[i]) {
            i += 1;
        }
        i
    }

    fn line_content_chars(&self, line: usize) -> usize {
        match self.buffer.line(line) {
            Some(s) => s
                .trim_end_matches('\n')
                .trim_end_matches('\r')
                .chars()
                .count(),
            None => 0,
        }
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
            revision: 0,
            pending_edits: Vec::new(),
            tree_invalidated: false,
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
    fn move_down_keeps_column() {
        // "abcd\nefgh" — caret at col 2 of line 0 (char 2) moves to col 2 of
        // line 1 (char 7).
        let mut ed = Editor::from("abcd\nefgh");
        ed.selections_mut_for_test(SelectionSet::single(Selection::cursor(2)));
        ed.move_down(false);
        assert_eq!(ed.selections().primary(), Selection::cursor(7));
        ed.move_up(false);
        assert_eq!(ed.selections().primary(), Selection::cursor(2));
    }

    #[test]
    fn move_down_clamps_to_short_line_then_goal_column_restores() {
        // line 0 "abcdef" (col 4), line 1 "xy" (max col 2), line 2 "uvwxyz".
        // Down clamps to col 2 on the short line; down again restores col 4
        // because the goal column is remembered.
        let mut ed = Editor::from("abcdef\nxy\nuvwxyz");
        ed.selections_mut_for_test(SelectionSet::single(Selection::cursor(4)));
        ed.move_down(false);
        // line 1 starts at char 7; clamped to col 2 → char 9
        assert_eq!(ed.selections().primary(), Selection::cursor(9));
        ed.move_down(false);
        // line 2 starts at char 10; goal column 4 restored → char 14
        assert_eq!(ed.selections().primary(), Selection::cursor(14));
    }

    #[test]
    fn move_up_at_first_line_is_noop() {
        let mut ed = Editor::from("abc\ndef");
        ed.selections_mut_for_test(SelectionSet::single(Selection::cursor(1)));
        ed.move_up(false);
        assert_eq!(ed.selections().primary(), Selection::cursor(1));
    }

    #[test]
    fn move_down_at_last_line_is_noop() {
        let mut ed = Editor::from("abc\ndef");
        ed.selections_mut_for_test(SelectionSet::single(Selection::cursor(5)));
        ed.move_down(false);
        assert_eq!(ed.selections().primary(), Selection::cursor(5));
    }

    #[test]
    fn move_down_with_extend_grows_selection() {
        let mut ed = Editor::from("abcd\nefgh");
        ed.selections_mut_for_test(SelectionSet::single(Selection::cursor(1)));
        ed.move_down(true);
        // anchor stays at 1, head moves to col 1 of line 1 = char 6
        assert_eq!(ed.selections().primary(), Selection::new(1, 6));
    }

    #[test]
    fn horizontal_move_resets_the_goal_column() {
        // Down onto a short line (column clamped), then a horizontal move,
        // then down again — the goal column must NOT resurrect the old column.
        let mut ed = Editor::from("abcdef\nxy\nuvwxyz");
        ed.selections_mut_for_test(SelectionSet::single(Selection::cursor(4)));
        ed.move_down(false); // clamped to char 9 (col 2 of "xy")
        ed.move_left(false); // char 8 — clears the goal column
        ed.move_down(false); // col 1 of line 2 ("uvwxyz" starts at 10) → char 11
        assert_eq!(ed.selections().primary(), Selection::cursor(11));
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

    #[test]
    fn set_selection_replaces_all_and_clamps() {
        let mut ed = Editor::from("hello");
        ed.selections_mut_for_test(SelectionSet::new(
            vec![Selection::cursor(0), Selection::cursor(3)],
            0,
        ));
        // collapses the multi-cursor set to one selection
        ed.set_selection(Selection::new(1, 4));
        assert_eq!(ed.selections().len(), 1);
        assert_eq!(ed.selections().primary(), Selection::new(1, 4));

        // out-of-bounds indices are clamped to the buffer length
        ed.set_selection(Selection::new(99, 100));
        assert_eq!(ed.selections().primary(), Selection::cursor(5));
    }

    // ── word / line / buffer movement + delete ────────────────────────────

    #[test]
    fn move_word_right_jumps_over_word_and_whitespace() {
        let mut ed = Editor::from("hello world  foo");
        ed.set_selection(Selection::cursor(0));
        ed.move_word_right(false);
        // Skips no non-word chars (already at word), walks to end of "hello".
        assert_eq!(ed.selections().primary(), Selection::cursor(5));
        ed.move_word_right(false);
        // Skips " ", walks to end of "world".
        assert_eq!(ed.selections().primary(), Selection::cursor(11));
        ed.move_word_right(false);
        // Skips "  ", walks to end of "foo".
        assert_eq!(ed.selections().primary(), Selection::cursor(16));
    }

    #[test]
    fn move_word_left_jumps_back_to_word_start() {
        let mut ed = Editor::from("hello world");
        ed.set_selection(Selection::cursor(11));
        ed.move_word_left(false);
        assert_eq!(ed.selections().primary(), Selection::cursor(6));
        ed.move_word_left(false);
        assert_eq!(ed.selections().primary(), Selection::cursor(0));
    }

    #[test]
    fn move_line_start_and_end_anchor_to_columns() {
        let mut ed = Editor::from("    foo\nbar baz");
        ed.set_selection(Selection::cursor(6)); // mid-word "foo"
        ed.move_line_start(false);
        assert_eq!(ed.selections().primary(), Selection::cursor(0));
        ed.move_line_end(false);
        assert_eq!(ed.selections().primary(), Selection::cursor(7));
        // Second line — line_start lands at the line's first char, not 0.
        ed.set_selection(Selection::cursor(12));
        ed.move_line_start(false);
        assert_eq!(ed.selections().primary(), Selection::cursor(8));
    }

    #[test]
    fn move_buffer_start_and_end() {
        let mut ed = Editor::from("a\nb\nc");
        ed.set_selection(Selection::cursor(2));
        ed.move_buffer_start(false);
        assert_eq!(ed.selections().primary(), Selection::cursor(0));
        ed.move_buffer_end(false);
        assert_eq!(ed.selections().primary(), Selection::cursor(5));
    }

    #[test]
    fn delete_word_left_removes_previous_token() {
        let mut ed = Editor::from("hello world");
        ed.set_selection(Selection::cursor(11));
        ed.delete_word_left();
        assert_eq!(ed.text(), "hello ");
        assert_eq!(ed.selections().primary(), Selection::cursor(6));
        ed.delete_word_left();
        // Walks back over the space AND the previous word.
        assert_eq!(ed.text(), "");
    }

    #[test]
    fn delete_word_right_removes_next_token() {
        let mut ed = Editor::from("hello world");
        ed.set_selection(Selection::cursor(0));
        ed.delete_word_right();
        assert_eq!(ed.text(), " world");
        ed.delete_word_right();
        assert_eq!(ed.text(), "");
    }

    #[test]
    fn delete_to_line_start_keeps_other_lines() {
        let mut ed = Editor::from("aaa\nbbb ccc\nddd");
        ed.set_selection(Selection::cursor(11)); // end of "bbb ccc"
        ed.delete_to_line_start();
        assert_eq!(ed.text(), "aaa\n\nddd");
        assert_eq!(ed.selections().primary(), Selection::cursor(4));
    }

    #[test]
    fn delete_to_line_end_keeps_other_lines() {
        let mut ed = Editor::from("aaa\nbbb ccc\nddd");
        ed.set_selection(Selection::cursor(4)); // start of "bbb ccc"
        ed.delete_to_line_end();
        assert_eq!(ed.text(), "aaa\n\nddd");
        assert_eq!(ed.selections().primary(), Selection::cursor(4));
    }

    #[test]
    fn move_word_right_with_extend_grows_selection() {
        let mut ed = Editor::from("hello world");
        ed.set_selection(Selection::cursor(0));
        ed.move_word_right(true);
        let p = ed.selections().primary();
        assert_eq!(p.anchor, 0);
        assert_eq!(p.head, 5);
    }

    // ── line operations ───────────────────────────────────────────────────

    #[test]
    fn delete_line_removes_line_and_trailing_newline() {
        let mut ed = Editor::from("a\nb\nc\n");
        ed.set_selection(Selection::cursor(2)); // on line 1 ("b")
        ed.delete_line();
        assert_eq!(ed.text(), "a\nc\n");
    }

    #[test]
    fn delete_line_removes_every_touched_line() {
        let mut ed = Editor::from("a\nb\nc\nd\n");
        // Selection spans lines 1..3 ("b\nc")
        ed.set_selection(Selection::new(2, 5));
        ed.delete_line();
        assert_eq!(ed.text(), "a\nd\n");
    }

    #[test]
    fn indent_lines_prepends_to_each_touched_line() {
        let mut ed = Editor::from("a\nb\nc");
        ed.set_selection(Selection::new(0, 4)); // spans lines 0..2
        ed.indent_lines("    ");
        assert_eq!(ed.text(), "    a\n    b\nc");
    }

    #[test]
    fn outdent_lines_strips_leading_spaces() {
        let mut ed = Editor::from("    a\n    b\nc");
        ed.set_selection(Selection::new(0, ed.text().chars().count()));
        ed.outdent_lines(4);
        assert_eq!(ed.text(), "a\nb\nc");
    }

    #[test]
    fn toggle_comment_adds_when_no_line_is_commented() {
        let mut ed = Editor::from("foo\nbar\n");
        ed.set_selection(Selection::new(0, ed.text().chars().count()));
        ed.toggle_comment_lines("//");
        assert_eq!(ed.text(), "// foo\n// bar\n");
    }

    #[test]
    fn toggle_comment_removes_when_every_non_blank_line_is_commented() {
        let mut ed = Editor::from("// foo\n\n// bar\n");
        ed.set_selection(Selection::new(0, ed.text().chars().count()));
        ed.toggle_comment_lines("//");
        assert_eq!(ed.text(), "foo\n\nbar\n");
    }

    #[test]
    fn toggle_comment_preserves_leading_indent() {
        let mut ed = Editor::from("    foo\n    bar\n");
        ed.set_selection(Selection::new(0, ed.text().chars().count()));
        ed.toggle_comment_lines("//");
        assert_eq!(ed.text(), "    // foo\n    // bar\n");
    }

    #[test]
    fn move_lines_up_swaps_with_previous() {
        let mut ed = Editor::from("a\nb\nc\n");
        ed.set_selection(Selection::cursor(2)); // on "b"
        ed.move_lines_up();
        assert_eq!(ed.text(), "b\na\nc\n");
        assert_eq!(ed.selections().primary(), Selection::cursor(0));
    }

    #[test]
    fn move_lines_down_swaps_with_next() {
        let mut ed = Editor::from("a\nb\nc\n");
        ed.set_selection(Selection::cursor(2)); // on "b"
        ed.move_lines_down();
        assert_eq!(ed.text(), "a\nc\nb\n");
        assert_eq!(ed.selections().primary(), Selection::cursor(4));
    }

    #[test]
    fn move_lines_first_line_up_is_noop() {
        let mut ed = Editor::from("a\nb\n");
        ed.set_selection(Selection::cursor(0));
        ed.move_lines_up();
        assert_eq!(ed.text(), "a\nb\n");
    }

    #[test]
    fn move_lines_last_line_down_handles_missing_trailing_newline() {
        let mut ed = Editor::from("a\nb\nc");
        ed.set_selection(Selection::cursor(0)); // on "a"
        ed.move_lines_down();
        assert_eq!(ed.text(), "b\na\nc");
    }

    #[test]
    fn move_lines_round_trips() {
        let mut ed = Editor::from("alpha\nbeta\ngamma\n");
        ed.set_selection(Selection::new(6, 10)); // selects within "beta"
        ed.move_lines_down();
        ed.move_lines_up();
        assert_eq!(ed.text(), "alpha\nbeta\ngamma\n");
    }

    #[test]
    fn indent_then_outdent_round_trips() {
        let original = "a\nb\nc";
        let mut ed = Editor::from(original);
        ed.set_selection(Selection::new(0, ed.text().chars().count()));
        ed.indent_lines("  ");
        ed.outdent_lines(2);
        assert_eq!(ed.text(), original);
    }

    // ── revision counter ──────────────────────────────────────────────────

    #[test]
    fn revision_starts_at_zero_and_increments_on_each_edit() {
        let mut ed = Editor::new();
        assert_eq!(ed.revision(), 0);
        ed.insert("a");
        assert_eq!(ed.revision(), 1);
        ed.insert("b");
        assert_eq!(ed.revision(), 2);
    }

    #[test]
    fn revision_does_not_change_on_selection_only() {
        let mut ed = Editor::from("hi");
        let r0 = ed.revision();
        ed.set_selection(Selection::new(0, 2));
        ed.move_left(false);
        ed.move_right(true);
        assert_eq!(ed.revision(), r0);
    }

    // ── pending edits (for incremental syntax parsing) ────────────────────

    #[test]
    fn insert_records_a_pure_insertion_delta() {
        let mut ed = Editor::new();
        ed.insert("hi");
        let drained = ed.take_pending_edits();
        assert!(!drained.tree_invalidated);
        assert_eq!(drained.edits.len(), 1);
        let d = drained.edits[0];
        // Pure insertion at offset 0 of an empty buffer.
        assert_eq!(d.start_byte, 0);
        assert_eq!(d.old_end_byte, 0);
        assert_eq!(d.new_end_byte, 2);
        assert_eq!(d.start_point.row, 0);
        assert_eq!(d.new_end_point.column, 2);
    }

    #[test]
    fn backspace_records_a_pure_deletion_delta() {
        let mut ed = Editor::from("hi");
        ed.set_selection(Selection::cursor(2));
        ed.take_pending_edits(); // discard the no-op drain after seed
        ed.backspace();
        let drained = ed.take_pending_edits();
        let d = drained.edits[0];
        // Deletion: old_end > start_byte; new_end == start_byte.
        assert_eq!(d.start_byte, 1);
        assert_eq!(d.old_end_byte, 2);
        assert_eq!(d.new_end_byte, 1);
    }

    #[test]
    fn deltas_use_utf8_byte_offsets() {
        // "ก" — 3 UTF-8 bytes; inserting "x" between the two clusters
        // should put start_byte at byte 3, not char 1.
        let mut ed = Editor::from("กก");
        ed.set_selection(Selection::cursor(1));
        ed.take_pending_edits();
        ed.insert("x");
        let d = ed.take_pending_edits().edits[0];
        assert_eq!(d.start_byte, 3);
        assert_eq!(d.new_end_byte, 4);
    }

    #[test]
    fn multi_cursor_insert_records_one_delta_per_caret() {
        let mut ed = Editor::from("a\nb\nc");
        ed.add_selection(Selection::cursor(2));
        ed.add_selection(Selection::cursor(4));
        ed.take_pending_edits();
        ed.insert("> ");
        let drained = ed.take_pending_edits();
        assert_eq!(drained.edits.len(), 3);
        // Back-to-front: highest byte offset comes first.
        assert!(drained.edits[0].start_byte > drained.edits[1].start_byte);
    }

    #[test]
    fn undo_invalidates_tree_and_drops_pending_edits() {
        let mut ed = Editor::from("a");
        ed.insert("b");
        ed.undo();
        let drained = ed.take_pending_edits();
        assert!(drained.tree_invalidated);
        assert!(drained.edits.is_empty());
    }

    #[test]
    fn take_pending_edits_clears_state() {
        let mut ed = Editor::new();
        ed.insert("x");
        let first = ed.take_pending_edits();
        assert_eq!(first.edits.len(), 1);
        let second = ed.take_pending_edits();
        assert!(second.edits.is_empty());
        assert!(!second.tree_invalidated);
    }

    #[test]
    fn indent_lines_records_one_delta_per_line() {
        let mut ed = Editor::from("a\nb\nc");
        ed.set_selection(Selection::new(0, ed.text().chars().count()));
        ed.take_pending_edits();
        ed.indent_lines("  ");
        let drained = ed.take_pending_edits();
        assert_eq!(drained.edits.len(), 3);
        // Each delta is a pure insertion.
        for d in &drained.edits {
            assert_eq!(d.old_end_byte, d.start_byte);
            assert_eq!(d.new_end_byte - d.start_byte, 2);
        }
    }

    #[test]
    fn revision_bumps_on_undo_and_redo() {
        let mut ed = Editor::new();
        ed.insert("a");
        let r1 = ed.revision();
        assert!(ed.undo());
        assert!(ed.revision() > r1);
        let r2 = ed.revision();
        assert!(ed.redo());
        assert!(ed.revision() > r2);
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
