//! One open document — a buffer with its own undo history, file path, dirty
//! flag, scroll offset, and find-bar state. Inactive documents keep all of
//! this intact so switching tabs is lossless. The TextStack and palette
//! remain single-instance on [`super::State`]; only the per-document fields
//! live here.

use std::path::{Path, PathBuf};

use editor_core::Editor;

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
}

impl Document {
    pub fn new_scratch(initial_text: &str) -> Self {
        Self {
            editor: Editor::from(initial_text),
            file_path: None,
            dirty: false,
            scroll_y: 0.0,
            find: None,
        }
    }

    pub fn from_file(path: PathBuf, content: &str) -> Self {
        Self {
            editor: Editor::from(content),
            file_path: Some(path),
            dirty: false,
            scroll_y: 0.0,
            find: None,
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
}
