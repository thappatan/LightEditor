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
/// coded for v1; surfacing as a setting is a follow-up.
const HIDDEN_DIRS: &[&str] = &[".git", "node_modules", "target", ".next", "dist", "build"];

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
    /// Root the tree is anchored at. Kept on the struct so a future
    /// "Open Folder" command can rebuild without recreating the whole
    /// state. Reads from this happen via [`reload`](FileTree::reload).
    #[allow(dead_code)]
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
            root,
            nodes,
            scroll_y: 0.0,
        }
    }

    /// Click on the node at row index `idx`. Files open via the caller's
    /// handler (returned as `OpenFile(path)`); directories toggle their
    /// expanded state in place.
    pub fn click(&mut self, idx: usize) -> ClickResult {
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

    /// Re-read the root directory's entries to pick up filesystem
    /// changes. Currently nukes the tree state (re-collapses everything
    /// under the root); a watcher-driven incremental refresh is a
    /// follow-up.
    #[allow(dead_code)]
    pub fn reload(&mut self) {
        self.nodes = read_children(&self.root, 0);
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
        // A simple `target/test-tmp-<rand>` so we don't depend on a tempdir
        // crate. Each invocation makes a unique path; the test cleans up.
        let n: u32 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("editor-app-filetree-{n}"));
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
}
