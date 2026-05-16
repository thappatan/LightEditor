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
/// `ts_language`, and the per-language `classify_*` function selected by
/// `Highlighter::highlight`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    TypeScript,
    Tsx,
    JavaScript,
    Json,
    Python,
    Go,
    C,
    Markdown,
    Toml,
    Yaml,
    Dart,
}

impl Language {
    /// Pick a language for `path` based on its extension. Returns `None`
    /// for unknown extensions or pathless docs.
    pub fn for_path(path: &Path) -> Option<Language> {
        let ext = path.extension()?.to_str()?;
        match ext.to_ascii_lowercase().as_str() {
            "rs" => Some(Language::Rust),
            "ts" => Some(Language::TypeScript),
            "tsx" => Some(Language::Tsx),
            "js" | "jsx" | "mjs" | "cjs" => Some(Language::JavaScript),
            "json" => Some(Language::Json),
            "py" | "pyi" => Some(Language::Python),
            "go" => Some(Language::Go),
            "c" | "h" => Some(Language::C),
            "md" | "markdown" => Some(Language::Markdown),
            "toml" => Some(Language::Toml),
            "yaml" | "yml" => Some(Language::Yaml),
            "dart" => Some(Language::Dart),
            _ => None,
        }
    }

    fn ts_language(self) -> TsLanguage {
        match self {
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Language::Json => tree_sitter_json::LANGUAGE.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            Language::Go => tree_sitter_go::LANGUAGE.into(),
            Language::C => tree_sitter_c::LANGUAGE.into(),
            Language::Markdown => tree_sitter_md::LANGUAGE.into(),
            Language::Toml => tree_sitter_toml_ng::LANGUAGE.into(),
            Language::Yaml => tree_sitter_yaml::LANGUAGE.into(),
            Language::Dart => tree_sitter_dart::LANGUAGE.into(),
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
    lang: Language,
}

impl Highlighter {
    /// Build a highlighter for `lang`. Returns `Err` only when tree-sitter
    /// rejects the language bindings (ABI mismatch, etc.).
    pub fn new(lang: Language) -> Result<Self, tree_sitter::LanguageError> {
        let mut parser = Parser::new();
        parser.set_language(&lang.ts_language())?;
        Ok(Self { parser, lang })
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
        let classifier = classifier_for(self.lang);
        let mut out = Vec::new();
        collect(
            &tree.root_node(),
            None,
            None,
            &byte_to_char,
            classifier,
            &mut out,
        );
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

/// Per-language classifier function pointer. The first argument is the
/// node's kind; the second is the parent's kind (or `None` at root); the
/// third is the field name the node occupies inside its parent. Returning
/// `None` falls through to the recursive walk and lets a more specific
/// child node provide the highlight.
type Classifier = fn(&str, Option<&str>, Option<&str>) -> Option<HighlightCategory>;

fn classifier_for(lang: Language) -> Classifier {
    match lang {
        Language::Rust => classify_rust,
        Language::TypeScript | Language::Tsx => classify_typescript,
        Language::JavaScript => classify_javascript,
        Language::Json => classify_json,
        Language::Python => classify_python,
        Language::Go => classify_go,
        Language::C => classify_c,
        Language::Markdown => classify_markdown,
        Language::Toml => classify_toml,
        Language::Yaml => classify_yaml,
        Language::Dart => classify_dart,
    }
}

fn collect(
    node: &Node,
    parent_kind: Option<&str>,
    parent_field: Option<&str>,
    byte_to_char: &[usize],
    classify: Classifier,
    out: &mut Vec<Highlight>,
) {
    if let Some(cat) = classify(node.kind(), parent_kind, parent_field) {
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
    // Walk children manually so we can pass field-name info downward — the
    // `children(&mut cursor)` iterator borrows the cursor exclusively which
    // makes querying `cursor.field_name()` mid-iteration awkward.
    let this_kind = node.kind();
    for i in 0..node.child_count() {
        let Some(child) = node.child(i) else {
            continue;
        };
        let field = node.field_name_for_child(i as u32);
        collect(&child, Some(this_kind), field, byte_to_char, classify, out);
    }
}

/// tree-sitter-rust. Anonymous keyword nodes carry the keyword text itself
/// as their `kind()`, which is what powers the long keyword match arm.
/// Specific context-sensitive cases (function names, macro invocations,
/// field access, lifetimes) check `parent_kind` / `field` first.
fn classify_rust(
    kind: &str,
    parent: Option<&str>,
    field: Option<&str>,
) -> Option<HighlightCategory> {
    use HighlightCategory::*;
    // Field-specific identifier classifications. Each line is
    // (parent_kind, field_name) → category.
    if kind == "identifier" {
        match (parent, field) {
            (Some("function_item"), Some("name")) => return Some(Function),
            (Some("function_signature_item"), Some("name")) => return Some(Function),
            (Some("call_expression"), Some("function")) => return Some(Function),
            (Some("macro_invocation"), Some("macro")) => return Some(Function),
            _ => {}
        }
    }
    match kind {
        "line_comment" | "block_comment" => Some(Comment),
        "string_literal" | "raw_string_literal" | "char_literal" => Some(StringLit),
        "integer_literal" | "float_literal" => Some(Number),
        "type_identifier" | "primitive_type" => Some(Type),
        // Lifetimes — `'a`, `'static`. Distinct enough to colour as Type.
        "lifetime" => Some(Type),
        // `obj.field` — colour the field name like punctuation so it sits
        // between value-loud and keyword-loud.
        "field_identifier" => Some(Punctuation),
        "fn" | "let" | "mut" | "pub" | "const" | "static" | "use" | "mod" | "struct" | "enum"
        | "impl" | "trait" | "for" | "in" | "if" | "else" | "while" | "loop" | "match"
        | "return" | "break" | "continue" | "as" | "ref" | "self" | "Self" | "true" | "false"
        | "where" | "async" | "await" | "move" | "type" | "extern" | "crate" | "dyn" | "unsafe"
        | "yield" => Some(Keyword),
        _ => None,
    }
}

/// tree-sitter-typescript (also serves TSX). Function-context identifiers
/// (function_declaration.name, call_expression.function, method names) get
/// the Function colour; other identifiers fall through to default text.
fn classify_typescript(
    kind: &str,
    parent: Option<&str>,
    field: Option<&str>,
) -> Option<HighlightCategory> {
    use HighlightCategory::*;
    if kind == "identifier" || kind == "property_identifier" {
        match (parent, field) {
            (Some("function_declaration"), Some("name"))
            | (Some("function_signature"), Some("name"))
            | (Some("method_definition"), Some("name"))
            | (Some("method_signature"), Some("name")) => return Some(Function),
            (Some("call_expression"), Some("function")) => return Some(Function),
            _ => {}
        }
    }
    match kind {
        "comment" => Some(Comment),
        "string" | "string_fragment" | "template_string" | "regex" | "regex_pattern" => {
            Some(StringLit)
        }
        "number" => Some(Number),
        "type_identifier" | "predefined_type" => Some(Type),
        "const" | "let" | "var" | "function" | "class" | "interface" | "type" | "enum"
        | "namespace" | "module" | "if" | "else" | "return" | "for" | "while" | "do" | "switch"
        | "case" | "default" | "break" | "continue" | "throw" | "try" | "catch" | "finally"
        | "import" | "from" | "export" | "as" | "extends" | "implements" | "new" | "this"
        | "super" | "in" | "of" | "typeof" | "instanceof" | "void" | "delete" | "yield"
        | "async" | "await" | "true" | "false" | "null" | "undefined" | "static" | "public"
        | "private" | "protected" | "readonly" | "abstract" | "declare" | "satisfies" | "keyof"
        | "infer" | "is" | "asserts" => Some(Keyword),
        _ => None,
    }
}

/// tree-sitter-javascript — same context-sensitive function rules as TS
/// minus the TS-only keywords.
fn classify_javascript(
    kind: &str,
    parent: Option<&str>,
    field: Option<&str>,
) -> Option<HighlightCategory> {
    use HighlightCategory::*;
    if kind == "identifier" || kind == "property_identifier" {
        match (parent, field) {
            (Some("function_declaration"), Some("name"))
            | (Some("method_definition"), Some("name")) => return Some(Function),
            (Some("call_expression"), Some("function")) => return Some(Function),
            _ => {}
        }
    }
    match kind {
        "comment" => Some(Comment),
        "string" | "string_fragment" | "template_string" | "regex" | "regex_pattern" => {
            Some(StringLit)
        }
        "number" => Some(Number),
        "const" | "let" | "var" | "function" | "class" | "if" | "else" | "return" | "for"
        | "while" | "do" | "switch" | "case" | "default" | "break" | "continue" | "throw"
        | "try" | "catch" | "finally" | "import" | "from" | "export" | "as" | "extends" | "new"
        | "this" | "super" | "in" | "of" | "typeof" | "instanceof" | "void" | "delete"
        | "yield" | "async" | "await" | "true" | "false" | "null" | "undefined" | "static" => {
            Some(Keyword)
        }
        _ => None,
    }
}

/// tree-sitter-json — small grammar, four buckets.
fn classify_json(
    kind: &str,
    _parent: Option<&str>,
    _field: Option<&str>,
) -> Option<HighlightCategory> {
    use HighlightCategory::*;
    match kind {
        "comment" => Some(Comment),
        "string" | "string_content" => Some(StringLit),
        "number" => Some(Number),
        "true" | "false" | "null" => Some(Keyword),
        _ => None,
    }
}

/// tree-sitter-python.
fn classify_python(
    kind: &str,
    _parent: Option<&str>,
    _field: Option<&str>,
) -> Option<HighlightCategory> {
    use HighlightCategory::*;
    match kind {
        "comment" => Some(Comment),
        "string" | "string_start" | "string_end" | "string_content" | "escape_sequence" => {
            Some(StringLit)
        }
        "integer" | "float" => Some(Number),
        "type" => Some(Type),
        "def" | "class" | "if" | "elif" | "else" | "while" | "for" | "in" | "not" | "and"
        | "or" | "is" | "return" | "import" | "from" | "as" | "try" | "except" | "finally"
        | "raise" | "pass" | "break" | "continue" | "lambda" | "yield" | "async" | "await"
        | "with" | "True" | "False" | "None" | "global" | "nonlocal" | "assert" | "del"
        | "match" | "case" => Some(Keyword),
        _ => None,
    }
}

/// tree-sitter-go.
fn classify_go(
    kind: &str,
    _parent: Option<&str>,
    _field: Option<&str>,
) -> Option<HighlightCategory> {
    use HighlightCategory::*;
    match kind {
        "comment" => Some(Comment),
        "interpreted_string_literal" | "raw_string_literal" | "rune_literal" => Some(StringLit),
        "int_literal" | "float_literal" | "imaginary_literal" => Some(Number),
        "type_identifier" | "predeclared_type" => Some(Type),
        "func" | "var" | "const" | "type" | "struct" | "interface" | "if" | "else" | "for"
        | "range" | "switch" | "case" | "default" | "break" | "continue" | "return" | "go"
        | "defer" | "select" | "chan" | "package" | "import" | "map" | "true" | "false" | "nil"
        | "fallthrough" | "goto" => Some(Keyword),
        _ => None,
    }
}

/// tree-sitter-c.
fn classify_c(
    kind: &str,
    _parent: Option<&str>,
    _field: Option<&str>,
) -> Option<HighlightCategory> {
    use HighlightCategory::*;
    match kind {
        "comment" => Some(Comment),
        "string_literal" | "char_literal" | "system_lib_string" => Some(StringLit),
        "number_literal" => Some(Number),
        "primitive_type" | "type_identifier" | "sized_type_specifier" => Some(Type),
        "int" | "char" | "float" | "double" | "void" | "long" | "short" | "unsigned" | "signed"
        | "const" | "static" | "extern" | "auto" | "register" | "volatile" | "if" | "else"
        | "for" | "while" | "do" | "switch" | "case" | "default" | "break" | "continue"
        | "return" | "goto" | "sizeof" | "typedef" | "struct" | "union" | "enum" | "inline"
        | "restrict" | "_Atomic" | "_Bool" | "true" | "false" | "NULL" => Some(Keyword),
        _ => None,
    }
}

/// tree-sitter-toml-ng.
fn classify_toml(
    kind: &str,
    _parent: Option<&str>,
    _field: Option<&str>,
) -> Option<HighlightCategory> {
    use HighlightCategory::*;
    match kind {
        "comment" => Some(Comment),
        "string"
        | "basic_string"
        | "literal_string"
        | "multiline_basic_string"
        | "multiline_literal_string" => Some(StringLit),
        "integer" | "float" => Some(Number),
        "boolean" | "true" | "false" => Some(Keyword),
        "offset_date_time" | "local_date_time" | "local_date" | "local_time" => Some(Type),
        "[" | "]" | "[[" | "]]" | "=" | "." | "," => Some(Punctuation),
        _ => None,
    }
}

/// tree-sitter-yaml.
fn classify_yaml(
    kind: &str,
    _parent: Option<&str>,
    _field: Option<&str>,
) -> Option<HighlightCategory> {
    use HighlightCategory::*;
    match kind {
        "comment" => Some(Comment),
        "string_scalar"
        | "single_quote_scalar"
        | "double_quote_scalar"
        | "block_scalar"
        | "plain_scalar" => Some(StringLit),
        "integer_scalar" | "float_scalar" => Some(Number),
        "boolean_scalar" | "null_scalar" => Some(Keyword),
        "anchor" | "alias" | "tag" => Some(Type),
        "-" | ":" | "|" | ">" | "?" | "&" | "*" | "!" => Some(Punctuation),
        _ => None,
    }
}

/// tree-sitter-dart.
fn classify_dart(
    kind: &str,
    _parent: Option<&str>,
    _field: Option<&str>,
) -> Option<HighlightCategory> {
    use HighlightCategory::*;
    match kind {
        "comment" | "documentation_comment" | "line_comment" | "block_comment" => Some(Comment),
        "string_literal" | "raw_string_literal" | "template_string_literal" => Some(StringLit),
        "decimal_integer_literal"
        | "hex_integer_literal"
        | "decimal_floating_point_literal"
        | "true"
        | "false" => Some(Number),
        "type_identifier" | "primitive_type" | "void_type" => Some(Type),
        "var" | "final" | "const" | "late" | "static" | "abstract" | "class" | "extends"
        | "implements" | "with" | "mixin" | "enum" | "typedef" | "if" | "else" | "for" | "in"
        | "while" | "do" | "switch" | "case" | "default" | "break" | "continue" | "return"
        | "throw" | "try" | "catch" | "finally" | "on" | "rethrow" | "yield" | "async"
        | "await" | "sync" | "new" | "this" | "super" | "is" | "as" | "null" | "void"
        | "import" | "export" | "library" | "part" | "show" | "hide" | "deferred" | "factory"
        | "external" | "operator" | "get" | "set" | "covariant" | "required" => Some(Keyword),
        _ => None,
    }
}

/// tree-sitter-md (CommonMark). Block-structure grammar — we surface the
/// leaf markers and code spans, treat headings as keywords.
fn classify_markdown(
    kind: &str,
    _parent: Option<&str>,
    _field: Option<&str>,
) -> Option<HighlightCategory> {
    use HighlightCategory::*;
    match kind {
        "atx_h1_marker"
        | "atx_h2_marker"
        | "atx_h3_marker"
        | "atx_h4_marker"
        | "atx_h5_marker"
        | "atx_h6_marker"
        | "setext_h1_underline"
        | "setext_h2_underline" => Some(Keyword),
        "code_fence_content" | "fenced_code_block" | "indented_code_block" | "code_span" => {
            Some(StringLit)
        }
        "info_string" => Some(Type),
        "list_marker_minus"
        | "list_marker_plus"
        | "list_marker_star"
        | "list_marker_dot"
        | "list_marker_parenthesis"
        | "block_quote_marker"
        | "thematic_break" => Some(Punctuation),
        "link_destination" | "link_label" | "uri_autolink" => Some(StringLit),
        "html_block" | "html_tag" => Some(Comment),
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

    fn categories_with(lang: Language, text: &str) -> Vec<HighlightCategory> {
        let mut h = Highlighter::new(lang).expect("grammar");
        h.highlight(text).into_iter().map(|h| h.category).collect()
    }

    #[test]
    fn for_path_recognizes_every_language() {
        let cases = [
            ("a.rs", Language::Rust),
            ("a.ts", Language::TypeScript),
            ("a.tsx", Language::Tsx),
            ("a.js", Language::JavaScript),
            ("a.jsx", Language::JavaScript),
            ("a.mjs", Language::JavaScript),
            ("a.json", Language::Json),
            ("a.py", Language::Python),
            ("a.go", Language::Go),
            ("a.c", Language::C),
            ("a.h", Language::C),
            ("a.md", Language::Markdown),
        ];
        for (name, expected) in cases {
            assert_eq!(
                Language::for_path(Path::new(name)),
                Some(expected),
                "{name}"
            );
        }
    }

    #[test]
    fn typescript_highlights_keywords_and_types() {
        let cats = categories_with(
            Language::TypeScript,
            "const x: number = 1;\ninterface A {}\n",
        );
        assert!(cats.contains(&HighlightCategory::Keyword));
        assert!(cats.contains(&HighlightCategory::Number));
        // `number` is a predefined_type
        assert!(cats.contains(&HighlightCategory::Type));
    }

    #[test]
    fn tsx_uses_typescript_classifier() {
        let cats = categories_with(Language::Tsx, "const x = <div>hi</div>;");
        assert!(cats.contains(&HighlightCategory::Keyword));
    }

    #[test]
    fn javascript_highlights_keywords() {
        let cats = categories_with(Language::JavaScript, "let x = 1;\nfunction f() {}\n");
        assert!(cats.contains(&HighlightCategory::Keyword));
        assert!(cats.contains(&HighlightCategory::Number));
    }

    #[test]
    fn json_highlights_string_and_bool() {
        let cats = categories_with(Language::Json, r#"{"a": "hello", "b": true, "c": 42}"#);
        assert!(cats.contains(&HighlightCategory::StringLit));
        assert!(cats.contains(&HighlightCategory::Keyword)); // true
        assert!(cats.contains(&HighlightCategory::Number));
    }

    #[test]
    fn python_highlights_def_and_string() {
        let cats = categories_with(Language::Python, "def hello():\n    return \"world\"\n");
        assert!(cats.contains(&HighlightCategory::Keyword));
        assert!(cats.contains(&HighlightCategory::StringLit));
    }

    #[test]
    fn go_highlights_func_and_types() {
        let cats = categories_with(
            Language::Go,
            "package main\nfunc main() { var x int = 1 }\n",
        );
        assert!(cats.contains(&HighlightCategory::Keyword));
        assert!(cats.contains(&HighlightCategory::Type));
        assert!(cats.contains(&HighlightCategory::Number));
    }

    #[test]
    fn c_highlights_primitives_and_keywords() {
        let cats = categories_with(Language::C, "int main() { return 0; }\n");
        assert!(cats.contains(&HighlightCategory::Type));
        assert!(cats.contains(&HighlightCategory::Keyword));
        assert!(cats.contains(&HighlightCategory::Number));
    }

    #[test]
    fn toml_highlights_string_and_number() {
        let src = "[package]\nname = \"editor\"\nversion = 42\n";
        let cats = categories_with(Language::Toml, src);
        assert!(cats.contains(&HighlightCategory::StringLit));
        assert!(cats.contains(&HighlightCategory::Number));
    }

    #[test]
    fn yaml_highlights_string_and_keyword() {
        let src = "name: editor\nversion: 1\nactive: true\n";
        let cats = categories_with(Language::Yaml, src);
        // plain scalars carry both value text + booleans
        assert!(cats.contains(&HighlightCategory::StringLit));
    }

    #[test]
    fn dart_highlights_class_and_string() {
        let src = "void main() {\n  var s = \"hello\";\n  print(s);\n}\n";
        let cats = categories_with(Language::Dart, src);
        assert!(cats.contains(&HighlightCategory::Keyword)); // void / var
        assert!(cats.contains(&HighlightCategory::StringLit));
    }

    #[test]
    fn for_path_recognizes_new_extensions() {
        assert_eq!(
            Language::for_path(Path::new("a.toml")),
            Some(Language::Toml)
        );
        assert_eq!(
            Language::for_path(Path::new("a.yaml")),
            Some(Language::Yaml)
        );
        assert_eq!(Language::for_path(Path::new("a.yml")), Some(Language::Yaml));
        assert_eq!(
            Language::for_path(Path::new("a.dart")),
            Some(Language::Dart)
        );
    }

    #[test]
    fn markdown_highlights_headings() {
        let cats = categories_with(Language::Markdown, "# Title\n\nsome text\n");
        // atx_h1_marker → Keyword
        assert!(cats.contains(&HighlightCategory::Keyword));
    }

    #[test]
    fn markdown_recognises_list_markers() {
        let cats = categories_with(Language::Markdown, "- a\n- b\n");
        // list_marker_minus → Punctuation
        assert!(cats.contains(&HighlightCategory::Punctuation));
    }

    /// Walk the tree of a code-fenced Markdown sample and dump every node
    /// kind. Useful when wiring more markdown nodes into the classifier —
    /// kept as a `#[ignore]`d test so it doesn't run in CI but stays
    /// runnable on demand with `cargo test -- --ignored md_node_kinds`.
    #[test]
    #[ignore]
    fn md_node_kinds_dump() {
        use tree_sitter::Parser;
        let mut p = Parser::new();
        p.set_language(&Language::Markdown.ts_language()).unwrap();
        let tree = p
            .parse("# h\n\n`x`\n\n```rust\nfn x(){}\n```\n", None)
            .unwrap();
        fn walk(n: tree_sitter::Node, depth: usize) {
            for _ in 0..depth {
                print!("  ");
            }
            println!("{} [{}..{}]", n.kind(), n.start_byte(), n.end_byte());
            let mut c = n.walk();
            for child in n.children(&mut c) {
                walk(child, depth + 1);
            }
        }
        walk(tree.root_node(), 0);
    }

    fn highlights_of(lang: Language, text: &str) -> Vec<Highlight> {
        let mut h = Highlighter::new(lang).expect("grammar");
        h.highlight(text)
    }

    #[test]
    fn rust_highlights_function_name_and_call() {
        let src = "fn hello() {}\nfn main() { hello(); }\n";
        let hs = highlights_of(Language::Rust, src);
        let func_ranges: Vec<_> = hs
            .iter()
            .filter(|h| h.category == HighlightCategory::Function)
            .map(|h| h.range.clone())
            .collect();
        // Expect at least three function highlights: `hello` (def),
        // `main` (def), `hello` (call).
        assert!(
            func_ranges.len() >= 3,
            "expected >=3 function highlights, got {:?}",
            func_ranges
        );
    }

    #[test]
    fn rust_highlights_macro_invocation() {
        let src = "fn x() { println!(\"hi\"); }";
        let hs = highlights_of(Language::Rust, src);
        // `println` should be a Function highlight via macro_invocation.macro.
        assert!(hs.iter().any(|h| h.category == HighlightCategory::Function));
    }

    #[test]
    fn rust_highlights_lifetime() {
        let src = "fn x<'a>(s: &'a str) {}";
        let hs = highlights_of(Language::Rust, src);
        // Two `'a` lifetimes appear (declaration + reference).
        let lifetime_count = hs
            .iter()
            .filter(|h| h.category == HighlightCategory::Type)
            .count();
        assert!(
            lifetime_count >= 2,
            "expected at least 2 type/lifetime tokens"
        );
    }

    #[test]
    fn rust_highlights_field_access() {
        let src = "fn x(p: Foo) { p.bar }";
        let hs = highlights_of(Language::Rust, src);
        assert!(hs
            .iter()
            .any(|h| h.category == HighlightCategory::Punctuation));
    }

    #[test]
    fn typescript_highlights_function_declaration_name() {
        let src = "function greet() { return 1; }";
        let hs = highlights_of(Language::TypeScript, src);
        assert!(hs.iter().any(|h| h.category == HighlightCategory::Function));
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
