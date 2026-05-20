//! Discover VSCode colour themes already installed on the machine.
//!
//! VSCode (and its forks) install each extension under
//! `~/.vscode/extensions/<publisher>.<name>-<version>/`. A theme
//! extension declares its themes in its `package.json` under
//! `contributes.themes`, each entry carrying a human `label`, a
//! `uiTheme` (`vs` / `vs-dark` / `hc-*`), and a `path` to the theme
//! JSON relative to the extension directory.
//!
//! We scan those manifests and surface the themes in the command
//! palette, so the user can switch to any theme they already have in
//! VSCode without hunting for the JSON file by hand. The JSON itself is
//! loaded through [`editor_config::load_vscode_theme`] at apply time —
//! this module only *finds* the files.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// One theme found on disk, ready to be offered in the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredTheme {
    /// The `label` from the manifest — what the user sees.
    pub label: String,
    /// Absolute path to the theme JSON.
    pub path: PathBuf,
    /// `true` for `vs-dark` / `hc-black` themes, `false` for light.
    /// Only used to order dark themes first (most users want dark).
    pub dark: bool,
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(default)]
    contributes: Contributes,
}

#[derive(Deserialize, Default)]
struct Contributes {
    #[serde(default)]
    themes: Vec<ThemeContribution>,
}

#[derive(Deserialize)]
struct ThemeContribution {
    /// Some extensions omit `label` and rely on the file name; we skip
    /// those rather than guess.
    label: Option<String>,
    path: String,
    #[serde(default, rename = "uiTheme")]
    ui_theme: Option<String>,
}

/// Candidate extension roots: stock VSCode plus the common forks that
/// reuse the same `~/.<app>/extensions/` layout. Missing dirs are
/// silently skipped.
fn extension_roots() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    [
        ".vscode/extensions",
        ".vscode-insiders/extensions",
        ".vscode-oss/extensions",
        ".cursor/extensions",
        ".windsurf/extensions",
    ]
    .iter()
    .map(|rel| home.join(rel))
    .filter(|p| p.is_dir())
    .collect()
}

/// Scan every extension root for theme contributions. Results are
/// de-duplicated by `(label, dark)` — the same theme is often present
/// in several forks — and sorted dark-first, then by label. Returns an
/// empty vec when VSCode isn't installed.
pub fn discover_themes() -> Vec<DiscoveredTheme> {
    let mut out = Vec::new();
    for root in extension_roots() {
        collect_from_root(&root, &mut out);
    }
    out.sort_by(|a, b| b.dark.cmp(&a.dark).then_with(|| a.label.cmp(&b.label)));
    out.dedup_by(|a, b| a.label == b.label && a.dark == b.dark);
    out
}

/// Read every `<root>/<ext>/package.json` and append its theme
/// contributions to `out`.
fn collect_from_root(root: &Path, out: &mut Vec<DiscoveredTheme>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let ext_dir = entry.path();
        if !ext_dir.is_dir() {
            continue;
        }
        let manifest_path = ext_dir.join("package.json");
        let Ok(text) = std::fs::read_to_string(&manifest_path) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_str::<Manifest>(&text) else {
            continue;
        };
        for theme in manifest.contributes.themes {
            let Some(label) = theme.label else { continue };
            // The manifest path is relative to the extension dir and may
            // use `./` — `join` handles both that and absolute paths.
            let theme_path = ext_dir.join(theme.path.trim_start_matches("./"));
            if !theme_path.is_file() {
                continue;
            }
            let dark = theme
                .ui_theme
                .as_deref()
                .map(|t| t.contains("dark") || t.contains("black"))
                .unwrap_or(true);
            out.push(DiscoveredTheme {
                label,
                path: theme_path,
                dark,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Build a fake extension dir with a manifest + theme file under
    /// `root`, mirroring VSCode's on-disk layout.
    fn make_ext(root: &Path, name: &str, manifest: &str, theme_rel: &str) {
        let ext = root.join(name);
        fs::create_dir_all(ext.join("themes")).unwrap();
        fs::write(ext.join("package.json"), manifest).unwrap();
        fs::write(ext.join(theme_rel), "{}").unwrap();
    }

    #[test]
    fn collects_labelled_themes_with_existing_files() {
        let tmp = std::env::temp_dir().join(format!("vscdisc-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        make_ext(
            &tmp,
            "pub.dracula-1.0.0",
            r#"{"contributes":{"themes":[
                {"label":"Dracula","uiTheme":"vs-dark","path":"./themes/dracula.json"}
            ]}}"#,
            "themes/dracula.json",
        );

        let mut out = Vec::new();
        collect_from_root(&tmp, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].label, "Dracula");
        assert!(out[0].dark);
        assert!(out[0].path.is_file());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn skips_missing_theme_files_and_unlabelled_entries() {
        let tmp = std::env::temp_dir().join(format!("vscdisc2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        // path points at a file that doesn't exist → skipped.
        make_ext(
            &tmp,
            "pub.broken-1.0.0",
            r#"{"contributes":{"themes":[
                {"label":"Ghost","uiTheme":"vs-dark","path":"./themes/missing.json"}
            ]}}"#,
            "themes/present.json",
        );
        // entry without a label → skipped.
        make_ext(
            &tmp,
            "pub.nolabel-1.0.0",
            r#"{"contributes":{"themes":[
                {"uiTheme":"vs","path":"./themes/light.json"}
            ]}}"#,
            "themes/light.json",
        );

        let mut out = Vec::new();
        collect_from_root(&tmp, &mut out);
        assert!(out.is_empty(), "got {out:?}");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn light_theme_detected_from_ui_theme() {
        let tmp = std::env::temp_dir().join(format!("vscdisc3-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        make_ext(
            &tmp,
            "pub.solar-1.0.0",
            r#"{"contributes":{"themes":[
                {"label":"Solarized Light","uiTheme":"vs","path":"./themes/sl.json"}
            ]}}"#,
            "themes/sl.json",
        );

        let mut out = Vec::new();
        collect_from_root(&tmp, &mut out);
        assert_eq!(out.len(), 1);
        assert!(!out[0].dark);

        let _ = fs::remove_dir_all(&tmp);
    }
}
