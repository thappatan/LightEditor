//! Syntax highlighting via tree-sitter (spec §3.3, §6 M2).
//!
//! Wraps a `tree_sitter::Parser` and exposes a stable `Highlight` type so
//! callers can stay independent of tree-sitter's node-kind strings. Only
//! Rust is wired up in this first cut; more languages slot in via the
//! `Language` enum and `classify_*` functions.
//!
//! Highlights are emitted at *leaf* token nodes, so they never overlap.
//! Char-indexed ranges (not byte-indexed) line up with `editor-core`'s
//! `Selection` model.

use std::ops::Range;
use std::path::Path;

use tree_sitter::{Language as TsLanguage, Node, Parser};

/// Languages we have a grammar for. New entries plug into `for_path`,
/// `ts_language`, and a matching `classify_*` function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
}

impl Language {
    /// Pick a language for `path` based on its extension. Returns `None`
    /// for unknown extensions or pathless docs.
    pub fn for_path(path: &Path) -> Option<Language> {
        let ext = path.extension()?.to_str()?;
        match ext.to_ascii_lowercase().as_str() {
            "rs" => Some(Language::Rust),
            _ => None,
        }
    }

    fn ts_language(self) -> TsLanguage {
        match self {
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
        }
    }
}

/// Coarse-grained highlight buckets — themes map these to colors. Adding a
/// category is a one-line change here plus the per-language `classify_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HighlightCategory {
    Keyword,
    StringLit,
    Number,
    Comment,
    Type,
    Function,
    Punctuation,
}

/// One contiguous highlight span. `range` is in `char` indices (not bytes)
/// so it matches `editor-core`'s `Selection` model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Highlight {
    pub range: Range<usize>,
    pub category: HighlightCategory,
}

/// A reusable parser bound to one language. `highlight` re-parses the whole
/// text every call — incremental parsing via stored old-tree is a follow-up.
pub struct Highlighter {
    parser: Parser,
}

impl Highlighter {
    /// Build a highlighter for `lang`. Returns `Err` only when tree-sitter
    /// rejects the language bindings (ABI mismatch, etc.).
    pub fn new(lang: Language) -> Result<Self, tree_sitter::LanguageError> {
        let mut parser = Parser::new();
        parser.set_language(&lang.ts_language())?;
        Ok(Self { parser })
    }

    /// Parse `text` and return every interesting leaf node as a `Highlight`.
    /// Empty input returns an empty `Vec`. Tree-sitter parse failure
    /// (e.g. invalid UTF-8 in source) is also treated as no highlights.
    pub fn highlight(&mut self, text: &str) -> Vec<Highlight> {
        if text.is_empty() {
            return Vec::new();
        }
        let Some(tree) = self.parser.parse(text, None) else {
            return Vec::new();
        };
        let byte_to_char = build_byte_to_char_map(text);
        let mut out = Vec::new();
        collect_rust(&tree.root_node(), &byte_to_char, &mut out);
        out
    }
}

/// Build a `byte → char` lookup over `text`. Length = `text.len() + 1`.
/// Looking up a non-char-boundary byte yields the char index of the *start*
/// of the char that byte sits inside.
fn build_byte_to_char_map(text: &str) -> Vec<usize> {
    let mut map = vec![0usize; text.len() + 1];
    let mut char_idx = 0;
    let mut last_byte = 0;
    for (byte, c) in text.char_indices() {
        for slot in map.iter_mut().take(byte).skip(last_byte) {
            *slot = char_idx;
        }
        map[byte] = char_idx;
        last_byte = byte + c.len_utf8();
        char_idx += 1;
    }
    for slot in map.iter_mut().skip(last_byte) {
        *slot = char_idx;
    }
    map
}

fn collect_rust(node: &Node, byte_to_char: &[usize], out: &mut Vec<Highlight>) {
    if let Some(cat) = classify_rust(node.kind()) {
        let start = byte_to_char
            .get(node.start_byte())
            .copied()
            .unwrap_or_default();
        let end = byte_to_char
            .get(node.end_byte())
            .copied()
            .unwrap_or_default();
        if start < end {
            out.push(Highlight {
                range: start..end,
                category: cat,
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_rust(&child, byte_to_char, out);
    }
}

/// Map tree-sitter-rust node kinds onto our coarse categories. Anonymous
/// keyword nodes carry the keyword text itself as their `kind()`, which is
/// what powers the long match arm.
fn classify_rust(kind: &str) -> Option<HighlightCategory> {
    use HighlightCategory::*;
    match kind {
        "line_comment" | "block_comment" => Some(Comment),
        "string_literal" | "raw_string_literal" | "char_literal" => Some(StringLit),
        "integer_literal" | "float_literal" => Some(Number),
        "type_identifier" | "primitive_type" => Some(Type),
        "fn" | "let" | "mut" | "pub" | "const" | "static" | "use" | "mod" | "struct" | "enum"
        | "impl" | "trait" | "for" | "in" | "if" | "else" | "while" | "loop" | "match"
        | "return" | "break" | "continue" | "as" | "ref" | "self" | "Self" | "true" | "false"
        | "where" | "async" | "await" | "move" | "type" | "extern" | "crate" | "dyn" | "unsafe"
        | "yield" => Some(Keyword),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn categories(text: &str) -> Vec<HighlightCategory> {
        let mut h = Highlighter::new(Language::Rust).expect("rust grammar");
        h.highlight(text).into_iter().map(|h| h.category).collect()
    }

    #[test]
    fn empty_input_no_highlights() {
        let mut h = Highlighter::new(Language::Rust).unwrap();
        assert!(h.highlight("").is_empty());
    }

    #[test]
    fn for_path_recognizes_rs() {
        assert_eq!(
            Language::for_path(Path::new("foo/bar.rs")),
            Some(Language::Rust)
        );
        assert_eq!(Language::for_path(Path::new("foo/bar.txt")), None);
        assert_eq!(Language::for_path(Path::new("noext")), None);
    }

    #[test]
    fn highlights_let_keyword() {
        let cats = categories("let x = 1;");
        assert!(cats.contains(&HighlightCategory::Keyword));
        assert!(cats.contains(&HighlightCategory::Number));
    }

    #[test]
    fn highlights_string_literal() {
        let cats = categories(r#"let s = "hello";"#);
        assert!(cats.contains(&HighlightCategory::StringLit));
    }

    #[test]
    fn highlights_line_comment() {
        let cats = categories("// hi\nfn x() {}");
        assert!(cats.contains(&HighlightCategory::Comment));
        // The fn keyword should also be present.
        assert!(cats.contains(&HighlightCategory::Keyword));
    }

    #[test]
    fn highlights_type_identifier() {
        let cats = categories("fn x() -> Vec<u32> { todo!() }");
        assert!(cats.contains(&HighlightCategory::Type));
    }

    #[test]
    fn char_indexed_ranges_handle_thai() {
        let mut h = Highlighter::new(Language::Rust).unwrap();
        // The string body is Thai; tree-sitter sees the byte span, we
        // convert to char indices.
        let src = r#"// สวัสดี
fn x() {}
"#;
        let hs = h.highlight(src);
        let comment = hs
            .iter()
            .find(|h| h.category == HighlightCategory::Comment)
            .expect("comment present");
        // The comment runs from char 0 to the first newline char.
        let total_chars = src.chars().count();
        assert!(comment.range.start < total_chars);
        assert!(comment.range.end <= total_chars);
        assert!(comment.range.end > comment.range.start);
    }

    #[test]
    fn highlights_are_non_overlapping_in_order() {
        let mut h = Highlighter::new(Language::Rust).unwrap();
        let src = "let foo = 42;";
        let hs = h.highlight(src);
        // Sort by start; expect strict non-overlapping when sorted.
        let mut sorted = hs.clone();
        sorted.sort_by_key(|h| h.range.start);
        for w in sorted.windows(2) {
            assert!(
                w[0].range.end <= w[1].range.start,
                "overlap: {:?} vs {:?}",
                w[0],
                w[1]
            );
        }
    }
}
