//! VSCode-format JSON theme loader.
//!
//! Reads a VSCode theme file (the JSON shape used under
//! `extensions/theme-defaults/themes/` in microsoft/vscode) and
//! returns one of our internal [`Theme`] structs. Handles the
//! pieces that show up in real-world themes:
//!
//! - **Comments** (`// …` and `/* … */`) and **trailing commas** —
//!   VSCode theme JSON is JSON-with-comments. We strip them before
//!   handing the text to `serde_json`.
//! - **`include` chains** — themes commonly inherit from a base
//!   (`dark_modern` → `dark_plus` → `dark_vs`). Resolution is
//!   recursive against the parent file's directory; later entries
//!   in the chain override earlier ones for `colors` and
//!   `tokenColors`.
//! - **Default fallback tables** per `type: "dark" | "light"` —
//!   many of the colour keys our internal [`Theme`] expects (a
//!   selection background, a line-highlight background, the 16
//!   ANSI palette slots) aren't always present in the JSON.
//!   VSCode itself supplies hard-coded defaults; we mirror them.
//! - **TextMate scope → syntax bucket** mapping — `tokenColors`
//!   carry TextMate scopes like `keyword.control` or
//!   `entity.name.function`. We pick the longest-prefix match
//!   against a hand-curated scope-to-category table and project
//!   into our 7-bucket [`SyntaxTheme`].
//!
//! Out of v1:
//!
//! - **`semanticTokenColors`** — LSP semantic tokens. Our syntax
//!   pipeline is tree-sitter-only today; semantic-token highlighting
//!   is a follow-up.
//! - **Workbench-specific keys** (sidebar / activity bar / debug
//!   toolbar) that don't map to anything we draw. Silently dropped.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::theme::{parse_hex_color, EditorTheme, SyntaxTheme, TerminalTheme, Theme};

/// Why a VSCode-format theme couldn't be loaded. The caller usually
/// shows the message on the status bar and falls back to the editor's
/// previous theme.
#[derive(Debug)]
pub enum VscodeThemeError {
    Io(std::io::Error),
    Parse(String),
    /// `include` chain referenced a path that doesn't resolve under
    /// the parent file's directory. Carries the offending include
    /// string + the path it tried.
    BadInclude {
        include: String,
        tried: PathBuf,
    },
    /// Cycle detected in the `include` chain. Carries the path that
    /// was re-entered.
    IncludeCycle(PathBuf),
}

impl std::fmt::Display for VscodeThemeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VscodeThemeError::Io(e) => write!(f, "io: {e}"),
            VscodeThemeError::Parse(msg) => write!(f, "json parse: {msg}"),
            VscodeThemeError::BadInclude { include, tried } => write!(
                f,
                "include '{}' could not be resolved (tried {})",
                include,
                tried.display()
            ),
            VscodeThemeError::IncludeCycle(p) => {
                write!(f, "include cycle through {}", p.display())
            }
        }
    }
}

impl std::error::Error for VscodeThemeError {}

/// Load a VSCode JSON theme from `path` and convert it into our
/// internal [`Theme`]. Comments and trailing commas in the JSON are
/// tolerated; an `include` directive is followed recursively
/// (relative to the parent file's directory).
pub fn load_vscode_theme(path: &Path) -> Result<Theme, VscodeThemeError> {
    let mut visited = Vec::new();
    let merged = resolve_chain(path, &mut visited)?;
    Ok(merged_to_theme(merged))
}

/// Raw VSCode theme shape (one file, post comment-strip).
#[derive(Debug, Default, Deserialize)]
struct VscodeFile {
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "type")]
    ty: Option<String>,
    #[serde(default)]
    include: Option<String>,
    #[serde(default)]
    colors: HashMap<String, Option<String>>,
    #[serde(default)]
    #[serde(rename = "tokenColors")]
    token_colors: Vec<TokenColorEntry>,
}

#[derive(Debug, Default, Deserialize)]
struct TokenColorEntry {
    #[serde(default)]
    scope: Option<ScopeSel>,
    #[serde(default)]
    settings: TokenSettings,
}

#[derive(Debug, Default, Deserialize)]
struct TokenSettings {
    #[serde(default)]
    foreground: Option<String>,
    #[allow(dead_code)]
    #[serde(default, rename = "fontStyle")]
    font_style: Option<String>,
}

/// `scope` in a `tokenColors` entry is either one string or an array
/// of strings. Serde's untagged enum handles both shapes.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ScopeSel {
    One(String),
    Many(Vec<String>),
}

impl ScopeSel {
    fn iter(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        match self {
            ScopeSel::One(s) => Box::new(std::iter::once(s.as_str())),
            ScopeSel::Many(v) => Box::new(v.iter().map(|s| s.as_str())),
        }
    }
}

/// What a chain merges to: a flat `colors` map, a list of token
/// rules in resolution order, the final `type` (used to pick the
/// defaults table when a key is missing) and the deepest file's name
/// for logging.
#[derive(Debug, Default)]
struct MergedTheme {
    colors: HashMap<String, String>,
    token_colors: Vec<TokenColorEntry>,
    ty: ThemeType,
    #[allow(dead_code)]
    name: Option<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum ThemeType {
    #[default]
    Dark,
    Light,
}

impl ThemeType {
    fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "dark" => Some(ThemeType::Dark),
            "light" => Some(ThemeType::Light),
            _ => None,
        }
    }
}

/// Walk the `include` chain from `path` upward (base → leaf) and
/// flatten into one [`MergedTheme`]. Later files override earlier
/// `colors`; `tokenColors` are concatenated so later rules win on
/// scope conflicts via the longest-prefix resolver.
fn resolve_chain(path: &Path, visited: &mut Vec<PathBuf>) -> Result<MergedTheme, VscodeThemeError> {
    let canon = path.canonicalize().map_err(VscodeThemeError::Io)?;
    if visited.iter().any(|p| p == &canon) {
        return Err(VscodeThemeError::IncludeCycle(canon));
    }
    visited.push(canon.clone());

    let text = std::fs::read_to_string(&canon).map_err(VscodeThemeError::Io)?;
    let stripped = strip_jsonc(&text);
    let file: VscodeFile =
        serde_json::from_str(&stripped).map_err(|e| VscodeThemeError::Parse(e.to_string()))?;

    let mut base = if let Some(inc) = file.include.as_ref() {
        let parent = canon.parent().ok_or_else(|| VscodeThemeError::BadInclude {
            include: inc.clone(),
            tried: PathBuf::from(inc),
        })?;
        let inc_path = parent.join(inc);
        if !inc_path.exists() {
            return Err(VscodeThemeError::BadInclude {
                include: inc.clone(),
                tried: inc_path,
            });
        }
        resolve_chain(&inc_path, visited)?
    } else {
        MergedTheme::default()
    };

    // Apply this file on top of the base.
    if let Some(ty) = file.ty.as_deref().and_then(ThemeType::parse) {
        base.ty = ty;
    }
    if file.name.is_some() {
        base.name = file.name;
    }
    for (k, v) in file.colors.into_iter() {
        if let Some(hex) = v {
            base.colors.insert(k, hex);
        }
    }
    base.token_colors.extend(file.token_colors);

    Ok(base)
}

/// Strip `//` and `/* */` comments and trailing commas from `text`.
/// Skips over string literals so a `//` inside `"http://..."` is
/// preserved. Output is plain JSON that `serde_json` can parse.
pub fn strip_jsonc(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_string = false;
    let mut escape = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            out.push(c as char);
            if escape {
                escape = false;
            } else if c == b'\\' {
                escape = true;
            } else if c == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == b'"' {
            in_string = true;
            out.push('"');
            i += 1;
            continue;
        }
        if c == b'/' && i + 1 < bytes.len() {
            let next = bytes[i + 1];
            if next == b'/' {
                // Line comment — skip to newline.
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            if next == b'*' {
                // Block comment — skip to '*/'.
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
                continue;
            }
        }
        out.push(c as char);
        i += 1;
    }
    // Remove trailing commas before `]` or `}`. Walk the output once.
    let mut cleaned = String::with_capacity(out.len());
    let chars: Vec<char> = out.chars().collect();
    let mut j = 0;
    while j < chars.len() {
        let ch = chars[j];
        if ch == ',' {
            // Look ahead past whitespace for a closer.
            let mut k = j + 1;
            while k < chars.len() && chars[k].is_whitespace() {
                k += 1;
            }
            if k < chars.len() && (chars[k] == ']' || chars[k] == '}') {
                // Drop this comma.
                j += 1;
                continue;
            }
        }
        cleaned.push(ch);
        j += 1;
    }
    cleaned
}

/// Convert a merged VSCode theme into our internal [`Theme`].
fn merged_to_theme(merged: MergedTheme) -> Theme {
    let MergedTheme {
        colors,
        token_colors,
        ty,
        ..
    } = merged;

    // Per-type defaults supply any key the JSON didn't name. VSCode's
    // own defaults are the source of truth here; this table mirrors
    // the most-important slice (selection / line-highlight / ANSI 16
    // / terminal bg-and-cursor) so themes that omit them still look
    // right.
    let defaults = type_defaults(ty);
    let get = |k: &str| -> Option<&str> {
        colors
            .get(k)
            .map(|s| s.as_str())
            .or_else(|| defaults.get(k).copied())
    };
    let get_or = |k: &str, fallback: &str| -> String { get(k).unwrap_or(fallback).to_string() };

    // The editor's text fg / bg fall back via foreground / background
    // (vs the editor-specific keys) the way VSCode itself does.
    let bg = get_or("editor.background", defaults["editor.background"]);
    let fg = get("editor.foreground")
        .or_else(|| get("foreground"))
        .unwrap_or(defaults["editor.foreground"])
        .to_string();

    let editor = EditorTheme {
        background: bg.clone(),
        text_fg: fg.clone(),
        caret: get("editorCursor.foreground").unwrap_or(&fg).to_string(),
        selection_bg: get_or(
            "editor.selectionBackground",
            defaults["editor.selectionBackground"],
        ),
        active_line_bg: get_or(
            "editor.lineHighlightBackground",
            defaults["editor.lineHighlightBackground"],
        ),
        gutter_bg: get_or("editorGutter.background", &bg),
        gutter_fg: get_or(
            "editorLineNumber.foreground",
            defaults["editorLineNumber.foreground"],
        ),
        gutter_active_fg: get_or("editorLineNumber.activeForeground", &fg),
        tab_bar_bg: get_or(
            "editorGroupHeader.tabsBackground",
            defaults["editorGroupHeader.tabsBackground"],
        ),
        tab_active_bg: get_or("tab.activeBackground", &bg),
        tab_inactive_bg: get_or("tab.inactiveBackground", defaults["tab.inactiveBackground"]),
        tab_separator: get_or("tab.border", defaults["tab.border"]),
        tab_label_fg: get_or("tab.activeForeground", &fg),
        close_button: get_or("icon.foreground", defaults["icon.foreground"]),
        status_bg: get_or("statusBar.background", defaults["statusBar.background"]),
        status_fg: get_or("statusBar.foreground", &fg),
        overlay_bg: get_or(
            "editorWidget.background",
            defaults["editorWidget.background"],
        ),
        overlay_scrim: defaults["overlayScrim"].to_string(),
        palette_selection_bg: get_or(
            "list.activeSelectionBackground",
            defaults["list.activeSelectionBackground"],
        ),
        find_match_bg: get_or(
            "editor.findMatchBackground",
            defaults["editor.findMatchBackground"],
        ),
        indent_guide: get_or(
            "editorIndentGuide.background1",
            defaults["editorIndentGuide.background1"],
        ),
        bracket_match: get_or(
            "editorBracketMatch.background",
            defaults["editorBracketMatch.background"],
        ),
    };

    let terminal = TerminalTheme {
        foreground: get_or("terminal.foreground", &fg),
        background: get_or("terminal.background", defaults["terminal.background"]),
        cursor: get_or("terminalCursor.foreground", &editor.caret),
        palette: vec![
            get_or("terminal.ansiBlack", defaults["terminal.ansiBlack"]),
            get_or("terminal.ansiRed", defaults["terminal.ansiRed"]),
            get_or("terminal.ansiGreen", defaults["terminal.ansiGreen"]),
            get_or("terminal.ansiYellow", defaults["terminal.ansiYellow"]),
            get_or("terminal.ansiBlue", defaults["terminal.ansiBlue"]),
            get_or("terminal.ansiMagenta", defaults["terminal.ansiMagenta"]),
            get_or("terminal.ansiCyan", defaults["terminal.ansiCyan"]),
            get_or("terminal.ansiWhite", defaults["terminal.ansiWhite"]),
            get_or(
                "terminal.ansiBrightBlack",
                defaults["terminal.ansiBrightBlack"],
            ),
            get_or("terminal.ansiBrightRed", defaults["terminal.ansiBrightRed"]),
            get_or(
                "terminal.ansiBrightGreen",
                defaults["terminal.ansiBrightGreen"],
            ),
            get_or(
                "terminal.ansiBrightYellow",
                defaults["terminal.ansiBrightYellow"],
            ),
            get_or(
                "terminal.ansiBrightBlue",
                defaults["terminal.ansiBrightBlue"],
            ),
            get_or(
                "terminal.ansiBrightMagenta",
                defaults["terminal.ansiBrightMagenta"],
            ),
            get_or(
                "terminal.ansiBrightCyan",
                defaults["terminal.ansiBrightCyan"],
            ),
            get_or(
                "terminal.ansiBrightWhite",
                defaults["terminal.ansiBrightWhite"],
            ),
        ],
    };

    let syntax = build_syntax(&token_colors, &fg);

    Theme {
        editor,
        syntax,
        terminal,
    }
}

/// Project the tokenColors list onto our 7-bucket [`SyntaxTheme`].
/// For each bucket, find the most-specific (longest-prefix) scope rule
/// that matches one of the bucket's curated scopes and pull its
/// foreground colour. Falls back to the editor foreground when no rule
/// covers a bucket.
fn build_syntax(token_colors: &[TokenColorEntry], fallback_fg: &str) -> SyntaxTheme {
    let pick = |scopes: &[&str]| -> String {
        let mut best: Option<(usize, String)> = None;
        for entry in token_colors {
            let Some(sel) = entry.scope.as_ref() else {
                continue;
            };
            let Some(fg) = entry.settings.foreground.as_ref() else {
                continue;
            };
            for raw_scope in sel.iter() {
                // VSCode scopes can be descendant selectors —
                // `source.js entity.name.function`. The leaf (last
                // space-separated segment) is what matters for our
                // bucket lookup; the prefix qualifies the language
                // context which we don't differentiate.
                let leaf = raw_scope
                    .rsplit_once(' ')
                    .map(|(_, t)| t)
                    .unwrap_or(raw_scope);
                for wanted in scopes {
                    if leaf == *wanted || leaf.starts_with(&format!("{wanted}.")) {
                        let len = leaf.len();
                        if best.as_ref().map_or(true, |(l, _)| len > *l) {
                            best = Some((len, fg.clone()));
                        }
                    }
                }
            }
        }
        best.map(|(_, fg)| fg)
            .unwrap_or_else(|| fallback_fg.to_string())
    };

    SyntaxTheme {
        keyword: pick(&["keyword", "storage", "storage.type"]),
        string: pick(&["string"]),
        number: pick(&["constant.numeric", "constant.language"]),
        comment: pick(&["comment"]),
        type_: pick(&[
            "entity.name.type",
            "entity.name.class",
            "support.type",
            "support.class",
        ]),
        function: pick(&["entity.name.function", "support.function"]),
        variable: pick(&[
            "variable",
            "variable.other",
            "meta.definition.variable",
            "support.variable",
        ]),
        punctuation: pick(&["punctuation", "meta.brace", "meta.delimiter"]),
    }
}

/// Hard-coded fallback table mirroring VSCode's per-type defaults
/// for the keys we read but real-world themes commonly omit.
fn type_defaults(ty: ThemeType) -> HashMap<&'static str, &'static str> {
    let mut m = HashMap::new();
    match ty {
        ThemeType::Dark => {
            m.insert("editor.background", "#1e1e1eff");
            m.insert("editor.foreground", "#d4d4d4ff");
            m.insert("editor.selectionBackground", "#264f78ff");
            m.insert("editor.lineHighlightBackground", "#ffffff0c");
            m.insert("editor.findMatchBackground", "#9e6a03ff");
            m.insert("editorLineNumber.foreground", "#858585ff");
            m.insert("editorGroupHeader.tabsBackground", "#252526ff");
            m.insert("tab.inactiveBackground", "#2d2d2dff");
            m.insert("tab.border", "#252526ff");
            m.insert("statusBar.background", "#007accff");
            m.insert("editorWidget.background", "#252526ff");
            m.insert("overlayScrim", "#00000060");
            m.insert("list.activeSelectionBackground", "#094771ff");
            m.insert("icon.foreground", "#c5c5c5ff");
            m.insert("editorIndentGuide.background1", "#404040ff");
            m.insert("editorBracketMatch.background", "#0064001a");
            m.insert("terminal.background", "#1e1e1eff");
            m.insert("terminal.ansiBlack", "#000000ff");
            m.insert("terminal.ansiRed", "#cd3131ff");
            m.insert("terminal.ansiGreen", "#0dbc79ff");
            m.insert("terminal.ansiYellow", "#e5e510ff");
            m.insert("terminal.ansiBlue", "#2472c8ff");
            m.insert("terminal.ansiMagenta", "#bc3fbcff");
            m.insert("terminal.ansiCyan", "#11a8cdff");
            m.insert("terminal.ansiWhite", "#e5e5e5ff");
            m.insert("terminal.ansiBrightBlack", "#666666ff");
            m.insert("terminal.ansiBrightRed", "#f14c4cff");
            m.insert("terminal.ansiBrightGreen", "#23d18bff");
            m.insert("terminal.ansiBrightYellow", "#f5f543ff");
            m.insert("terminal.ansiBrightBlue", "#3b8eeaff");
            m.insert("terminal.ansiBrightMagenta", "#d670d6ff");
            m.insert("terminal.ansiBrightCyan", "#29b8dbff");
            m.insert("terminal.ansiBrightWhite", "#e5e5e5ff");
        }
        ThemeType::Light => {
            m.insert("editor.background", "#ffffffff");
            m.insert("editor.foreground", "#000000ff");
            m.insert("editor.selectionBackground", "#add6ffff");
            m.insert("editor.lineHighlightBackground", "#0000000c");
            m.insert("editor.findMatchBackground", "#a8ac94ff");
            m.insert("editorLineNumber.foreground", "#237893ff");
            m.insert("editorGroupHeader.tabsBackground", "#f3f3f3ff");
            m.insert("tab.inactiveBackground", "#ececeeff");
            m.insert("tab.border", "#f3f3f3ff");
            m.insert("statusBar.background", "#007accff");
            m.insert("editorWidget.background", "#f3f3f3ff");
            m.insert("overlayScrim", "#00000020");
            m.insert("list.activeSelectionBackground", "#0060c0ff");
            m.insert("icon.foreground", "#424242ff");
            m.insert("editorIndentGuide.background1", "#d3d3d3ff");
            m.insert("editorBracketMatch.background", "#0064001a");
            m.insert("terminal.background", "#ffffffff");
            m.insert("terminal.ansiBlack", "#000000ff");
            m.insert("terminal.ansiRed", "#cd3131ff");
            m.insert("terminal.ansiGreen", "#00bc00ff");
            m.insert("terminal.ansiYellow", "#949800ff");
            m.insert("terminal.ansiBlue", "#0451a5ff");
            m.insert("terminal.ansiMagenta", "#bc05bcff");
            m.insert("terminal.ansiCyan", "#0598bcff");
            m.insert("terminal.ansiWhite", "#555555ff");
            m.insert("terminal.ansiBrightBlack", "#666666ff");
            m.insert("terminal.ansiBrightRed", "#cd3131ff");
            m.insert("terminal.ansiBrightGreen", "#14ce14ff");
            m.insert("terminal.ansiBrightYellow", "#b5ba00ff");
            m.insert("terminal.ansiBrightBlue", "#0451a5ff");
            m.insert("terminal.ansiBrightMagenta", "#bc05bcff");
            m.insert("terminal.ansiBrightCyan", "#0598bcff");
            m.insert("terminal.ansiBrightWhite", "#a5a5a5ff");
        }
    }
    m
}

/// Verify `hex` parses as a colour; helpful as a smoke-test on the
/// produced theme before swapping it into live state.
#[allow(dead_code)]
pub fn validate_hex(hex: &str) -> bool {
    parse_hex_color(hex).is_some()
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
        let p = std::env::temp_dir().join(format!("editor-config-vscode-{pid}-{n}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn strip_line_comments() {
        let src = r##"{
            // a top-level comment
            "a": 1 // trailing
        }"##;
        let s = strip_jsonc(src);
        assert!(!s.contains("//"));
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn strip_block_comments() {
        let src = r##"{ /* hi */ "a": 2 /* there */ }"##;
        let s = strip_jsonc(src);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["a"], 2);
    }

    #[test]
    fn preserve_double_slash_in_strings() {
        let src = r##"{ "url": "http://example.com" }"##;
        let s = strip_jsonc(src);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["url"], "http://example.com");
    }

    #[test]
    fn strip_trailing_commas() {
        let src = r##"{ "a": [1, 2, 3,], "b": 4, }"##;
        let s = strip_jsonc(src);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["a"][2], 3);
        assert_eq!(v["b"], 4);
    }

    #[test]
    fn single_file_dark_theme_loads() {
        let root = tempdir();
        let path = root.join("dark.json");
        std::fs::write(
            &path,
            r##"{
                "name": "DemoDark",
                "type": "dark",
                "colors": {
                    "editor.background": "#101010",
                    "editor.foreground": "#eeeeee",
                    "editorCursor.foreground": "#ff0000",
                    "terminal.ansiRed": "#ff5555"
                },
                "tokenColors": [
                    { "scope": "comment", "settings": { "foreground": "#888888" } },
                    { "scope": ["keyword", "keyword.control"], "settings": { "foreground": "#cc88ff" } }
                ]
            }"##,
        )
        .unwrap();

        let theme = load_vscode_theme(&path).unwrap();
        assert_eq!(theme.editor.background, "#101010");
        assert_eq!(theme.editor.text_fg, "#eeeeee");
        assert_eq!(theme.editor.caret, "#ff0000");
        // Override is picked from `colors`.
        assert_eq!(theme.terminal.palette[1], "#ff5555");
        // Missing palette slots come from the dark defaults table.
        assert_eq!(theme.terminal.palette[0], "#000000ff");
        // Syntax buckets read tokenColors.
        assert_eq!(theme.syntax.comment, "#888888");
        assert_eq!(theme.syntax.keyword, "#cc88ff");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn include_chain_overlays_in_order() {
        let root = tempdir();
        let base = root.join("base.json");
        let leaf = root.join("leaf.json");
        std::fs::write(
            &base,
            r##"{
                "type": "dark",
                "colors": {
                    "editor.background": "#000000",
                    "editor.foreground": "#aaaaaa"
                },
                "tokenColors": [
                    { "scope": "comment", "settings": { "foreground": "#444444" } }
                ]
            }"##,
        )
        .unwrap();
        std::fs::write(
            &leaf,
            r##"{
                "include": "./base.json",
                "colors": {
                    "editor.background": "#101010"
                },
                "tokenColors": [
                    { "scope": "keyword", "settings": { "foreground": "#88ccff" } }
                ]
            }"##,
        )
        .unwrap();
        let theme = load_vscode_theme(&leaf).unwrap();
        // Leaf override wins for `colors`.
        assert_eq!(theme.editor.background, "#101010");
        // Base values still present for unoverridden keys.
        assert_eq!(theme.editor.text_fg, "#aaaaaa");
        // Concatenated tokenColors: both rules visible in syntax.
        assert_eq!(theme.syntax.comment, "#444444");
        assert_eq!(theme.syntax.keyword, "#88ccff");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn longest_prefix_scope_wins() {
        let root = tempdir();
        let path = root.join("t.json");
        std::fs::write(
            &path,
            r##"{
                "type": "dark",
                "tokenColors": [
                    { "scope": "keyword", "settings": { "foreground": "#aaaaaa" } },
                    { "scope": "keyword.control", "settings": { "foreground": "#bbbbbb" } }
                ]
            }"##,
        )
        .unwrap();
        let theme = load_vscode_theme(&path).unwrap();
        // Both rules match the `keyword` bucket via prefix; the
        // longer scope wins.
        assert_eq!(theme.syntax.keyword, "#bbbbbb");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn descendant_selector_uses_leaf_scope() {
        let root = tempdir();
        let path = root.join("t.json");
        std::fs::write(
            &path,
            r##"{
                "type": "dark",
                "tokenColors": [
                    {
                        "scope": "source.js entity.name.function",
                        "settings": { "foreground": "#dccc99" }
                    }
                ]
            }"##,
        )
        .unwrap();
        let theme = load_vscode_theme(&path).unwrap();
        assert_eq!(theme.syntax.function, "#dccc99");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn missing_keys_fall_back_to_defaults_table() {
        let root = tempdir();
        let path = root.join("sparse.json");
        std::fs::write(&path, r##"{ "type": "dark", "colors": {} }"##).unwrap();
        let theme = load_vscode_theme(&path).unwrap();
        // From the dark defaults table.
        assert_eq!(theme.editor.background, "#1e1e1eff");
        assert_eq!(theme.terminal.palette[1], "#cd3131ff");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn light_type_uses_light_defaults() {
        let root = tempdir();
        let path = root.join("light.json");
        std::fs::write(&path, r##"{ "type": "light", "colors": {} }"##).unwrap();
        let theme = load_vscode_theme(&path).unwrap();
        assert_eq!(theme.editor.background, "#ffffffff");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn include_cycle_returns_error() {
        let root = tempdir();
        let a = root.join("a.json");
        let b = root.join("b.json");
        std::fs::write(&a, r##"{ "include": "./b.json", "colors": {} }"##).unwrap();
        std::fs::write(&b, r##"{ "include": "./a.json", "colors": {} }"##).unwrap();
        let err = load_vscode_theme(&a).unwrap_err();
        assert!(matches!(err, VscodeThemeError::IncludeCycle(_)));
        std::fs::remove_dir_all(&root).ok();
    }
}
