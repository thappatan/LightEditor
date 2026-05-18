//! Theme — user-overridable colors loaded from a TOML file.
//!
//! Every visual surface in the editor (status bar background, indent guide
//! tint, syntax keyword color, etc.) goes through this struct. Missing
//! fields fall back to the dark default, so a sparse `theme.toml` only
//! overrides the colors the user cares about.
//!
//! Colors are stored as hex strings (`"#RRGGBB"` or `"#RRGGBBAA"`) so the
//! TOML stays human-readable. Use [`parse_hex_color`] at the consumer site
//! to convert to `[u8; 4]`; invalid input returns `None` so the caller can
//! fall back to a sane default without panicking.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The top-level theme document.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    pub editor: EditorTheme,
    pub syntax: SyntaxTheme,
    pub terminal: TerminalTheme,
}

/// Colors for editor chrome and non-syntax surfaces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorTheme {
    /// Window clear color — the background under everything.
    pub background: String,
    /// Default editor text color.
    pub text_fg: String,
    /// Caret color.
    pub caret: String,
    /// Selection highlight (with alpha).
    pub selection_bg: String,
    /// Subtle full-row tint behind the line containing the caret.
    pub active_line_bg: String,

    /// Gutter (line numbers) backdrop.
    pub gutter_bg: String,
    /// Inactive line numbers.
    pub gutter_fg: String,
    /// Number for the line that has the caret.
    pub gutter_active_fg: String,

    /// Tab strip background.
    pub tab_bar_bg: String,
    /// Active tab background.
    pub tab_active_bg: String,
    /// Inactive tab background.
    pub tab_inactive_bg: String,
    /// Vertical separator between tabs.
    pub tab_separator: String,
    /// Active label text + (hovered) close "×".
    pub tab_label_fg: String,
    /// Close "×" idle color (also reused for muted UI text).
    pub close_button: String,

    /// Status bar background.
    pub status_bg: String,
    /// Status bar text color.
    pub status_fg: String,

    /// Find / palette panel background.
    pub overlay_bg: String,
    /// Dim layer behind the palette so the editor reads as "behind".
    pub overlay_scrim: String,
    /// Highlight on the palette's selected row.
    pub palette_selection_bg: String,

    /// Other-match (non-current) highlight for Find.
    pub find_match_bg: String,
    /// Faint vertical line per indent column.
    pub indent_guide: String,
    /// Square highlight on the bracket the caret is next to + its match.
    pub bracket_match: String,
}

impl Default for EditorTheme {
    fn default() -> Self {
        // Hand-picked dark palette — what the editor shipped before themes
        // landed. Every value carries its alpha as the trailing two hex
        // digits, so opaque (`ff`) and translucent (`<ff`) read the same.
        Self {
            background: "#050508ff".into(),
            text_fg: "#eeeeeeff".into(),
            caret: "#78a0ffff".into(),
            selection_bg: "#78a0ff40".into(),
            active_line_bg: "#ffffff0c".into(),

            gutter_bg: "#0e0e14ff".into(),
            gutter_fg: "#b4b4beff".into(),
            gutter_active_fg: "#eeeeeeff".into(),

            tab_bar_bg: "#16161cff".into(),
            tab_active_bg: "#30303cff".into(),
            tab_inactive_bg: "#1e1e26ff".into(),
            tab_separator: "#3c3c46ff".into(),
            tab_label_fg: "#dcdcdcff".into(),
            close_button: "#b4b4beff".into(),

            status_bg: "#16161cff".into(),
            status_fg: "#b4b4beff".into(),

            // Popups must be fully opaque so the editor's text doesn't
            // bleed through the palette / completion / hover panel.
            overlay_bg: "#262630ff".into(),
            overlay_scrim: "#00000060".into(),
            palette_selection_bg: "#78a0ff60".into(),

            find_match_bg: "#ffc83c40".into(),
            indent_guide: "#505064a0".into(),
            bracket_match: "#b4c8ff24".into(),
        }
    }
}

/// Colors for tree-sitter highlight categories.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyntaxTheme {
    pub keyword: String,
    pub string: String,
    pub number: String,
    pub comment: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub function: String,
    pub punctuation: String,
}

impl Default for SyntaxTheme {
    fn default() -> Self {
        Self {
            keyword: "#cd82e9ff".into(),
            string: "#a0e6a8ff".into(),
            number: "#ffb87cff".into(),
            comment: "#7a7a88ff".into(),
            type_: "#f0d983ff".into(),
            function: "#8ab4f8ff".into(),
            punctuation: "#a0a0b0ff".into(),
        }
    }
}

/// Colours for the embedded terminal pane. The 16-entry `palette` maps
/// to the standard ANSI colour names in their numeric order:
/// `[Black, Red, Green, Yellow, Blue, Magenta, Cyan, White,
///   BrightBlack, BrightRed, …, BrightWhite]`. `foreground` / `background`
/// / `cursor` are returned for the corresponding `NamedColor` sentinels
/// programs emit when they want the pane defaults.
///
/// Missing fields fall back to the Tango-ish defaults that shipped
/// hardcoded before this section landed — an existing `theme.toml`
/// without a `[terminal]` block keeps its old look.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalTheme {
    pub foreground: String,
    pub background: String,
    pub cursor: String,
    pub palette: Vec<String>,
}

impl Default for TerminalTheme {
    fn default() -> Self {
        Self {
            // Empty strings → callers fall back to the editor's own
            // text_fg / background / caret. Keeps the pane visually
            // continuous with the chrome when no override is supplied.
            foreground: String::new(),
            background: String::new(),
            cursor: String::new(),
            palette: vec![
                "#000000ff".into(), // 0  Black
                "#cc0000ff".into(), // 1  Red
                "#4e9a06ff".into(), // 2  Green
                "#c4a000ff".into(), // 3  Yellow
                "#3465a4ff".into(), // 4  Blue
                "#75507bff".into(), // 5  Magenta
                "#06989aff".into(), // 6  Cyan
                "#d3d7cfff".into(), // 7  White
                "#555753ff".into(), // 8  BrightBlack
                "#ef2929ff".into(), // 9  BrightRed
                "#8ae234ff".into(), // 10 BrightGreen
                "#fce94fff".into(), // 11 BrightYellow
                "#729fcfff".into(), // 12 BrightBlue
                "#ad7fa8ff".into(), // 13 BrightMagenta
                "#34e2e2ff".into(), // 14 BrightCyan
                "#eeeeecff".into(), // 15 BrightWhite
            ],
        }
    }
}

impl Theme {
    /// Read a TOML theme file from `path`. Tolerant of every common failure
    /// mode — missing file, IO error, malformed TOML — by falling back to
    /// [`Default`] and logging.
    pub fn load_or_default(path: &Path) -> Theme {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<Theme>(&text) {
                Ok(t) => {
                    log::info!("loaded theme from {}", path.display());
                    t
                }
                Err(e) => {
                    log::warn!(
                        "theme at {} is malformed ({e}); using defaults",
                        path.display()
                    );
                    Theme::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                log::debug!("no theme file at {}; using defaults", path.display());
                Theme::default()
            }
            Err(e) => {
                log::warn!(
                    "couldn't read theme from {} ({e}); using defaults",
                    path.display()
                );
                Theme::default()
            }
        }
    }
}

/// Parse a hex color string into `[r, g, b, a]`. Supports `"#RRGGBB"`,
/// `"#RRGGBBAA"`, `"#RGB"`, and `"#RGBA"`. Returns `None` for any other
/// shape — the caller should fall back to a sane default.
pub fn parse_hex_color(s: &str) -> Option<[u8; 4]> {
    let s = s.trim().trim_start_matches('#');
    match s.len() {
        6 => Some([hex(&s[0..2])?, hex(&s[2..4])?, hex(&s[4..6])?, 0xff]),
        8 => Some([
            hex(&s[0..2])?,
            hex(&s[2..4])?,
            hex(&s[4..6])?,
            hex(&s[6..8])?,
        ]),
        3 => Some([
            short_hex(&s[0..1])?,
            short_hex(&s[1..2])?,
            short_hex(&s[2..3])?,
            0xff,
        ]),
        4 => Some([
            short_hex(&s[0..1])?,
            short_hex(&s[1..2])?,
            short_hex(&s[2..3])?,
            short_hex(&s[3..4])?,
        ]),
        _ => None,
    }
}

fn hex(s: &str) -> Option<u8> {
    u8::from_str_radix(s, 16).ok()
}

/// Expand a single hex digit to a byte by repetition (`f` → `0xff`).
fn short_hex(s: &str) -> Option<u8> {
    let d = u8::from_str_radix(s, 16).ok()?;
    Some(d * 16 + d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rrggbb_assumes_full_alpha() {
        assert_eq!(parse_hex_color("#cd82e9"), Some([0xcd, 0x82, 0xe9, 0xff]));
        assert_eq!(parse_hex_color("cd82e9"), Some([0xcd, 0x82, 0xe9, 0xff]));
    }

    #[test]
    fn parse_rrggbbaa_keeps_alpha() {
        assert_eq!(parse_hex_color("#cd82e980"), Some([0xcd, 0x82, 0xe9, 0x80]));
    }

    #[test]
    fn parse_short_form_repeats_digits() {
        assert_eq!(parse_hex_color("#abc"), Some([0xaa, 0xbb, 0xcc, 0xff]));
        assert_eq!(parse_hex_color("#abcd"), Some([0xaa, 0xbb, 0xcc, 0xdd]));
    }

    #[test]
    fn parse_rejects_garbage() {
        assert_eq!(parse_hex_color("not-a-color"), None);
        assert_eq!(parse_hex_color("#xyzxyz"), None);
        assert_eq!(parse_hex_color(""), None);
        assert_eq!(parse_hex_color("#1234567"), None);
    }

    #[test]
    fn default_theme_round_trips() {
        let t = Theme::default();
        let text = toml::to_string(&t).unwrap();
        let parsed: Theme = toml::from_str(&text).unwrap();
        assert_eq!(parsed, t);
    }

    #[test]
    fn partial_theme_keeps_defaults() {
        let text = r##"
[editor]
caret = "#ff0000"

[syntax]
keyword = "#00ff00"
"##;
        let parsed: Theme = toml::from_str(text).unwrap();
        assert_eq!(parsed.editor.caret, "#ff0000");
        assert_eq!(parsed.syntax.keyword, "#00ff00");
        // Untouched fields still match the default.
        let default = Theme::default();
        assert_eq!(parsed.editor.background, default.editor.background);
        assert_eq!(parsed.syntax.string, default.syntax.string);
    }

    #[test]
    fn type_field_renames_in_toml() {
        // `type` is a Rust keyword; the field is named `type_` but the
        // TOML key is `type` thanks to `#[serde(rename = "type")]`.
        let text = r##"
[syntax]
type = "#abcdef"
"##;
        let parsed: Theme = toml::from_str(text).unwrap();
        assert_eq!(parsed.syntax.type_, "#abcdef");
    }

    #[test]
    fn load_or_default_missing_path_is_default() {
        let path = std::env::temp_dir().join("lighteditor-test-theme-missing.toml");
        let _ = std::fs::remove_file(&path);
        let t = Theme::load_or_default(&path);
        assert_eq!(t, Theme::default());
    }

    #[test]
    fn load_or_default_malformed_is_default() {
        let path = std::env::temp_dir().join("lighteditor-test-theme-malformed.toml");
        std::fs::write(&path, "this is not = = = valid").unwrap();
        let t = Theme::load_or_default(&path);
        assert_eq!(t, Theme::default());
        let _ = std::fs::remove_file(&path);
    }
}
