//! Node-style package-script detection (spec §4.4).
//!
//! Parses `<workspace_root>/package.json` for the `"scripts"` map and
//! detects which package manager the workspace uses from its lockfile.
//! Results are consumed by the command palette: each script becomes a
//! "Run script: <name>" entry that, when chosen, sends
//! `<pm> run <name>\n` into the embedded terminal pane.
//!
//! The detection is intentionally cheap — a single `read_to_string` for
//! the manifest plus a few `path.exists()` checks for lockfiles. Anything
//! more elaborate (monorepo walking, melos, turborepo) is a follow-up.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Which CLI to spawn when running a script. Detected from the lockfile
/// in the workspace root; falls back to `npm` when no lockfile exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageManager {
    Npm,
    Pnpm,
    Yarn,
    Bun,
}

impl PackageManager {
    /// The CLI command name (`npm`, `pnpm`, `yarn`, `bun`) that
    /// `<cmd> run <script>` invokes.
    pub fn binary(self) -> &'static str {
        match self {
            PackageManager::Npm => "npm",
            PackageManager::Pnpm => "pnpm",
            PackageManager::Yarn => "yarn",
            PackageManager::Bun => "bun",
        }
    }
}

/// One entry under the `"scripts"` key of `package.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpmScript {
    pub name: String,
    /// The raw command string from `package.json`. Stored so the palette
    /// can show it as a hint and the runner doesn't have to re-read the
    /// manifest.
    pub command: String,
    /// The workspace root the script lives in. In a multi-root workspace
    /// several roots can each have a `package.json`; the runner `cd`s
    /// here so the command executes against the right manifest.
    pub dir: PathBuf,
}

/// Look at the workspace root and pick a package manager from the
/// lockfile present (if any). Order tracks how exclusive each lockfile
/// is — bun's lockfile is binary-distinct, pnpm's is unique to its
/// CLI, yarn's similarly. `package-lock.json` is npm's. No lockfile
/// → assume npm (the broadest fallback).
pub fn detect_package_manager(root: &Path) -> PackageManager {
    if root.join("bun.lockb").exists() {
        PackageManager::Bun
    } else if root.join("pnpm-lock.yaml").exists() {
        PackageManager::Pnpm
    } else if root.join("yarn.lock").exists() {
        PackageManager::Yarn
    } else {
        // `package-lock.json` present *or* no lockfile at all → npm.
        PackageManager::Npm
    }
}

/// Read `<root>/package.json` and return its `"scripts"` map as a
/// sorted list. Empty when the manifest is missing, malformed, or has
/// no `scripts` key — callers can treat the empty case as "no
/// runnable scripts in this workspace".
pub fn read_scripts(root: &Path) -> Vec<NpmScript> {
    let path = root.join("package.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    #[derive(Deserialize)]
    struct PackageJson {
        #[serde(default)]
        scripts: std::collections::BTreeMap<String, String>,
    }
    let Ok(pkg) = serde_json::from_str::<PackageJson>(&text) else {
        return Vec::new();
    };
    // BTreeMap iterates in key order — alphabetical by script name,
    // matching how `npm run` lists them. Keeps the palette order
    // stable across reloads.
    pkg.scripts
        .into_iter()
        .map(|(name, command)| NpmScript {
            name,
            command,
            dir: root.to_path_buf(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let pid = std::process::id();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("editor-app-scripts-{pid}-{n}"));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn detect_falls_back_to_npm_when_no_lockfile() {
        let root = tempdir();
        assert_eq!(detect_package_manager(&root), PackageManager::Npm);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn detect_pnpm_from_lockfile() {
        let root = tempdir();
        std::fs::write(root.join("pnpm-lock.yaml"), "").unwrap();
        assert_eq!(detect_package_manager(&root), PackageManager::Pnpm);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn detect_yarn_from_lockfile() {
        let root = tempdir();
        std::fs::write(root.join("yarn.lock"), "").unwrap();
        assert_eq!(detect_package_manager(&root), PackageManager::Yarn);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn detect_bun_from_lockfile() {
        let root = tempdir();
        std::fs::write(root.join("bun.lockb"), "").unwrap();
        assert_eq!(detect_package_manager(&root), PackageManager::Bun);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn detect_bun_wins_when_multiple_lockfiles_exist() {
        // Monorepo migrations can leave stale lockfiles around. The
        // newer / more-exclusive bun lockfile wins so the user's
        // active CLI matches what's actually being used.
        let root = tempdir();
        std::fs::write(root.join("bun.lockb"), "").unwrap();
        std::fs::write(root.join("package-lock.json"), "{}").unwrap();
        assert_eq!(detect_package_manager(&root), PackageManager::Bun);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn read_scripts_returns_empty_when_no_package_json() {
        let root = tempdir();
        assert!(read_scripts(&root).is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn read_scripts_parses_and_sorts() {
        let root = tempdir();
        std::fs::write(
            root.join("package.json"),
            r#"{
                "name": "demo",
                "scripts": {
                    "dev": "vite",
                    "build": "vite build",
                    "test": "vitest"
                }
            }"#,
        )
        .unwrap();
        let scripts = read_scripts(&root);
        let names: Vec<&str> = scripts.iter().map(|s| s.name.as_str()).collect();
        // BTreeMap → alphabetical order.
        assert_eq!(names, vec!["build", "dev", "test"]);
        assert_eq!(scripts[1].command, "vite");
        // Every script remembers its owning root (multi-root cwd).
        assert!(scripts.iter().all(|s| s.dir == root));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn read_scripts_returns_empty_on_malformed_json() {
        let root = tempdir();
        std::fs::write(root.join("package.json"), "{ not valid json").unwrap();
        assert!(read_scripts(&root).is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn read_scripts_returns_empty_when_scripts_key_missing() {
        let root = tempdir();
        std::fs::write(root.join("package.json"), r#"{"name": "x"}"#).unwrap();
        assert!(read_scripts(&root).is_empty());
        std::fs::remove_dir_all(&root).ok();
    }
}
