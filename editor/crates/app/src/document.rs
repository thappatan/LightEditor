//! One open document — a buffer with its own undo history, file path, dirty
//! flag, scroll offset, and find-bar state. Inactive documents keep all of
//! this intact so switching tabs is lossless. The TextStack and palette
//! remain single-instance on [`super::State`]; only the per-document fields
//! live here.

use std::path::{Path, PathBuf};

use editor_core::Editor;
use editor_syntax::{Highlight, Highlighter, Language};

use crate::find::FindBar;

pub struct Document {
    pub editor: Editor,
    pub file_path: Option<PathBuf>,
    pub dirty: bool,
    /// Vertical scroll, in physical pixels. Kept per-tab so switching back to
    /// a long file lands the user where they were, not at the top.
    pub scroll_y: f32,
    /// Find bar belongs to the document: switch tab, find bar disappears;
    /// switch back, it reappears with the same query.
    pub find: Option<FindBar>,
    /// Tree-sitter highlighter when the document's extension matches one of
    /// the supported languages. `None` for pathless / unknown-extension
    /// documents — the renderer falls back to plain text.
    pub highlighter: Option<Highlighter>,
    /// Cached highlights from the most recent successful parse, keyed on
    /// the editor's revision at parse time. The renderer reuses these when
    /// the revision hasn't moved (typical when switching tabs without
    /// editing).
    pub cached_highlights: Vec<Highlight>,
    /// Revision counter the cached highlights were produced from. `None`
    /// means "no parse yet" — force one.
    pub cached_revision: Option<u64>,
    /// Detected indent width *in characters* for this document, used by
    /// indent guides so they line up with the file's actual indentation
    /// rather than the user-settings `tab_size`. Falls back to 4 for
    /// fresh / un-indented files.
    pub indent_unit: usize,
}

impl Document {
    pub fn new_scratch(initial_text: &str) -> Self {
        Self {
            editor: Editor::from(initial_text),
            file_path: None,
            dirty: false,
            scroll_y: 0.0,
            find: None,
            highlighter: None,
            cached_highlights: Vec::new(),
            cached_revision: None,
            indent_unit: detect_indent_unit(initial_text),
        }
    }

    pub fn from_file(path: PathBuf, content: &str) -> Self {
        let highlighter = Language::for_path(&path).and_then(|l| Highlighter::new(l).ok());
        let indent_unit = detect_indent_unit(content);
        Self {
            editor: Editor::from(content),
            file_path: Some(path),
            dirty: false,
            scroll_y: 0.0,
            find: None,
            highlighter,
            cached_highlights: Vec::new(),
            cached_revision: None,
            indent_unit,
        }
    }

    /// Filename (or "untitled") for the tab strip and window title.
    pub fn label(&self) -> String {
        match self.file_path.as_deref() {
            Some(p) => filename(p),
            None => "untitled".to_string(),
        }
    }

    /// `true` when the document is a never-saved, never-edited scratch buffer.
    /// Used by file-open to replace the slot instead of pushing a new tab.
    pub fn is_pristine_scratch(&self) -> bool {
        self.file_path.is_none() && !self.dirty
    }
}

/// Best-effort detection of the document's leading-space indent unit.
/// Scans up to the first 500 lines; the smallest non-zero leading-space
/// count seen (ignoring blank / tab-indented lines) becomes the unit.
/// Returns 4 when no spaced indent is found.
fn detect_indent_unit(text: &str) -> usize {
    let mut smallest = usize::MAX;
    for line in text.lines().take(500) {
        // Tab-indented lines defeat space-based guides; skip them.
        if line.starts_with('\t') {
            continue;
        }
        let leading = line.chars().take_while(|c| *c == ' ').count();
        if leading == 0 {
            continue;
        }
        if line[leading..].trim().is_empty() {
            continue; // blank-with-whitespace
        }
        if leading < smallest {
            smallest = leading;
        }
    }
    if smallest == usize::MAX || smallest == 0 {
        4
    } else {
        smallest
    }
}

fn filename(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scratch_starts_pristine() {
        let d = Document::new_scratch("");
        assert!(d.is_pristine_scratch());
        assert_eq!(d.label(), "untitled");
    }

    #[test]
    fn file_label_is_basename() {
        let d = Document::from_file(PathBuf::from("/tmp/foo/bar.rs"), "");
        assert_eq!(d.label(), "bar.rs");
        assert!(!d.is_pristine_scratch());
    }

    #[test]
    fn dirty_scratch_is_not_pristine() {
        let mut d = Document::new_scratch("");
        d.dirty = true;
        assert!(!d.is_pristine_scratch());
    }

    #[test]
    fn detect_indent_unit_finds_two_space_indent() {
        let src = "fn main() {\n  let x = 1;\n  if x > 0 {\n    let y = 2;\n  }\n}\n";
        assert_eq!(detect_indent_unit(src), 2);
    }

    #[test]
    fn detect_indent_unit_finds_four_space_indent() {
        let src = "fn a() {\n    let x = 1;\n        let y = 2;\n}\n";
        assert_eq!(detect_indent_unit(src), 4);
    }

    #[test]
    fn detect_indent_unit_defaults_to_4_when_unindented() {
        assert_eq!(detect_indent_unit("nope\nno indent\nat all\n"), 4);
        assert_eq!(detect_indent_unit(""), 4);
    }

    #[test]
    fn detect_indent_unit_ignores_tab_indented_lines() {
        let src = "\tfoo\n  bar\n    baz\n";
        // Tab-indented "foo" doesn't count; "  bar" wins.
        assert_eq!(detect_indent_unit(src), 2);
    }
}
