//! Command palette (spec §4.1.4) — the fuzzy-search popup that fires
//! commands by name.
//!
//! This module is pure logic: the visible list, the query, the selected row,
//! and a `nucleo`-backed fuzzy filter. The app's main module owns the overlay
//! rendering and the event wiring.
//!
//! Entries are passed in at construction so the palette can mix built-in
//! commands with dynamic ones — npm scripts discovered from the workspace's
//! `package.json`, for instance — without the palette having to know
//! anything about the host.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};

/// Identifier for what a palette entry does. Cheap to copy; the dispatch
/// site in `main.rs` matches on this to fire the actual command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandId {
    NewFile,
    OpenFile,
    SaveFile,
    SaveFileAs,
    SaveAll,
    CloseOtherTabs,
    CloseAllTabs,
    ThemeDefault,
    ThemeSolarizedDark,
    ThemeSolarizedLight,
    ThemeMonokai,
    ThemeGruvboxDark,
    ThemeNord,
    ThemeTokyoNight,
    BrowseThemes,
    /// Run a `package.json` script by name in the embedded terminal.
    /// The string is the bare script name (the host already knows the
    /// package manager and the workspace root).
    RunScript(String),
}

/// One entry shown in the palette. `id` drives dispatch; `label` is what
/// the user sees and what the fuzzy matcher scores against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandEntry {
    pub id: CommandId,
    pub label: String,
}

impl CommandEntry {
    /// Convenience: build an entry for a built-in command with the
    /// canonical label used everywhere else in the chrome.
    pub fn builtin(id: CommandId) -> Self {
        let label = match &id {
            CommandId::NewFile => "New File",
            CommandId::OpenFile => "Open File…",
            CommandId::SaveFile => "Save",
            CommandId::SaveFileAs => "Save As…",
            CommandId::SaveAll => "Save All",
            CommandId::CloseOtherTabs => "Close Other Tabs",
            CommandId::CloseAllTabs => "Close All Tabs",
            CommandId::ThemeDefault => "Theme: Default Dark",
            CommandId::ThemeSolarizedDark => "Theme: Solarized Dark",
            CommandId::ThemeSolarizedLight => "Theme: Solarized Light",
            CommandId::ThemeMonokai => "Theme: Monokai",
            CommandId::ThemeGruvboxDark => "Theme: Gruvbox Dark",
            CommandId::ThemeNord => "Theme: Nord",
            CommandId::ThemeTokyoNight => "Theme: Tokyo Night",
            CommandId::BrowseThemes => "Theme: Browse…",
            CommandId::RunScript(_) => "Run script",
        };
        Self {
            id,
            label: label.to_string(),
        }
    }
}

/// The set of built-in command ids, in the order the palette shows them
/// when the query is empty. Dynamic entries (scripts) are appended after
/// these by the host.
pub const BUILTIN_COMMAND_IDS: &[CommandId] = &[
    CommandId::NewFile,
    CommandId::OpenFile,
    CommandId::SaveFile,
    CommandId::SaveFileAs,
    CommandId::SaveAll,
    CommandId::CloseOtherTabs,
    CommandId::CloseAllTabs,
    CommandId::ThemeDefault,
    CommandId::ThemeSolarizedDark,
    CommandId::ThemeSolarizedLight,
    CommandId::ThemeMonokai,
    CommandId::ThemeGruvboxDark,
    CommandId::ThemeNord,
    CommandId::ThemeTokyoNight,
    CommandId::BrowseThemes,
];

/// The popup's state. Built from a fresh list of entries every time the
/// palette opens — see [`CommandPalette::new`].
pub struct CommandPalette {
    query: String,
    entries: Vec<CommandEntry>,
    /// Indices into `entries`, in display order (best match first).
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
    /// A palette over `entries`, with every entry visible and the first
    /// row selected. Pass the built-ins first so they keep their stable
    /// order on the empty query, then any dynamic entries.
    pub fn new(entries: Vec<CommandEntry>) -> Self {
        let visible = (0..entries.len()).collect();
        Self {
            query: String::new(),
            entries,
            visible,
            selected: 0,
            matcher: Matcher::new(Config::DEFAULT),
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// Iterator over the labels of the currently-matching entries, in
    /// display order. Used by the renderer.
    pub fn visible_labels(&self) -> impl Iterator<Item = &str> + '_ {
        self.visible.iter().map(|&i| self.entries[i].label.as_str())
    }

    pub fn visible_count(&self) -> usize {
        self.visible.len()
    }

    /// Zero-based row of the currently-selected command, or 0 when the
    /// visible list is empty.
    pub fn selected_row(&self) -> usize {
        self.selected
    }

    /// The currently-selected entry, or `None` if the filter matched nothing.
    pub fn selected(&self) -> Option<&CommandEntry> {
        self.visible.get(self.selected).map(|&i| &self.entries[i])
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
    /// shows every entry in registration order; otherwise nucleo scores
    /// each label and sorts by best-match-first, dropping non-matches.
    fn refilter(&mut self) {
        if self.query.is_empty() {
            self.visible = (0..self.entries.len()).collect();
        } else {
            let pattern = Pattern::parse(&self.query, CaseMatching::Smart, Normalization::Smart);
            // Match against owned labels. `match_list` consumes the
            // strings, so we hand it clones and map results back to
            // indices by label equality. Identical labels (rare) tie-
            // break to whichever appears first — fine for v1.
            let labels: Vec<String> = self.entries.iter().map(|e| e.label.clone()).collect();
            let scored = pattern.match_list(&labels, &mut self.matcher);
            self.visible = scored
                .into_iter()
                .filter_map(|(label, _)| self.entries.iter().position(|e| e.label == *label))
                .collect();
        }
        self.selected = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtin_palette() -> CommandPalette {
        let entries: Vec<CommandEntry> = BUILTIN_COMMAND_IDS
            .iter()
            .cloned()
            .map(CommandEntry::builtin)
            .collect();
        CommandPalette::new(entries)
    }

    #[test]
    fn new_shows_every_command() {
        let p = builtin_palette();
        assert_eq!(p.visible_count(), BUILTIN_COMMAND_IDS.len());
        assert_eq!(p.selected().map(|e| e.id.clone()), Some(CommandId::NewFile));
    }

    #[test]
    fn query_filters_case_insensitively() {
        let mut p = builtin_palette();
        for c in "save".chars() {
            p.push_char(c);
        }
        let labels: Vec<&str> = p.visible_labels().collect();
        assert!(labels.iter().all(|l| l.to_lowercase().contains("save")));
        // Three Save variants in the static list.
        assert_eq!(labels.len(), 3);
    }

    #[test]
    fn next_and_prev_wrap() {
        let mut p = builtin_palette();
        let last = p.visible_count() - 1;
        p.prev();
        assert_eq!(p.selected_row(), last);
        p.next();
        assert_eq!(p.selected_row(), 0);
    }

    #[test]
    fn navigation_is_a_noop_on_empty_results() {
        let mut p = builtin_palette();
        for c in "xyzzy".chars() {
            p.push_char(c);
        }
        assert_eq!(p.visible_count(), 0);
        p.next();
        p.prev();
        assert_eq!(p.selected(), None);
    }

    #[test]
    fn dynamic_entries_append_after_builtins() {
        let mut entries: Vec<CommandEntry> = BUILTIN_COMMAND_IDS
            .iter()
            .cloned()
            .map(CommandEntry::builtin)
            .collect();
        entries.push(CommandEntry {
            id: CommandId::RunScript("dev".into()),
            label: "Run script: dev".into(),
        });
        let palette = CommandPalette::new(entries);
        assert_eq!(
            palette.visible_count(),
            BUILTIN_COMMAND_IDS.len() + 1,
            "dynamic script entry should be visible"
        );
        // Last entry on empty-query is the script.
        let last_label = palette.visible_labels().last().expect("last entry exists");
        assert_eq!(last_label, "Run script: dev");
    }
}
