//! Command palette (spec §4.1.4) — the fuzzy-search popup that fires
//! commands by name.
//!
//! This module is pure logic: the visible list, the query, the selected row,
//! and a `nucleo`-backed fuzzy filter. The app's main module owns the overlay
//! rendering and the event wiring.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};

/// A command the palette can execute. The app dispatches these via
/// `execute_command`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    NewFile,
    OpenFile,
    SaveFile,
    SaveFileAs,
}

impl Command {
    /// The text shown for this command in the palette list.
    pub fn label(self) -> &'static str {
        match self {
            Command::NewFile => "New File",
            Command::OpenFile => "Open File…",
            Command::SaveFile => "Save",
            Command::SaveFileAs => "Save As…",
        }
    }
}

/// Every command the palette knows about, in registration order.
pub const ALL_COMMANDS: &[Command] = &[
    Command::NewFile,
    Command::OpenFile,
    Command::SaveFile,
    Command::SaveFileAs,
];

/// The popup's state.
pub struct CommandPalette {
    query: String,
    /// Indices into [`ALL_COMMANDS`], in display order (best match first).
    /// Always reflects the current `query`.
    visible: Vec<usize>,
    /// Index into `visible`, identifying the currently-highlighted row.
    /// Reset to 0 whenever the filter changes.
    selected: usize,
    /// Reused across re-filters — building one is more expensive than a query
    /// edit, and the matcher carries scratch buffers.
    matcher: Matcher,
}

impl CommandPalette {
    /// A palette with every command visible and the first row selected.
    pub fn new() -> Self {
        Self {
            query: String::new(),
            visible: (0..ALL_COMMANDS.len()).collect(),
            selected: 0,
            matcher: Matcher::new(Config::DEFAULT),
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// The commands matching the current query, in display order.
    pub fn visible(&self) -> impl Iterator<Item = Command> + '_ {
        self.visible.iter().map(|&i| ALL_COMMANDS[i])
    }

    pub fn visible_count(&self) -> usize {
        self.visible.len()
    }

    /// Zero-based row of the currently-selected command, or 0 when the
    /// visible list is empty.
    pub fn selected_row(&self) -> usize {
        self.selected
    }

    /// The currently-selected command, or `None` if the filter matched nothing.
    pub fn selected(&self) -> Option<Command> {
        self.visible.get(self.selected).map(|&i| ALL_COMMANDS[i])
    }

    /// Append a character to the query and re-filter.
    pub fn push_char(&mut self, c: char) {
        self.query.push(c);
        self.refilter();
    }

    /// Remove the last char of the query and re-filter. A no-op when empty.
    pub fn backspace(&mut self) {
        if self.query.pop().is_some() {
            self.refilter();
        }
    }

    /// Move selection to the next visible row, wrapping at the bottom.
    pub fn next(&mut self) {
        if !self.visible.is_empty() {
            self.selected = (self.selected + 1) % self.visible.len();
        }
    }

    /// Move selection to the previous visible row, wrapping at the top.
    pub fn prev(&mut self) {
        if let Some(last) = self.visible.len().checked_sub(1) {
            self.selected = if self.selected == 0 {
                last
            } else {
                self.selected - 1
            };
        }
    }

    /// Recompute the visible list from the current query. An empty query
    /// shows every command in registration order; otherwise nucleo scores
    /// each label and sorts by best-match-first, dropping non-matches.
    fn refilter(&mut self) {
        if self.query.is_empty() {
            self.visible = (0..ALL_COMMANDS.len()).collect();
        } else {
            let pattern = Pattern::parse(&self.query, CaseMatching::Smart, Normalization::Smart);
            let labels: Vec<&'static str> = ALL_COMMANDS.iter().map(|c| c.label()).collect();
            // match_list returns scored items sorted high → low; we then
            // map each surviving label back to its index in ALL_COMMANDS.
            let scored = pattern.match_list(labels, &mut self.matcher);
            self.visible = scored
                .into_iter()
                .filter_map(|(label, _)| ALL_COMMANDS.iter().position(|c| c.label() == label))
                .collect();
        }
        self.selected = 0;
    }
}

impl Default for CommandPalette {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(p: &CommandPalette) -> Vec<&'static str> {
        p.visible().map(|c| c.label()).collect()
    }

    #[test]
    fn new_shows_every_command() {
        let p = CommandPalette::new();
        assert_eq!(p.visible_count(), ALL_COMMANDS.len());
        assert_eq!(p.selected(), Some(Command::NewFile));
    }

    #[test]
    fn query_filters_case_insensitively() {
        let mut p = CommandPalette::new();
        p.push_char('s');
        // Both "Save" labels match; case doesn't matter (nucleo's Smart mode
        // treats lowercase query as case-insensitive).
        let visible_lower = labels(&p);
        assert!(visible_lower.contains(&"Save"));
        assert!(visible_lower.contains(&"Save As…"));

        p.backspace();
        p.push_char('S');
        let visible_upper = labels(&p);
        assert!(visible_upper.contains(&"Save"));
        assert!(visible_upper.contains(&"Save As…"));
    }

    #[test]
    fn backspace_repopulates_when_emptied() {
        let mut p = CommandPalette::new();
        p.push_char('z'); // matches nothing
        assert_eq!(p.visible_count(), 0);
        assert_eq!(p.selected(), None);
        p.backspace();
        assert_eq!(p.visible_count(), ALL_COMMANDS.len());
    }

    #[test]
    fn next_and_prev_wrap() {
        let mut p = CommandPalette::new();
        assert_eq!(p.selected_row(), 0);
        for _ in 0..ALL_COMMANDS.len() {
            p.next();
        }
        // wrapped around once — back to the top
        assert_eq!(p.selected_row(), 0);
        p.prev();
        // ...and prev from the top wraps to the bottom
        assert_eq!(p.selected_row(), ALL_COMMANDS.len() - 1);
    }

    #[test]
    fn navigation_is_a_noop_on_empty_results() {
        let mut p = CommandPalette::new();
        p.push_char('z'); // empty
        p.next();
        p.prev();
        assert_eq!(p.selected(), None);
        assert_eq!(p.selected_row(), 0);
    }

    #[test]
    fn changing_the_query_resets_selection() {
        let mut p = CommandPalette::new();
        p.next();
        p.next();
        assert_eq!(p.selected_row(), 2);
        p.push_char('s');
        assert_eq!(p.selected_row(), 0);
    }
}
