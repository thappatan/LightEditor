//! Project-wide search (spec §4.1.3 — find-in-files).
//!
//! Cmd-Shift-F opens an overlay panel; typing a query and pressing
//! Enter walks the workspace via `ignore::WalkBuilder` (so `.gitignore`
//! is respected and the usual `target/` + `node_modules/` noise is
//! skipped for free) and runs a case-insensitive substring match per
//! line of every UTF-8-readable text file. Results are grouped by
//! path, displayed in the panel, and clickable to jump to that line.
//!
//! v1 limits: synchronous search (runs on the host thread when the
//! user hits Enter), no regex, case-insensitive only, capped at
//! [`MAX_RESULTS`] matches and [`MAX_FILE_BYTES`] per file. Real
//! ripgrep-grade async + regex modes come later — the API surface
//! here keeps that upgrade path open by hiding the actual search
//! behind a single [`search`] function.

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use regex::RegexBuilder;

/// Stop after this many matches across all files — keeps the panel
/// from overflowing and the host thread from spending seconds chasing
/// a too-common substring.
const MAX_RESULTS: usize = 500;
/// Don't open files larger than this for substring matching. Binary
/// blobs slip in occasionally (lock files, minified js) and dwarf the
/// useful matches.
const MAX_FILE_BYTES: u64 = 1_000_000;

/// One line that matched the query, plus enough context for the UI to
/// open it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindMatch {
    pub path: PathBuf,
    /// 0-based line index of the matching line in `path`.
    pub line: usize,
    /// The full text of the matching line (no trailing newline).
    pub line_text: String,
}

/// Panel state: query the user has typed + the results the last search
/// produced + which row is selected. `None` on `State` means the panel
/// is closed; `Some` means it's visible.
pub struct FindInFiles {
    pub query: String,
    /// Results from the most recent [`search`] call, in walker order
    /// (depth-first, alphabetical within each directory).
    pub results: Vec<FindMatch>,
    /// Currently-highlighted row index into `results`. 0 when empty.
    pub selected: usize,
    /// First visible row when `results.len()` exceeds the panel's row
    /// budget — used to keep the selection in view while paging.
    pub scroll: usize,
    /// True when the input row has keyboard focus (typing edits
    /// `query`); false when focus is on the results list (↑/↓ + Enter
    /// navigate results).
    pub input_focused: bool,
}

impl FindInFiles {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            results: Vec::new(),
            selected: 0,
            scroll: 0,
            input_focused: true,
        }
    }

    pub fn select_next(&mut self) {
        if self.results.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.results.len();
    }

    pub fn select_prev(&mut self) {
        if self.results.is_empty() {
            return;
        }
        self.selected = if self.selected == 0 {
            self.results.len() - 1
        } else {
            self.selected - 1
        };
    }
}

impl Default for FindInFiles {
    fn default() -> Self {
        Self::new()
    }
}

/// Walk the file tree rooted at `root` (respecting `.gitignore`) and
/// return every line containing `query` (case-insensitive substring
/// match). Capped at [`MAX_RESULTS`] hits.
///
/// Empty query → empty results, no walk. Anything that fails to parse
/// as UTF-8, or that's bigger than [`MAX_FILE_BYTES`], is skipped.
pub fn search(query: &str, root: &Path) -> Vec<FindMatch> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    // Escape so the user's input is treated as a literal, not a regex.
    // Regex polish comes later behind a UI toggle.
    let Ok(re) = RegexBuilder::new(&regex::escape(query))
        .case_insensitive(true)
        .build()
    else {
        return Vec::new();
    };

    let mut results: Vec<FindMatch> = Vec::new();
    for entry in WalkBuilder::new(root).hidden(false).build().flatten() {
        if results.len() >= MAX_RESULTS {
            break;
        }
        // Skip directories and symlinks — only walk regular files.
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if meta.len() > MAX_FILE_BYTES {
                continue;
            }
        }
        let path = entry.path();
        // Read as `String`; non-UTF8 (binary) blobs fail here and are
        // silently dropped. That covers PNG / lock-file / .so noise
        // without us having to write a separate binary heuristic.
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        for (line_idx, line) in content.lines().enumerate() {
            if re.is_match(line) {
                results.push(FindMatch {
                    path: path.to_path_buf(),
                    line: line_idx,
                    line_text: line.to_string(),
                });
                if results.len() >= MAX_RESULTS {
                    break;
                }
            }
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tempdir() -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let pid = std::process::id();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("editor-app-find-{pid}-{n}"));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn matches_a_substring_per_line() {
        let root = tempdir();
        std::fs::write(root.join("a.txt"), "hello world\nnope\nhello again\n").unwrap();
        let hits = search("hello", &root);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].line, 0);
        assert_eq!(hits[1].line, 2);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn case_insensitive_match() {
        let root = tempdir();
        std::fs::write(root.join("a.txt"), "Hello\nWORLD\nfoo\n").unwrap();
        let hits = search("hello", &root);
        assert_eq!(hits.len(), 1);
        let hits = search("WORLD", &root);
        assert_eq!(hits.len(), 1);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn empty_query_returns_no_results() {
        let root = tempdir();
        std::fs::write(root.join("a.txt"), "anything\n").unwrap();
        assert!(search("", &root).is_empty());
        assert!(search("   ", &root).is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn binary_files_are_skipped() {
        let root = tempdir();
        std::fs::write(root.join("text.txt"), "hello\n").unwrap();
        // Bytes that don't form valid UTF-8 → `read_to_string` fails →
        // the file is silently skipped.
        std::fs::write(root.join("blob.bin"), [0u8, 159, 146, 150, 0, 0]).unwrap();
        let hits = search("hello", &root);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].path.ends_with("text.txt"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn gitignored_paths_are_skipped_when_inside_a_repo() {
        use std::process::Command;
        let root = tempdir();
        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .ok();
        };
        run(&["-c", "init.defaultBranch=main", "init", "-q"]);
        std::fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(root.join("kept.txt"), "needle\n").unwrap();
        std::fs::write(root.join("ignored.txt"), "needle\n").unwrap();
        let hits = search("needle", &root);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].path.ends_with("kept.txt"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn select_next_and_prev_wrap() {
        let mut f = FindInFiles::new();
        f.results = vec![
            FindMatch {
                path: PathBuf::from("a"),
                line: 0,
                line_text: "x".into(),
            },
            FindMatch {
                path: PathBuf::from("b"),
                line: 0,
                line_text: "y".into(),
            },
        ];
        assert_eq!(f.selected, 0);
        f.select_next();
        assert_eq!(f.selected, 1);
        f.select_next();
        assert_eq!(f.selected, 0); // wraps
        f.select_prev();
        assert_eq!(f.selected, 1); // wraps the other way
    }
}
