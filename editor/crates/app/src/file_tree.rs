//! File-tree sidebar state (spec §4.1.2).
//!
//! A flat `Vec<TreeNode>` representing the *visible* rows in the tree
//! sidebar. Expanding a directory splices its children into the vec at
//! the directory's index + 1; collapsing removes that contiguous range.
//! Rendering is then a straight iteration — no recursive traversal at
//! draw time.
//!
//! Filesystem reads are lazy: a directory's children load only on its
//! first expand. The tree does not watch for filesystem changes in v1;
//! the user re-toggles the sidebar (Cmd-B twice) or relaunches to pick
//! up new files.

use std::path::{Path, PathBuf};

/// Directories that are uninteresting to a code editor by default. Hard-
/// coded for v1; surfacing as a setting is a follow-up. Public so the
/// file-tree watcher in `main.rs` can ignore filesystem churn inside
/// these dirs (build artifacts, dependency installs, git operations
/// fire dozens of events a second otherwise).
pub const HIDDEN_DIRS: &[&str] = &[".git", "node_modules", "target", ".next", "dist", "build"];

/// One visible row in the sidebar.
pub struct TreeNode {
    pub path: PathBuf,
    /// Display name (file or directory basename).
    pub name: String,
    /// 0 for items directly under the root, 1 for items inside a
    /// top-level directory, and so on.
    pub depth: usize,
    pub kind: NodeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    File,
    /// `expanded` flips on click; children are spliced into / removed
    /// from the parent `nodes` vec accordingly.
    Directory {
        expanded: bool,
    },
}

/// The whole sidebar's state. Hidden when `visible == false`, but the
/// loaded node list is retained so a re-show is instant.
pub struct FileTree {
    pub visible: bool,
    /// `true` when the sidebar owns the keyboard. Independent of
    /// `visible`: the panel can be shown without keyboard focus (after
    /// the user clicks back into the editor), and it can be focused
    /// only while visible (focus is force-cleared on hide). The host
    /// uses this to decide whether ↑ / ↓ / Enter / Esc go to the tree
    /// or fall through to the editor.
    pub focused: bool,
    /// Index of the row the user has selected via keyboard, or `None`
    /// when no row is highlighted. Mouse clicks open / toggle without
    /// touching this, so the selection cursor stays where the keyboard
    /// last left it.
    pub selected: Option<usize>,
    /// Root the tree is anchored at. Kept on the struct so a future
    /// "Open Folder" command can rebuild without recreating the whole
    /// state. Reads from this happen via [`reload`](FileTree::reload).
    pub root: PathBuf,
    pub nodes: Vec<TreeNode>,
    /// Vertical scroll position in physical pixels, kept per-tree so the
    /// user's place is preserved across hide/show.
    pub scroll_y: f32,
}

impl FileTree {
    /// Build a tree rooted at `root` with its top-level entries already
    /// loaded. The sidebar starts hidden — callers flip `visible` when
    /// the user toggles it (e.g. via Cmd-B).
    pub fn new(root: PathBuf) -> Self {
        let nodes = read_children(&root, 0);
        Self {
            visible: false,
            focused: false,
            selected: None,
            root,
            nodes,
            scroll_y: 0.0,
        }
    }

    /// Click on the node at row index `idx`. Files open via the caller's
    /// handler (returned as `OpenFile(path)`); directories toggle their
    /// expanded state in place.
    pub fn click(&mut self, idx: usize) -> ClickResult {
        let result = self.activate_at(idx);
        // Mouse interaction moves the selection cursor too so the
        // keyboard picks up where the mouse left off.
        if !self.nodes.is_empty() {
            self.selected = Some(idx.min(self.nodes.len() - 1));
        }
        result
    }

    /// Activate the currently-selected row — equivalent to clicking it.
    /// No-op when nothing is selected. Used by the Enter-key handler.
    pub fn activate_selected(&mut self) -> ClickResult {
        match self.selected {
            Some(idx) => self.activate_at(idx),
            None => ClickResult::Nothing,
        }
    }

    /// Move the selection one row down, wrapping to the top at the end.
    /// Seeds the selection at row 0 if nothing was selected yet.
    pub fn select_next(&mut self) {
        if self.nodes.is_empty() {
            self.selected = None;
            return;
        }
        self.selected = Some(match self.selected {
            None => 0,
            Some(idx) => (idx + 1) % self.nodes.len(),
        });
    }

    /// Move the selection one row up, wrapping to the bottom at the top.
    /// Seeds the selection at the last row if nothing was selected yet.
    pub fn select_prev(&mut self) {
        if self.nodes.is_empty() {
            self.selected = None;
            return;
        }
        let last = self.nodes.len() - 1;
        self.selected = Some(match self.selected {
            None => last,
            Some(0) => last,
            Some(idx) => idx - 1,
        });
    }

    /// Re-read the root directory's entries to pick up filesystem
    /// changes. Re-collapses every directory under the root — useful
    /// for an explicit "Refresh" command but heavy-handed for a
    /// watcher-driven refresh; for that use
    /// [`reload_preserving_expansion`](Self::reload_preserving_expansion).
    #[allow(dead_code)]
    pub fn reload(&mut self) {
        self.nodes = read_children(&self.root, 0);
        self.clamp_selection();
    }

    /// Re-read the tree while keeping every directory that was
    /// expanded before still expanded after — so a `cargo build` /
    /// `npm install` running in the watcher doesn't collapse the
    /// user's open tree on every event. Directories that vanished
    /// since the last read simply don't reappear; directories that
    /// were collapsed stay collapsed.
    pub fn reload_preserving_expansion(&mut self) {
        // 1. Snapshot which directories were expanded.
        let expanded: std::collections::HashSet<PathBuf> = self
            .nodes
            .iter()
            .filter_map(|n| match n.kind {
                NodeKind::Directory { expanded: true } => Some(n.path.clone()),
                _ => None,
            })
            .collect();

        // 2. Rebuild from the root's top-level entries.
        self.nodes = read_children(&self.root, 0);

        // 3. Walk depth-first re-expanding the dirs we remembered. The
        //    flat-vec layout guarantees parents come before children,
        //    so a single linear pass is enough — every dir we hit
        //    either gets expanded immediately or is left collapsed,
        //    and any new children it splices in are then visited by
        //    later iterations of this loop.
        let mut i = 0;
        while i < self.nodes.len() {
            if let NodeKind::Directory { expanded: false } = self.nodes[i].kind {
                if expanded.contains(&self.nodes[i].path) {
                    self.nodes[i].kind = NodeKind::Directory { expanded: true };
                    let parent_path = self.nodes[i].path.clone();
                    let parent_depth = self.nodes[i].depth;
                    let children = read_children(&parent_path, parent_depth + 1);
                    self.nodes.splice(i + 1..i + 1, children);
                }
            }
            i += 1;
        }

        // 4. The expanded list could have grown or shrunk; rein the
        //    selection cursor back into bounds.
        self.clamp_selection();
    }

    /// Clamp `selected` so it always points inside `nodes` (or is
    /// `None` when the list is empty). Called after every reload so
    /// the keyboard cursor doesn't dangle past the end of the list.
    fn clamp_selection(&mut self) {
        if let Some(idx) = self.selected {
            if self.nodes.is_empty() {
                self.selected = None;
            } else if idx >= self.nodes.len() {
                self.selected = Some(self.nodes.len() - 1);
            }
        }
    }

    /// Shared body of [`click`](Self::click) /
    /// [`activate_selected`](Self::activate_selected). Files become an
    /// `OpenFile`; directories toggle expansion in place.
    fn activate_at(&mut self, idx: usize) -> ClickResult {
        let Some(node) = self.nodes.get_mut(idx) else {
            return ClickResult::Nothing;
        };
        match node.kind {
            NodeKind::File => ClickResult::OpenFile(node.path.clone()),
            NodeKind::Directory { expanded } => {
                node.kind = NodeKind::Directory {
                    expanded: !expanded,
                };
                if expanded {
                    self.collapse_at(idx);
                } else {
                    self.expand_at(idx);
                }
                ClickResult::Nothing
            }
        }
    }

    fn expand_at(&mut self, idx: usize) {
        let parent = &self.nodes[idx];
        let parent_path = parent.path.clone();
        let parent_depth = parent.depth;
        let children = read_children(&parent_path, parent_depth + 1);
        // Splice the children in immediately after the parent — this is
        // why the node list is flat: rendering is then a single linear
        // pass over `self.nodes`.
        self.nodes.splice(idx + 1..idx + 1, children);
    }

    fn collapse_at(&mut self, idx: usize) {
        let parent_depth = self.nodes[idx].depth;
        // Children of `idx` are the contiguous run of nodes after it
        // whose depth is greater. Find the end of that run and drain.
        let mut end = idx + 1;
        while end < self.nodes.len() && self.nodes[end].depth > parent_depth {
            end += 1;
        }
        self.nodes.drain(idx + 1..end);
    }
}

/// What a click on a row should make the host do.
#[derive(Debug)]
pub enum ClickResult {
    /// No host-visible effect — the click toggled a directory that's
    /// already been handled in-place on the tree.
    Nothing,
    /// Open this file in the editor.
    OpenFile(PathBuf),
}

/// Read directory entries at `dir`, sort directories before files (each
/// group alphabetical, case-insensitive), and skip the names in
/// [`HIDDEN_DIRS`]. Errors (permission denied, path gone) return an
/// empty vec so the UI degrades gracefully.
fn read_children(dir: &Path, depth: usize) -> Vec<TreeNode> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut nodes: Vec<TreeNode> = entries
        .filter_map(|e| e.ok())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            // Cheap hardcoded filter — leading dot dirs (other than the
            // ones we want to surface for config like `.env`) are kept;
            // only the well-known noisy ones are dropped by name.
            if HIDDEN_DIRS.contains(&name.as_str()) {
                return None;
            }
            let path = entry.path();
            let file_type = entry.file_type().ok()?;
            let kind = if file_type.is_dir() {
                NodeKind::Directory { expanded: false }
            } else {
                NodeKind::File
            };
            Some(TreeNode {
                path,
                name,
                depth,
                kind,
            })
        })
        .collect();
    nodes.sort_by(|a, b| {
        match (
            matches!(a.kind, NodeKind::Directory { .. }),
            matches!(b.kind, NodeKind::Directory { .. }),
        ) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        }
    });
    nodes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        // Unique per-test path without depending on the `tempfile` crate.
        // (PID, atomic counter) avoids the nanosecond-resolution collision
        // we hit when CI runs tests in parallel — `subsec_nanos()` alone
        // can land on the same value across threads.
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let pid = std::process::id();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("editor-app-filetree-{pid}-{n}"));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn root_lists_entries_sorted_dirs_first() {
        let root = tempdir();
        std::fs::create_dir(root.join("zeta-dir")).unwrap();
        std::fs::write(root.join("a-file.txt"), "").unwrap();
        std::fs::create_dir(root.join("alpha-dir")).unwrap();
        std::fs::write(root.join("zzz-file.txt"), "").unwrap();

        let tree = FileTree::new(root.clone());
        let names: Vec<&str> = tree.nodes.iter().map(|n| n.name.as_str()).collect();
        // dirs (alpha) before files; each group alphabetical
        assert_eq!(
            names,
            vec!["alpha-dir", "zeta-dir", "a-file.txt", "zzz-file.txt"]
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn hidden_dirs_are_skipped() {
        let root = tempdir();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        std::fs::create_dir(root.join("node_modules")).unwrap();
        std::fs::create_dir(root.join("target")).unwrap();

        let tree = FileTree::new(root.clone());
        let names: Vec<&str> = tree.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["src"]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn expanding_a_directory_splices_children_in() {
        let root = tempdir();
        std::fs::create_dir(root.join("inner")).unwrap();
        std::fs::write(root.join("inner/a.txt"), "").unwrap();
        std::fs::write(root.join("inner/b.txt"), "").unwrap();
        std::fs::write(root.join("outside.txt"), "").unwrap();

        let mut tree = FileTree::new(root.clone());
        assert_eq!(tree.nodes.len(), 2);
        let _ = tree.click(0); // expand inner/
        assert_eq!(tree.nodes.len(), 4);
        assert_eq!(tree.nodes[0].depth, 0);
        assert_eq!(tree.nodes[1].depth, 1);
        assert_eq!(tree.nodes[2].depth, 1);
        // collapse again
        let _ = tree.click(0);
        assert_eq!(tree.nodes.len(), 2);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn clicking_a_file_yields_open_file() {
        let root = tempdir();
        std::fs::write(root.join("hello.rs"), "").unwrap();
        let mut tree = FileTree::new(root.clone());
        match tree.click(0) {
            ClickResult::OpenFile(p) => assert_eq!(p, root.join("hello.rs")),
            other => panic!("expected OpenFile, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn select_next_seeds_and_advances_with_wrap() {
        let root = tempdir();
        std::fs::write(root.join("a.txt"), "").unwrap();
        std::fs::write(root.join("b.txt"), "").unwrap();
        std::fs::write(root.join("c.txt"), "").unwrap();
        let mut tree = FileTree::new(root.clone());
        assert_eq!(tree.selected, None);

        tree.select_next();
        assert_eq!(tree.selected, Some(0));
        tree.select_next();
        assert_eq!(tree.selected, Some(1));
        tree.select_next();
        assert_eq!(tree.selected, Some(2));
        // Past the end → wrap to top.
        tree.select_next();
        assert_eq!(tree.selected, Some(0));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn select_prev_seeds_at_bottom_and_wraps() {
        let root = tempdir();
        std::fs::write(root.join("a.txt"), "").unwrap();
        std::fs::write(root.join("b.txt"), "").unwrap();
        let mut tree = FileTree::new(root.clone());

        tree.select_prev();
        // Nothing was selected → seed at the *last* row, not the first.
        assert_eq!(tree.selected, Some(1));
        tree.select_prev();
        assert_eq!(tree.selected, Some(0));
        tree.select_prev();
        // Wrap past the top → bottom.
        assert_eq!(tree.selected, Some(1));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn activate_selected_opens_file_or_toggles_dir() {
        let root = tempdir();
        std::fs::create_dir(root.join("inner")).unwrap();
        std::fs::write(root.join("inner/x.txt"), "").unwrap();
        std::fs::write(root.join("y.txt"), "").unwrap();
        let mut tree = FileTree::new(root.clone());
        // [inner, y.txt]
        tree.selected = Some(0);
        let r = tree.activate_selected();
        // Directory expands in place.
        assert!(matches!(r, ClickResult::Nothing));
        assert_eq!(tree.nodes.len(), 3); // inner + x.txt + y.txt

        // Move down to the expanded child and open it.
        tree.select_next();
        assert_eq!(tree.selected, Some(1));
        match tree.activate_selected() {
            ClickResult::OpenFile(p) => assert_eq!(p, root.join("inner/x.txt")),
            other => panic!("expected OpenFile, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn reload_clamps_selection_to_new_node_count() {
        let root = tempdir();
        std::fs::write(root.join("a.txt"), "").unwrap();
        std::fs::write(root.join("b.txt"), "").unwrap();
        std::fs::write(root.join("c.txt"), "").unwrap();
        let mut tree = FileTree::new(root.clone());
        tree.selected = Some(2);

        // Drop the bottom file and reload — selection was off the end.
        std::fs::remove_file(root.join("c.txt")).unwrap();
        tree.reload();
        assert_eq!(tree.selected, Some(1));

        // Drain everything and the selection clears.
        std::fs::remove_file(root.join("a.txt")).unwrap();
        std::fs::remove_file(root.join("b.txt")).unwrap();
        tree.reload();
        assert_eq!(tree.selected, None);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn reload_preserving_expansion_keeps_open_dirs_open() {
        let root = tempdir();
        std::fs::create_dir(root.join("inner")).unwrap();
        std::fs::write(root.join("inner/a.txt"), "").unwrap();
        std::fs::write(root.join("other.txt"), "").unwrap();
        let mut tree = FileTree::new(root.clone());
        // [inner, other.txt]
        let _ = tree.click(0); // expand inner/
        assert_eq!(tree.nodes.len(), 3);
        assert!(matches!(
            tree.nodes[0].kind,
            NodeKind::Directory { expanded: true }
        ));

        // Simulate the watcher firing after a new file lands in inner/.
        std::fs::write(root.join("inner/b.txt"), "").unwrap();
        tree.reload_preserving_expansion();
        // inner/ stays expanded and picks up b.txt.
        assert!(matches!(
            tree.nodes[0].kind,
            NodeKind::Directory { expanded: true }
        ));
        let names: Vec<&str> = tree.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["inner", "a.txt", "b.txt", "other.txt"]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn reload_preserving_expansion_drops_vanished_dirs() {
        let root = tempdir();
        std::fs::create_dir(root.join("doomed")).unwrap();
        std::fs::write(root.join("doomed/x.txt"), "").unwrap();
        std::fs::write(root.join("survivor.txt"), "").unwrap();
        let mut tree = FileTree::new(root.clone());
        let _ = tree.click(0); // expand doomed/
        assert_eq!(tree.nodes.len(), 3);

        // doomed/ gets nuked entirely between reloads.
        std::fs::remove_dir_all(root.join("doomed")).unwrap();
        tree.reload_preserving_expansion();
        let names: Vec<&str> = tree.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["survivor.txt"]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn click_updates_selection_to_clicked_row() {
        let root = tempdir();
        std::fs::write(root.join("a.txt"), "").unwrap();
        std::fs::write(root.join("b.txt"), "").unwrap();
        let mut tree = FileTree::new(root.clone());
        let _ = tree.click(1);
        assert_eq!(tree.selected, Some(1));
        std::fs::remove_dir_all(&root).ok();
    }
}
