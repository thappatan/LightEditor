//! File-tree sidebar state (spec §4.1.2, §4.1.5).
//!
//! A flat `Vec<TreeNode>` representing the *visible* rows in the tree
//! sidebar. Expanding a directory splices its children into the vec at
//! the directory's index + 1; collapsing removes that contiguous range.
//! Rendering is then a straight iteration — no recursive traversal at
//! draw time.
//!
//! **Multi-root** (spec §4.1.5): the sidebar holds an ordered list of
//! workspace roots, each shown as a collapsible top-level header
//! ([`depth`](TreeNode::depth) 0) with its contents nested at depth 1+.
//! A single open folder is just the one-root case. Roots are added /
//! removed at runtime ([`add_root`](FileTree::add_root) /
//! [`remove_root`](FileTree::remove_root)); the order is preserved.
//!
//! Filesystem reads are lazy: a directory's children load only on its
//! first expand.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Built-in default hidden-dir list. The user-facing source of truth
/// is `settings.toml`'s `[file_tree] hidden_dirs = [...]` (see
/// [`editor_config::FileTreeSettings`]); this constant just keeps
/// the unit tests' seed list in sync with the config crate's defaults
/// without taking a dev-dependency on it.
#[allow(dead_code)]
pub const DEFAULT_HIDDEN_DIRS: &[&str] =
    &[".git", "node_modules", "target", ".next", "dist", "build"];

/// One visible row in the sidebar.
pub struct TreeNode {
    pub path: PathBuf,
    /// Display name (file or directory basename, or root label).
    pub name: String,
    /// 0 for workspace-root headers, 1 for items directly inside a root,
    /// 2 for items one directory deeper, and so on.
    pub depth: usize,
    pub kind: NodeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    File,
    /// `expanded` flips on click; children are spliced into / removed
    /// from the parent `nodes` vec accordingly. Workspace-root headers
    /// use this variant too (`is_root: true`), so expand/collapse works
    /// on them uniformly.
    Directory {
        expanded: bool,
        /// `true` when this directory is a workspace root header rather
        /// than a directory nested inside one. Root headers can't be
        /// collapsed away — only their *children* are hidden — and only
        /// they are valid targets for "Remove Folder from Workspace".
        is_root: bool,
    },
}

/// One workspace root: the folder path plus the label shown on its
/// header row (defaults to the folder's basename).
#[derive(Debug, Clone)]
pub struct RootEntry {
    pub path: PathBuf,
    pub name: String,
}

impl RootEntry {
    fn from_path(path: PathBuf) -> Self {
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        Self { path, name }
    }
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
    /// Ordered workspace roots. One entry for a single open folder,
    /// more after "Add Folder to Workspace". Each renders as a top-level
    /// header row; the order here is the order shown.
    pub roots: Vec<RootEntry>,
    pub nodes: Vec<TreeNode>,
    /// Vertical scroll position in physical pixels, kept per-tree so the
    /// user's place is preserved across hide/show.
    pub scroll_y: f32,
    /// Directory names hidden from the listing. Sourced from
    /// `settings.toml`'s `[file_tree] hidden_dirs = [...]`; defaults
    /// to [`DEFAULT_HIDDEN_DIRS`] when nothing is set.
    pub hidden_dirs: Vec<String>,
}

impl FileTree {
    /// Build a tree over `roots` with each root's header + top-level
    /// entries loaded (roots start expanded). The sidebar starts hidden
    /// — callers flip `visible` when the user toggles it (e.g. via
    /// Cmd-B). `hidden_dirs` is the list of directory basenames the
    /// listing should skip; pass the effective
    /// `settings.file_tree.hidden_dirs`.
    pub fn new(roots: Vec<PathBuf>, hidden_dirs: Vec<String>) -> Self {
        let roots: Vec<RootEntry> = roots.into_iter().map(RootEntry::from_path).collect();
        let mut tree = Self {
            visible: false,
            focused: false,
            selected: None,
            roots,
            nodes: Vec::new(),
            scroll_y: 0.0,
            hidden_dirs,
        };
        // Roots start expanded so the user sees their files immediately.
        let expanded: HashSet<PathBuf> = tree.roots.iter().map(|r| r.path.clone()).collect();
        tree.rebuild(&expanded);
        tree
    }

    /// The first (primary) workspace root, if any. Several
    /// not-yet-multi-root-aware features (git status, npm scripts,
    /// Flutter detection, find-in-files default scope) anchor on this
    /// until they're taught to span every root.
    pub fn primary_root(&self) -> Option<&Path> {
        self.roots.first().map(|r| r.path.as_path())
    }

    /// Add `path` as a new workspace root (appended last) and rebuild,
    /// keeping existing expansion. No-op if the path is already a root.
    pub fn add_root(&mut self, path: PathBuf) {
        if self.roots.iter().any(|r| r.path == path) {
            return;
        }
        self.roots.push(RootEntry::from_path(path.clone()));
        let mut expanded = self.expanded_paths();
        expanded.insert(path); // show the new root's contents right away
        self.rebuild(&expanded);
    }

    /// Remove the workspace root at `path`. No-op if it isn't a root or
    /// if it's the only one (a workspace keeps at least one folder; use
    /// "Close Folder" to go back to an empty editor — not yet wired).
    /// Returns `true` if a root was removed.
    pub fn remove_root(&mut self, path: &Path) -> bool {
        if self.roots.len() <= 1 || !self.roots.iter().any(|r| r.path == path) {
            return false;
        }
        self.roots.retain(|r| r.path != path);
        let expanded = self.expanded_paths();
        self.rebuild(&expanded);
        true
    }

    /// The workspace root that contains the node at `idx`, found by
    /// walking back to the nearest `depth == 0` header. Used by "Remove
    /// Folder from Workspace" to act on the user's current location.
    pub fn root_path_at(&self, idx: usize) -> Option<PathBuf> {
        let mut i = idx.min(self.nodes.len().saturating_sub(1));
        loop {
            let node = self.nodes.get(i)?;
            if node.depth == 0 {
                return Some(node.path.clone());
            }
            i = i.checked_sub(1)?;
        }
    }

    /// Snapshot the set of currently-expanded directory paths (root
    /// headers + nested dirs) so a rebuild can restore them.
    fn expanded_paths(&self) -> HashSet<PathBuf> {
        self.nodes
            .iter()
            .filter_map(|n| match n.kind {
                NodeKind::Directory { expanded: true, .. } => Some(n.path.clone()),
                _ => None,
            })
            .collect()
    }

    /// Rebuild `nodes` from `roots`: emit each root header (collapsed),
    /// then a single linear pass re-expands every directory whose path
    /// is in `expanded`, splicing children in as it goes. The flat-vec
    /// layout guarantees parents precede children, so one pass suffices.
    fn rebuild(&mut self, expanded: &HashSet<PathBuf>) {
        self.nodes = self
            .roots
            .iter()
            .map(|r| TreeNode {
                path: r.path.clone(),
                name: r.name.clone(),
                depth: 0,
                kind: NodeKind::Directory {
                    expanded: false,
                    is_root: true,
                },
            })
            .collect();
        let mut i = 0;
        while i < self.nodes.len() {
            if let NodeKind::Directory {
                expanded: false,
                is_root,
            } = self.nodes[i].kind
            {
                if expanded.contains(&self.nodes[i].path) {
                    self.nodes[i].kind = NodeKind::Directory {
                        expanded: true,
                        is_root,
                    };
                    let path = self.nodes[i].path.clone();
                    let depth = self.nodes[i].depth;
                    let children = read_children(&path, depth + 1, &self.hidden_dirs);
                    self.nodes.splice(i + 1..i + 1, children);
                }
            }
            i += 1;
        }
        self.clamp_selection();
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

    /// Re-read every root, collapsing all directories (roots stay
    /// expanded). Heavy-handed; the watcher path uses
    /// [`reload_preserving_expansion`](Self::reload_preserving_expansion).
    #[allow(dead_code)]
    pub fn reload(&mut self) {
        let expanded: HashSet<PathBuf> = self.roots.iter().map(|r| r.path.clone()).collect();
        self.rebuild(&expanded);
    }

    /// Re-read every root while keeping each directory that was expanded
    /// before still expanded after — so a `cargo build` / `npm install`
    /// running in the watcher doesn't collapse the user's open tree on
    /// every event. Directories that vanished simply don't reappear.
    pub fn reload_preserving_expansion(&mut self) {
        let expanded = self.expanded_paths();
        self.rebuild(&expanded);
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
            NodeKind::Directory { expanded, is_root } => {
                node.kind = NodeKind::Directory {
                    expanded: !expanded,
                    is_root,
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
        let children = read_children(&parent_path, parent_depth + 1, &self.hidden_dirs);
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
/// group alphabetical, case-insensitive), and skip names in
/// `hidden_dirs`. Errors (permission denied, path gone) return an
/// empty vec so the UI degrades gracefully.
fn read_children(dir: &Path, depth: usize, hidden_dirs: &[String]) -> Vec<TreeNode> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut nodes: Vec<TreeNode> = entries
        .filter_map(|e| e.ok())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            // Filter against the user's hidden-dirs list. Leading-dot
            // dirs that don't appear in the list (`.env`, `.lighteditor`,
            // …) still show up so config / dotfile workflows work.
            if hidden_dirs.iter().any(|d| d == &name) {
                return None;
            }
            let path = entry.path();
            let file_type = entry.file_type().ok()?;
            let kind = if file_type.is_dir() {
                NodeKind::Directory {
                    expanded: false,
                    is_root: false,
                }
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

    /// The same list the [`Default`] impl of `FileTreeSettings` uses —
    /// kept inline so the unit tests don't need a config dep.
    fn default_hidden() -> Vec<String> {
        DEFAULT_HIDDEN_DIRS.iter().map(|s| s.to_string()).collect()
    }

    /// Names of the non-header rows (depth > 0), in display order.
    fn child_names(tree: &FileTree) -> Vec<&str> {
        tree.nodes
            .iter()
            .filter(|n| n.depth > 0)
            .map(|n| n.name.as_str())
            .collect()
    }

    #[test]
    fn root_header_then_entries_sorted_dirs_first() {
        let root = tempdir();
        std::fs::create_dir(root.join("zeta-dir")).unwrap();
        std::fs::write(root.join("a-file.txt"), "").unwrap();
        std::fs::create_dir(root.join("alpha-dir")).unwrap();
        std::fs::write(root.join("zzz-file.txt"), "").unwrap();

        let tree = FileTree::new(vec![root.clone()], default_hidden());
        // Row 0 is the root header (depth 0, expanded).
        assert_eq!(tree.nodes[0].depth, 0);
        assert!(matches!(
            tree.nodes[0].kind,
            NodeKind::Directory {
                expanded: true,
                is_root: true
            }
        ));
        // Children: dirs (alpha) before files; each group alphabetical.
        assert_eq!(
            child_names(&tree),
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

        let tree = FileTree::new(vec![root.clone()], default_hidden());
        assert_eq!(child_names(&tree), vec!["src"]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn expanding_a_directory_splices_children_in() {
        let root = tempdir();
        std::fs::create_dir(root.join("inner")).unwrap();
        std::fs::write(root.join("inner/a.txt"), "").unwrap();
        std::fs::write(root.join("inner/b.txt"), "").unwrap();
        std::fs::write(root.join("outside.txt"), "").unwrap();

        let mut tree = FileTree::new(vec![root.clone()], default_hidden());
        // [root, inner, outside.txt]
        assert_eq!(tree.nodes.len(), 3);
        let _ = tree.click(1); // expand inner/
        assert_eq!(tree.nodes.len(), 5);
        assert_eq!(tree.nodes[1].depth, 1); // inner
        assert_eq!(tree.nodes[2].depth, 2); // inner/a.txt
                                            // collapse again
        let _ = tree.click(1);
        assert_eq!(tree.nodes.len(), 3);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn clicking_a_file_yields_open_file() {
        let root = tempdir();
        std::fs::write(root.join("hello.rs"), "").unwrap();
        let mut tree = FileTree::new(vec![root.clone()], default_hidden());
        // [root, hello.rs]
        match tree.click(1) {
            ClickResult::OpenFile(p) => assert_eq!(p, root.join("hello.rs")),
            other => panic!("expected OpenFile, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn clicking_root_header_collapses_its_children() {
        let root = tempdir();
        std::fs::write(root.join("a.txt"), "").unwrap();
        let mut tree = FileTree::new(vec![root.clone()], default_hidden());
        assert_eq!(tree.nodes.len(), 2); // [root, a.txt]
        let _ = tree.click(0); // collapse the root
        assert_eq!(tree.nodes.len(), 1); // just the header
        let _ = tree.click(0); // expand again
        assert_eq!(tree.nodes.len(), 2);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn select_next_seeds_and_advances_with_wrap() {
        let root = tempdir();
        std::fs::write(root.join("a.txt"), "").unwrap();
        std::fs::write(root.join("b.txt"), "").unwrap();
        let mut tree = FileTree::new(vec![root.clone()], default_hidden());
        // [root, a.txt, b.txt]
        assert_eq!(tree.selected, None);
        tree.select_next();
        assert_eq!(tree.selected, Some(0));
        tree.select_next();
        assert_eq!(tree.selected, Some(1));
        tree.select_next();
        assert_eq!(tree.selected, Some(2));
        tree.select_next(); // wrap
        assert_eq!(tree.selected, Some(0));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn select_prev_seeds_at_bottom_and_wraps() {
        let root = tempdir();
        std::fs::write(root.join("a.txt"), "").unwrap();
        let mut tree = FileTree::new(vec![root.clone()], default_hidden());
        // [root, a.txt]
        tree.select_prev();
        assert_eq!(tree.selected, Some(1)); // seed at last row
        tree.select_prev();
        assert_eq!(tree.selected, Some(0));
        tree.select_prev();
        assert_eq!(tree.selected, Some(1)); // wrap
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn reload_clamps_selection_to_new_node_count() {
        let root = tempdir();
        std::fs::write(root.join("a.txt"), "").unwrap();
        std::fs::write(root.join("b.txt"), "").unwrap();
        let mut tree = FileTree::new(vec![root.clone()], default_hidden());
        // [root, a.txt, b.txt]
        tree.selected = Some(2);
        std::fs::remove_file(root.join("b.txt")).unwrap();
        tree.reload();
        assert_eq!(tree.selected, Some(1));

        // Even with no files the root header remains, so selection
        // clamps to the header rather than clearing.
        std::fs::remove_file(root.join("a.txt")).unwrap();
        tree.reload();
        assert_eq!(tree.selected, Some(0));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn reload_preserving_expansion_keeps_open_dirs_open() {
        let root = tempdir();
        std::fs::create_dir(root.join("inner")).unwrap();
        std::fs::write(root.join("inner/a.txt"), "").unwrap();
        std::fs::write(root.join("other.txt"), "").unwrap();
        let mut tree = FileTree::new(vec![root.clone()], default_hidden());
        // [root, inner, other.txt]
        let _ = tree.click(1); // expand inner/
        assert!(matches!(
            tree.nodes[1].kind,
            NodeKind::Directory { expanded: true, .. }
        ));

        std::fs::write(root.join("inner/b.txt"), "").unwrap();
        tree.reload_preserving_expansion();
        assert!(matches!(
            tree.nodes[1].kind,
            NodeKind::Directory { expanded: true, .. }
        ));
        assert_eq!(
            child_names(&tree),
            vec!["inner", "a.txt", "b.txt", "other.txt"]
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn reload_preserving_expansion_drops_vanished_dirs() {
        let root = tempdir();
        std::fs::create_dir(root.join("doomed")).unwrap();
        std::fs::write(root.join("doomed/x.txt"), "").unwrap();
        std::fs::write(root.join("survivor.txt"), "").unwrap();
        let mut tree = FileTree::new(vec![root.clone()], default_hidden());
        let _ = tree.click(1); // expand doomed/
        std::fs::remove_dir_all(root.join("doomed")).unwrap();
        tree.reload_preserving_expansion();
        assert_eq!(child_names(&tree), vec!["survivor.txt"]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn click_updates_selection_to_clicked_row() {
        let root = tempdir();
        std::fs::write(root.join("a.txt"), "").unwrap();
        std::fs::write(root.join("b.txt"), "").unwrap();
        let mut tree = FileTree::new(vec![root.clone()], default_hidden());
        let _ = tree.click(2);
        assert_eq!(tree.selected, Some(2));
        std::fs::remove_dir_all(&root).ok();
    }

    // ── multi-root (spec §4.1.5) ──────────────────────────────────────────

    #[test]
    fn add_root_appends_and_shows_a_second_header() {
        let a = tempdir();
        std::fs::write(a.join("a.txt"), "").unwrap();
        let b = tempdir();
        std::fs::write(b.join("b.txt"), "").unwrap();

        let mut tree = FileTree::new(vec![a.clone()], default_hidden());
        assert_eq!(tree.roots.len(), 1);
        tree.add_root(b.clone());
        assert_eq!(tree.roots.len(), 2);
        // Two headers (depth 0) plus a file under each.
        let headers: Vec<&str> = tree
            .nodes
            .iter()
            .filter(|n| n.depth == 0)
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(headers.len(), 2);
        assert!(child_names(&tree).contains(&"a.txt"));
        assert!(child_names(&tree).contains(&"b.txt"));
        // Re-adding the same path is a no-op.
        tree.add_root(b.clone());
        assert_eq!(tree.roots.len(), 2);
        std::fs::remove_dir_all(&a).ok();
        std::fs::remove_dir_all(&b).ok();
    }

    #[test]
    fn remove_root_drops_one_but_refuses_the_last() {
        let a = tempdir();
        let b = tempdir();
        let mut tree = FileTree::new(vec![a.clone(), b.clone()], default_hidden());
        assert!(tree.remove_root(&b));
        assert_eq!(tree.roots.len(), 1);
        assert_eq!(tree.roots[0].path, a);
        // Can't remove the only remaining root.
        assert!(!tree.remove_root(&a));
        assert_eq!(tree.roots.len(), 1);
        std::fs::remove_dir_all(&a).ok();
        std::fs::remove_dir_all(&b).ok();
    }

    #[test]
    fn root_path_at_walks_back_to_the_header() {
        let a = tempdir();
        std::fs::write(a.join("a.txt"), "").unwrap();
        let b = tempdir();
        std::fs::write(b.join("b.txt"), "").unwrap();
        let mut tree = FileTree::new(vec![a.clone()], default_hidden());
        tree.add_root(b.clone());
        // Find the row for b.txt and confirm it maps back to root b.
        let idx = tree
            .nodes
            .iter()
            .position(|n| n.name == "b.txt")
            .expect("b.txt row");
        assert_eq!(tree.root_path_at(idx), Some(b.clone()));
        std::fs::remove_dir_all(&a).ok();
        std::fs::remove_dir_all(&b).ok();
    }
}
