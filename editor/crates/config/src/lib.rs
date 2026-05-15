//! Editor settings, loaded from a TOML file (ADR-009, spec §4.1.4).
//!
//! The schema is intentionally tiny in M1 — just the few knobs the app
//! actually reads. New fields slot in by adding a serde-defaulted field; old
//! files keep working.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The top-level settings document.
///
/// Missing fields fall back to the values in [`Default`] thanks to
/// `#[serde(default)]`, so a partial settings file works.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub editor: EditorSettings,
}

/// Editor-section settings — type, spacing, indentation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorSettings {
    /// Font size in *logical* points. Multiplied by the window's scale factor
    /// at the app boundary.
    pub font_size: f32,
    /// Line height in logical points. Should be larger than `font_size`.
    pub line_height: f32,
    /// Number of spaces a Tab key press inserts.
    pub tab_size: usize,
}

impl Default for EditorSettings {
    fn default() -> Self {
        // 16/22 matches what the app already shipped before settings landed,
        // so an unconfigured installation looks identical.
        Self {
            font_size: 16.0,
            line_height: 22.0,
            tab_size: 4,
        }
    }
}

impl Settings {
    /// Read a TOML settings file from `path`. Tolerant of every common
    /// failure mode — missing file, IO error, malformed TOML — by falling
    /// back to [`Default`] and logging.
    pub fn load_or_default(path: &Path) -> Settings {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<Settings>(&text) {
                Ok(s) => {
                    log::info!("loaded settings from {}", path.display());
                    s
                }
                Err(e) => {
                    log::warn!(
                        "settings file at {} is malformed ({e}); using defaults",
                        path.display()
                    );
                    Settings::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                log::debug!("no settings file at {}; using defaults", path.display());
                Settings::default()
            }
            Err(e) => {
                log::warn!(
                    "couldn't read settings from {} ({e}); using defaults",
                    path.display()
                );
                Settings::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_legacy_hardcoded_values() {
        let s = Settings::default();
        assert_eq!(s.editor.font_size, 16.0);
        assert_eq!(s.editor.line_height, 22.0);
        assert_eq!(s.editor.tab_size, 4);
    }

    #[test]
    fn round_trip_full_document() {
        let original = Settings {
            editor: EditorSettings {
                font_size: 18.0,
                line_height: 26.0,
                tab_size: 2,
            },
        };
        let text = toml::to_string(&original).unwrap();
        let parsed: Settings = toml::from_str(&text).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn partial_document_keeps_defaults() {
        // Only font_size is set — line_height and tab_size should still be
        // the defaults.
        let text = "[editor]\nfont_size = 20.0\n";
        let parsed: Settings = toml::from_str(text).unwrap();
        assert_eq!(parsed.editor.font_size, 20.0);
        assert_eq!(parsed.editor.line_height, 22.0);
        assert_eq!(parsed.editor.tab_size, 4);
    }

    #[test]
    fn empty_document_is_all_defaults() {
        let parsed: Settings = toml::from_str("").unwrap();
        assert_eq!(parsed, Settings::default());
    }

    #[test]
    fn load_or_default_missing_path_is_default() {
        let path = std::env::temp_dir().join("lighteditor-test-does-not-exist.toml");
        // Make sure the file really doesn't exist.
        let _ = std::fs::remove_file(&path);
        let s = Settings::load_or_default(&path);
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn load_or_default_malformed_file_is_default() {
        let path = std::env::temp_dir().join("lighteditor-test-malformed.toml");
        std::fs::write(&path, "this is not valid TOML = = =").unwrap();
        let s = Settings::load_or_default(&path);
        assert_eq!(s, Settings::default());
        let _ = std::fs::remove_file(&path);
    }
}
