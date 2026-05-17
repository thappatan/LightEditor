//! Per-line git diff status (spec §4.6, partial).
//!
//! Diffs the active document's buffer text against the file's blob in
//! HEAD, classifying each new-file line as `Added`, `Modified`, or
//! `Deleted` (marker shown on the line below the cut). The gutter
//! renders a small coloured bar per status, like every modern editor.
//!
//! libgit2 is doing the work; we just project its hunk output into a
//! `HashMap<line_index, status>`. On files not in HEAD (newly tracked,
//! untracked, or no repo at all) the helper returns an empty map —
//! the gutter just shows no markers, no panic.

use std::collections::HashMap;
use std::path::Path;

use git2::{DiffOptions, Patch, Repository};

/// What changed on a given line vs HEAD. Mirrors the standard
/// editor-gutter convention: green bar (added), blue bar (modified),
/// red wedge (one or more lines were deleted just above this line).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitLineStatus {
    Added,
    Modified,
    Deleted,
}

/// Diff `current_text` against the file's HEAD blob and return a map
/// of new-file line index (0-based) to its [`GitLineStatus`]. An empty
/// map means "nothing changed" *or* "we couldn't compute the diff"
/// (no repo, file not tracked, libgit2 error) — callers should not
/// distinguish those visually in v1.
pub fn compute_line_status(path: &Path, current_text: &str) -> HashMap<usize, GitLineStatus> {
    let mut status: HashMap<usize, GitLineStatus> = HashMap::new();

    // Canonicalise so the prefix-strip against libgit2's workdir works
    // on systems where the caller's path is a symlink (macOS's
    // `/var` → `/private/var` is the usual culprit on /tmp tests, and
    // `~/Documents` etc. can hit similar resolutions).
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    // Walk up from the file looking for a `.git` directory. Returns
    // None for files outside any git repo, which is a normal case
    // (untitled scratch buffers, files opened from /tmp, etc.).
    let Some(parent) = path.parent() else {
        return status;
    };
    let Ok(repo) = Repository::discover(parent) else {
        return status;
    };

    // Convert the file's absolute path to one relative to the repo's
    // workdir; `Tree::get_path` needs a repo-relative path. Canonicalise
    // workdir too so the comparison sees matching strings on macOS.
    let Some(workdir) = repo.workdir() else {
        return status;
    };
    let workdir = workdir
        .canonicalize()
        .unwrap_or_else(|_| workdir.to_path_buf());
    let Ok(rel_path) = path.strip_prefix(&workdir) else {
        return status;
    };

    // Resolve the HEAD tree's entry for this file. A missing entry
    // means the file is new to the repo — every current line is an
    // addition.
    let Ok(head_tree) = repo.head().and_then(|h| h.peel_to_tree()) else {
        return status;
    };
    let Ok(entry) = head_tree.get_path(rel_path) else {
        for (i, _) in current_text.split('\n').enumerate() {
            status.insert(i, GitLineStatus::Added);
        }
        return status;
    };
    let Ok(blob_obj) = entry.to_object(&repo) else {
        return status;
    };
    let Ok(blob) = blob_obj.peel_to_blob() else {
        return status;
    };

    // 0-context diff so the hunks only carry the touched lines, no
    // surrounding context noise.
    let mut opts = DiffOptions::new();
    opts.context_lines(0);
    let patch = match Patch::from_blob_and_buffer(
        &blob,
        None,
        current_text.as_bytes(),
        None,
        Some(&mut opts),
    ) {
        Ok(p) => p,
        Err(_) => return status,
    };

    let num_hunks = patch.num_hunks();
    for hunk_idx in 0..num_hunks {
        let Ok((hunk, num_lines)) = patch.hunk(hunk_idx) else {
            continue;
        };
        let hunk_new_start = hunk.new_start();

        // Collect each line's role for this hunk before classifying:
        // '-' lines have no new_lineno, '+' lines do. libgit2 returns
        // the origin marker as a byte (b'+', b'-', b' '), not a char.
        let mut adds_in_hunk: Vec<u32> = Vec::new();
        let mut removes_in_hunk: usize = 0;
        for line_idx in 0..num_lines {
            let Ok(line) = patch.line_in_hunk(hunk_idx, line_idx) else {
                continue;
            };
            match line.origin() as u8 {
                b'+' => {
                    if let Some(n) = line.new_lineno() {
                        adds_in_hunk.push(n);
                    }
                }
                b'-' => removes_in_hunk += 1,
                _ => {}
            }
        }

        // Standard VS-Code-ish classification: the first N adds (where
        // N = min(adds, removes)) are paired with removed lines and
        // count as Modified. Extra adds are pure Added. Extra removes
        // are anchored to a Deleted marker on the closest new-file line.
        let modified_count = adds_in_hunk.len().min(removes_in_hunk);
        for &lineno in &adds_in_hunk[..modified_count] {
            // libgit2 line numbers are 1-based.
            status.insert((lineno.saturating_sub(1)) as usize, GitLineStatus::Modified);
        }
        for &lineno in &adds_in_hunk[modified_count..] {
            status.insert((lineno.saturating_sub(1)) as usize, GitLineStatus::Added);
        }
        if removes_in_hunk > adds_in_hunk.len() {
            // Pure (or trailing) deletion — anchor the marker at the
            // last add of the hunk if there is one, otherwise at the
            // line immediately before the hunk's new_start. Clamp to
            // 0 for safety.
            let anchor = adds_in_hunk
                .last()
                .copied()
                .unwrap_or_else(|| hunk_new_start.max(1));
            let idx = (anchor.saturating_sub(1)) as usize;
            // Only fill in Deleted when nothing else claimed that line.
            status.entry(idx).or_insert(GitLineStatus::Deleted);
        }
    }

    status
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    fn make_repo() -> PathBuf {
        // (PID, atomic counter) avoids collisions when CI runs tests in
        // parallel — `subsec_nanos` is not unique enough on fast boxes.
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let pid = std::process::id();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("editor-app-git-{pid}-{n}"));
        std::fs::create_dir_all(&path).unwrap();
        // libgit2 needs a real repo with a HEAD; init + commit one file.
        let run = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(&path)
                .output()
                .expect("git available");
            assert!(
                out.status.success(),
                "git {args:?} failed: stdout={}, stderr={}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
            );
        };
        // `-c` overrides take precedence over the user's global config,
        // so identity + signing are explicit per-invocation.
        run(&["-c", "init.defaultBranch=main", "init", "-q"]);
        std::fs::write(path.join("hello.txt"), "alpha\nbeta\ngamma\n").unwrap();
        run(&["add", "."]);
        run(&[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "init",
        ]);
        path
    }

    #[test]
    fn unchanged_file_has_no_markers() {
        let root = make_repo();
        let status = compute_line_status(&root.join("hello.txt"), "alpha\nbeta\ngamma\n");
        assert!(
            status.is_empty(),
            "unchanged file should have no markers; got {status:?}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn appended_line_is_added() {
        let root = make_repo();
        let new = "alpha\nbeta\ngamma\ndelta\n";
        let status = compute_line_status(&root.join("hello.txt"), new);
        assert_eq!(
            status.get(&3),
            Some(&GitLineStatus::Added),
            "got {status:?}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn changed_middle_line_is_modified() {
        let root = make_repo();
        let new = "alpha\nBETA\ngamma\n";
        let status = compute_line_status(&root.join("hello.txt"), new);
        assert_eq!(
            status.get(&1),
            Some(&GitLineStatus::Modified),
            "got {status:?}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn deleted_line_anchors_a_marker() {
        let root = make_repo();
        // Drop "beta".
        let new = "alpha\ngamma\n";
        let status = compute_line_status(&root.join("hello.txt"), new);
        // Some line in {0, 1} is now Deleted-tagged.
        let has_delete = status.values().any(|s| *s == GitLineStatus::Deleted);
        assert!(has_delete, "expected a Deleted marker; got {status:?}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn untracked_file_marks_every_line_as_added() {
        let root = make_repo();
        let path = root.join("new.txt");
        std::fs::write(&path, "one\ntwo\n").unwrap();
        let status = compute_line_status(&path, "one\ntwo\n");
        assert_eq!(status.get(&0), Some(&GitLineStatus::Added));
        assert_eq!(status.get(&1), Some(&GitLineStatus::Added));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn file_outside_a_repo_returns_empty() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let pid = std::process::id();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!("editor-app-git-no-repo-{pid}-{n}"));
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("x.txt");
        std::fs::write(&path, "anything\n").unwrap();
        let status = compute_line_status(&path, "anything\n");
        assert!(status.is_empty());
        std::fs::remove_dir_all(&tmp).ok();
    }
}
