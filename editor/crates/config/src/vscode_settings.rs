//! Import a VSCode `settings.json` into our [`PartialSettings`].
//!
//! VSCode stores user settings as a flat JSON-with-comments object
//! keyed by dotted paths (`"editor.fontSize": 14`). We read the handful
//! of keys that map onto our (deliberately tiny) settings schema and
//! return them as a [`PartialSettings`] the caller overlays onto the
//! current settings. Keys we don't model are ignored.
//!
//! Mapped keys:
//!
//! - `editor.fontSize` → `editor.font_size`
//! - `editor.tabSize` → `editor.tab_size`
//! - `editor.lineHeight` → `editor.line_height`, honouring VSCode's
//!   tri-modal semantics: `0` is "auto" (skipped, we keep our value);
//!   a value `< 8` is a multiplier of the font size; `>= 8` is a pixel
//!   height.
//! - `files.exclude` → `file_tree.hidden_dirs`, best-effort: glob keys
//!   of the form `**/<name>` (or `<name>`) whose value is `true` are
//!   reduced to their base name. Patterns with slashes or wildcards in
//!   the middle are too expressive for our basename-only model and are
//!   skipped.

use serde_json::Value;

use crate::vscode_theme::strip_jsonc;
use crate::PartialSettings;

/// Parse a VSCode `settings.json` document and project the keys we
/// understand onto a [`PartialSettings`]. Comments and trailing commas
/// are tolerated. A document that doesn't parse, or carries none of the
/// keys we map, yields `PartialSettings::default()` (an all-`None`
/// overlay that changes nothing when merged).
pub fn import_vscode_settings(json_text: &str) -> PartialSettings {
    let stripped = strip_jsonc(json_text);
    let Ok(value) = serde_json::from_str::<Value>(&stripped) else {
        return PartialSettings::default();
    };
    let Some(obj) = value.as_object() else {
        return PartialSettings::default();
    };

    let mut out = PartialSettings::default();

    let font_size = obj.get("editor.fontSize").and_then(Value::as_f64);
    if let Some(fs) = font_size {
        out.editor.font_size = Some(fs as f32);
    }
    if let Some(tab) = obj.get("editor.tabSize").and_then(Value::as_u64) {
        out.editor.tab_size = Some(tab as usize);
    }
    if let Some(lh) = obj.get("editor.lineHeight").and_then(Value::as_f64) {
        // VSCode: 0 = auto (leave ours); <8 = multiplier; >=8 = pixels.
        let resolved = if lh == 0.0 {
            None
        } else if lh < 8.0 {
            font_size.map(|fs| (fs * lh) as f32)
        } else {
            Some(lh as f32)
        };
        out.editor.line_height = resolved;
    }

    if let Some(excl) = obj.get("files.exclude").and_then(Value::as_object) {
        let dirs: Vec<String> = excl
            .iter()
            .filter(|(_, v)| v.as_bool() == Some(true))
            .filter_map(|(glob, _)| glob_to_basename(glob))
            .collect();
        if !dirs.is_empty() {
            out.file_tree.hidden_dirs = Some(dirs);
        }
    }

    out
}

/// Reduce a `files.exclude` glob to a bare directory name, or `None`
/// when the pattern is more expressive than our basename-only filter
/// can represent. `**/.git` and `node_modules` both yield their last
/// segment; `src/**/*.tmp` yields `None`.
fn glob_to_basename(glob: &str) -> Option<String> {
    // Drop a leading `**/`, the only wildcard prefix we can honour.
    let rest = glob.strip_prefix("**/").unwrap_or(glob);
    // Anything still carrying a path separator or wildcard isn't a
    // plain directory name.
    if rest.contains('/') || rest.contains('*') || rest.contains('?') || rest.is_empty() {
        return None;
    }
    Some(rest.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_font_and_tab_size() {
        let p = import_vscode_settings(r#"{ "editor.fontSize": 13, "editor.tabSize": 2 }"#);
        assert_eq!(p.editor.font_size, Some(13.0));
        assert_eq!(p.editor.tab_size, Some(2));
        assert_eq!(p.editor.line_height, None);
    }

    #[test]
    fn line_height_pixel_value_passes_through() {
        let p = import_vscode_settings(r#"{ "editor.lineHeight": 24 }"#);
        assert_eq!(p.editor.line_height, Some(24.0));
    }

    #[test]
    fn line_height_multiplier_uses_font_size() {
        let p = import_vscode_settings(r#"{ "editor.fontSize": 10, "editor.lineHeight": 1.5 }"#);
        assert_eq!(p.editor.line_height, Some(15.0));
    }

    #[test]
    fn line_height_zero_is_auto_and_skipped() {
        let p = import_vscode_settings(r#"{ "editor.lineHeight": 0 }"#);
        assert_eq!(p.editor.line_height, None);
    }

    #[test]
    fn files_exclude_reduces_to_basenames() {
        let p = import_vscode_settings(
            r#"{ "files.exclude": {
                "**/.git": true,
                "**/node_modules": true,
                "**/.DS_Store": false,
                "src/**/*.tmp": true
            } }"#,
        );
        let mut dirs = p.file_tree.hidden_dirs.unwrap();
        dirs.sort();
        assert_eq!(dirs, vec![".git".to_string(), "node_modules".to_string()]);
    }

    #[test]
    fn tolerates_comments_and_trailing_commas() {
        let p = import_vscode_settings(
            r#"{
                // editor
                "editor.fontSize": 15, // size
                "editor.tabSize": 4,
            }"#,
        );
        assert_eq!(p.editor.font_size, Some(15.0));
        assert_eq!(p.editor.tab_size, Some(4));
    }

    #[test]
    fn unknown_or_malformed_yields_empty_overlay() {
        assert_eq!(
            import_vscode_settings("not json"),
            PartialSettings::default()
        );
        assert_eq!(
            import_vscode_settings(r#"{ "workbench.colorTheme": "Dracula" }"#),
            PartialSettings::default()
        );
    }
}
