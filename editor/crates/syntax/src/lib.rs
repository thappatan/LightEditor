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

use editor_buffer::BufferDelta;
use tree_sitter::{InputEdit, Language as TsLanguage, Node, Parser, Point, Tree};

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
    Bash,
    Lua,
    Ruby,
    Html,
    Css,
    Java,
    Swift,
    Cpp,
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
            "sh" | "bash" | "zsh" => Some(Language::Bash),
            "lua" => Some(Language::Lua),
            "rb" | "rbw" => Some(Language::Ruby),
            "html" | "htm" | "xhtml" => Some(Language::Html),
            "css" => Some(Language::Css),
            "java" => Some(Language::Java),
            "swift" => Some(Language::Swift),
            "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => Some(Language::Cpp),
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
            Language::Bash => tree_sitter_bash::LANGUAGE.into(),
            Language::Lua => tree_sitter_lua::LANGUAGE.into(),
            Language::Ruby => tree_sitter_ruby::LANGUAGE.into(),
            Language::Html => tree_sitter_html::LANGUAGE.into(),
            Language::Css => tree_sitter_css::LANGUAGE.into(),
            Language::Java => tree_sitter_java::LANGUAGE.into(),
            Language::Swift => tree_sitter_swift::LANGUAGE.into(),
            Language::Cpp => tree_sitter_cpp::LANGUAGE.into(),
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
    /// Identifiers that name a value: local variables, parameters, and
    /// object-property *reads* (`res.statusCode`). VSCode paints these
    /// light blue (`#9cdcfe`); without the bucket they fall through to
    /// default text and most of a typical file ends up uncoloured.
    Variable,
    Punctuation,
}

/// One contiguous highlight span. `range` is in `char` indices (not bytes)
/// so it matches `editor-core`'s `Selection` model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Highlight {
    pub range: Range<usize>,
    pub category: HighlightCategory,
}

/// A reusable parser bound to one language.
///
/// `highlight` parses the text and stashes the resulting `Tree` for the next
/// call. Feeding edits via [`apply_edit`](Highlighter::apply_edit) between
/// parses lets tree-sitter touch only the changed region instead of
/// reparsing the whole file — typically sub-millisecond on long buffers.
/// Wholesale buffer replacements (undo, redo, file reload) must be signalled
/// via [`reset`](Highlighter::reset) so the stale tree is dropped.
pub struct Highlighter {
    parser: Parser,
    lang: Language,
    /// Most recent parse tree, with any pending edits already applied via
    /// `tree.edit()`. `None` after construction or after `reset`.
    old_tree: Option<Tree>,
}

impl Highlighter {
    /// Build a highlighter for `lang`. Returns `Err` only when tree-sitter
    /// rejects the language bindings (ABI mismatch, etc.).
    pub fn new(lang: Language) -> Result<Self, tree_sitter::LanguageError> {
        let mut parser = Parser::new();
        parser.set_language(&lang.ts_language())?;
        Ok(Self {
            parser,
            lang,
            old_tree: None,
        })
    }

    /// Record one buffer edit against the cached parse tree. Cheap (O(log n)
    /// inside tree-sitter); call once per [`BufferDelta`] the editor emits
    /// before the next [`highlight`](Highlighter::highlight) call. No-op
    /// when there is no cached tree yet — the next parse will be from
    /// scratch anyway.
    pub fn apply_edit(&mut self, delta: &BufferDelta) {
        if let Some(tree) = &mut self.old_tree {
            tree.edit(&InputEdit {
                start_byte: delta.start_byte,
                old_end_byte: delta.old_end_byte,
                new_end_byte: delta.new_end_byte,
                start_position: point_of(delta.start_point),
                old_end_position: point_of(delta.old_end_point),
                new_end_position: point_of(delta.new_end_point),
            });
        }
    }

    /// Discard the cached parse tree. Use after undo/redo or any other
    /// wholesale buffer replacement, so the next `highlight` reparses from
    /// scratch instead of trying to reconcile with a stale tree.
    pub fn reset(&mut self) {
        self.old_tree = None;
    }

    /// Parse `text` and return every interesting leaf node as a `Highlight`.
    /// Empty input returns an empty `Vec`. Tree-sitter parse failure
    /// (e.g. invalid UTF-8 in source) is also treated as no highlights.
    ///
    /// When a cached tree is present (with edits already applied), the
    /// parse is incremental — tree-sitter only re-examines the affected
    /// subtrees.
    pub fn highlight(&mut self, text: &str) -> Vec<Highlight> {
        if text.is_empty() {
            // An empty buffer has no meaningful tree to cache.
            self.old_tree = None;
            return Vec::new();
        }
        let Some(tree) = self.parser.parse(text, self.old_tree.as_ref()) else {
            return Vec::new();
        };
        let byte_to_char = build_byte_to_char_map(text);
        let classifier = classifier_for(self.lang);
        let mut out = Vec::new();
        collect(
            &tree.root_node(),
            None,
            None,
            None,
            None,
            &byte_to_char,
            classifier,
            &mut out,
        );
        self.old_tree = Some(tree);
        out
    }
}

fn point_of(p: editor_buffer::BytePoint) -> Point {
    Point::new(p.row, p.column)
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

/// Context handed to a classifier for one node. Carries two ancestor
/// levels so rules can distinguish e.g. a plain property read
/// (`obj.prop`) from a method call (`obj.method()`): the property name
/// is a `property_identifier` under a `member_expression`, but only in
/// the call case is that `member_expression` the `function` field of a
/// `call_expression` (the grandparent).
#[derive(Clone, Copy)]
pub struct NodeCtx<'a> {
    /// The node's own kind.
    pub kind: &'a str,
    /// Parent kind, or `None` at the root.
    pub parent: Option<&'a str>,
    /// Field name the node occupies inside its parent.
    pub field: Option<&'a str>,
    /// Grandparent kind, or `None` near the root.
    pub grandparent: Option<&'a str>,
    /// Field name the *parent* occupies inside the grandparent.
    pub grandparent_field: Option<&'a str>,
}

/// Per-language classifier function pointer. Returning `None` falls
/// through to the recursive walk and lets a more specific child node
/// provide the highlight.
type Classifier = fn(&NodeCtx) -> Option<HighlightCategory>;

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
        Language::Bash => classify_bash,
        Language::Lua => classify_lua,
        Language::Ruby => classify_ruby,
        Language::Html => classify_html,
        Language::Css => classify_css,
        Language::Java => classify_java,
        Language::Swift => classify_swift,
        // C++ shares enough node kinds with C that the C classifier is
        // a reasonable base; C++-only constructs degrade gracefully.
        Language::Cpp => classify_c,
    }
}

#[allow(clippy::too_many_arguments)]
fn collect(
    node: &Node,
    parent_kind: Option<&str>,
    parent_field: Option<&str>,
    grandparent_kind: Option<&str>,
    grandparent_field: Option<&str>,
    byte_to_char: &[usize],
    classify: Classifier,
    out: &mut Vec<Highlight>,
) {
    let ctx = NodeCtx {
        kind: node.kind(),
        parent: parent_kind,
        field: parent_field,
        grandparent: grandparent_kind,
        grandparent_field,
    };
    if let Some(cat) = classify(&ctx) {
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
    // makes querying `cursor.field_name()` mid-iteration awkward. The child's
    // grandparent context is *this* node's parent context.
    let this_kind = node.kind();
    for i in 0..node.child_count() {
        let Some(child) = node.child(i) else {
            continue;
        };
        let field = node.field_name_for_child(i as u32);
        collect(
            &child,
            Some(this_kind),
            field,
            parent_kind,
            parent_field,
            byte_to_char,
            classify,
            out,
        );
    }
}

/// tree-sitter-rust. Anonymous keyword nodes carry the keyword text itself
/// as their `kind()`, which is what powers the long keyword match arm.
/// Specific context-sensitive cases (function names, macro invocations,
/// field access, lifetimes) check `parent_kind` / `field` first.
fn classify_rust(ctx: &NodeCtx) -> Option<HighlightCategory> {
    let NodeCtx {
        kind,
        parent,
        field,
        ..
    } = *ctx;
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

/// tree-sitter-typescript (also serves TSX). Identifiers that name or
/// call a function get the Function colour; every other value identifier
/// (locals, params, property reads) gets Variable so the file reads like
/// VSCode's instead of falling through to plain text.
fn classify_typescript(ctx: &NodeCtx) -> Option<HighlightCategory> {
    let NodeCtx {
        kind,
        parent,
        field,
        grandparent,
        grandparent_field,
    } = *ctx;
    use HighlightCategory::*;
    if kind == "identifier"
        || kind == "property_identifier"
        || kind == "shorthand_property_identifier"
    {
        match (parent, field) {
            (Some("function_declaration"), Some("name"))
            | (Some("function_signature"), Some("name"))
            | (Some("function_expression"), Some("name"))
            | (Some("generator_function_declaration"), Some("name"))
            | (Some("method_definition"), Some("name"))
            | (Some("method_signature"), Some("name")) => return Some(Function),
            // Direct call: `foo(...)`.
            (Some("call_expression"), Some("function")) => return Some(Function),
            // Method call: `obj.method(...)`. The name is the `property`
            // of a `member_expression` that is itself the callee
            // (`function` field of the enclosing `call_expression`).
            (Some("member_expression"), Some("property"))
                if grandparent == Some("call_expression")
                    && grandparent_field == Some("function") =>
            {
                return Some(Function)
            }
            _ => {}
        }
        // Any other identifier names a value: a local, a parameter, or a
        // property read.
        return Some(Variable);
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

/// tree-sitter-javascript — same context-sensitive function/variable
/// rules as TS minus the TS-only keywords.
fn classify_javascript(ctx: &NodeCtx) -> Option<HighlightCategory> {
    let NodeCtx {
        kind,
        parent,
        field,
        grandparent,
        grandparent_field,
    } = *ctx;
    use HighlightCategory::*;
    if kind == "identifier"
        || kind == "property_identifier"
        || kind == "shorthand_property_identifier"
    {
        match (parent, field) {
            (Some("function_declaration"), Some("name"))
            | (Some("function_expression"), Some("name"))
            | (Some("generator_function_declaration"), Some("name"))
            | (Some("method_definition"), Some("name")) => return Some(Function),
            (Some("call_expression"), Some("function")) => return Some(Function),
            (Some("member_expression"), Some("property"))
                if grandparent == Some("call_expression")
                    && grandparent_field == Some("function") =>
            {
                return Some(Function)
            }
            _ => {}
        }
        return Some(Variable);
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
fn classify_json(ctx: &NodeCtx) -> Option<HighlightCategory> {
    let kind = ctx.kind;
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
fn classify_python(ctx: &NodeCtx) -> Option<HighlightCategory> {
    let NodeCtx {
        kind,
        parent,
        field,
        ..
    } = *ctx;
    use HighlightCategory::*;
    if kind == "identifier" {
        match (parent, field) {
            (Some("function_definition"), Some("name")) => return Some(Function),
            (Some("class_definition"), Some("name")) => return Some(Type),
            (Some("call"), Some("function")) => return Some(Function),
            (Some("decorator"), _) => return Some(Function),
            _ => {}
        }
    }
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
fn classify_go(ctx: &NodeCtx) -> Option<HighlightCategory> {
    let NodeCtx {
        kind,
        parent,
        field,
        ..
    } = *ctx;
    use HighlightCategory::*;
    if matches!(kind, "identifier" | "field_identifier") {
        match (parent, field) {
            (Some("function_declaration"), Some("name")) => return Some(Function),
            (Some("method_declaration"), Some("name")) => return Some(Function),
            (Some("call_expression"), Some("function")) => return Some(Function),
            _ => {}
        }
    }
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
fn classify_c(ctx: &NodeCtx) -> Option<HighlightCategory> {
    let NodeCtx {
        kind,
        parent,
        field,
        ..
    } = *ctx;
    use HighlightCategory::*;
    if kind == "identifier" {
        match (parent, field) {
            (Some("call_expression"), Some("function")) => return Some(Function),
            (Some("function_declarator"), Some("declarator")) => return Some(Function),
            _ => {}
        }
    }
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
fn classify_toml(ctx: &NodeCtx) -> Option<HighlightCategory> {
    let kind = ctx.kind;
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
fn classify_yaml(ctx: &NodeCtx) -> Option<HighlightCategory> {
    let kind = ctx.kind;
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

/// tree-sitter-dart. Covers Flutter idioms — annotations like
/// `@override`, function calls (`obj.method()`), constructor invocations
/// — on top of the obvious comments / strings / numbers / type names.
fn classify_dart(ctx: &NodeCtx) -> Option<HighlightCategory> {
    let NodeCtx {
        kind,
        parent,
        field,
        ..
    } = *ctx;
    use HighlightCategory::*;
    if kind == "identifier" {
        match (parent, field) {
            (Some("function_signature"), Some("name"))
            | (Some("method_signature"), Some("name"))
            | (Some("getter_signature"), Some("name"))
            | (Some("setter_signature"), Some("name"))
            | (Some("constructor_signature"), Some("name"))
            | (Some("factory_constructor_signature"), Some("name")) => return Some(Function),
            (Some("class_definition"), Some("name"))
            | (Some("mixin_declaration"), Some("name"))
            | (Some("enum_declaration"), Some("name"))
            | (Some("type_alias"), Some("name"))
            | (Some("extension_declaration"), Some("name")) => return Some(Type),
            // `@override`, `@deprecated`, `@JsonSerializable()` etc.
            // The identifier sits under `marker_annotation` / `annotation`
            // — paint it as Keyword so the `@` stands out.
            (Some("marker_annotation"), _) | (Some("annotation"), _) => return Some(Keyword),
            // The method / function name in a call site (`widget.build()`).
            (Some("selector"), _)
            | (Some("function_expression_invocation"), Some("function"))
            | (Some("conditional_assignable_selector"), _) => return Some(Function),
            _ => {}
        }
        // Any other identifier names a value — local, parameter, field
        // access. Light-blue, matching VSCode's Dart highlighting.
        return Some(Variable);
    }
    match kind {
        "comment" | "documentation_comment" | "line_comment" | "block_comment" => Some(Comment),
        "string_literal" | "raw_string_literal" | "template_string_literal" => Some(StringLit),
        "decimal_integer_literal"
        | "hex_integer_literal"
        | "decimal_floating_point_literal"
        | "true"
        | "false" => Some(Number),
        "type_identifier" | "primitive_type" | "void_type" => Some(Type),
        // Annotations themselves (the `@` + name unit) read as Keyword
        // so they don't accidentally collide with function-call colours
        // when the inner identifier path above misses.
        "marker_annotation" | "annotation" => Some(Keyword),
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

/// tree-sitter-html. Tag names → Type (they read as "things you're
/// instantiating"), attribute names → Function (callable / namespacey),
/// attribute values and text content → StringLit, doctype + entities
/// → Keyword.
fn classify_html(ctx: &NodeCtx) -> Option<HighlightCategory> {
    let kind = ctx.kind;
    use HighlightCategory::*;
    match kind {
        "comment" => Some(Comment),
        "tag_name" => Some(Type),
        "attribute_name" => Some(Function),
        "attribute_value" | "quoted_attribute_value" => Some(StringLit),
        "doctype" => Some(Keyword),
        "erroneous_end_tag_name" => Some(Type),
        "entity" => Some(Keyword),
        _ => None,
    }
}

/// tree-sitter-css. Selectors → Type / Function depending on whether
/// they're tag, class or id; property names → Function; values
/// (strings, colours, units) keep their nature.
fn classify_css(ctx: &NodeCtx) -> Option<HighlightCategory> {
    let kind = ctx.kind;
    use HighlightCategory::*;
    match kind {
        "comment" => Some(Comment),
        // Selectors
        "tag_name" | "nesting_selector" => Some(Type),
        "class_name" | "id_name" | "property_name" => Some(Function),
        "pseudo_class_selector" | "pseudo_element_selector" => Some(Keyword),
        "attribute_name" | "attribute_selector" => Some(Function),
        // Literals
        "string_value" | "url" => Some(StringLit),
        "color_value" | "color" => Some(StringLit),
        "integer_value" | "float_value" | "unit" => Some(Number),
        // At-rules (`@media`, `@import`, …) and important.
        "at_keyword" | "important" | "from" | "to" => Some(Keyword),
        _ => None,
    }
}

/// tree-sitter-java. Flutter's Android side. Covers the common
/// surface: type names, method declarations, annotations, modifiers,
/// strings, numbers, comments.
fn classify_java(ctx: &NodeCtx) -> Option<HighlightCategory> {
    let NodeCtx {
        kind,
        parent,
        field,
        ..
    } = *ctx;
    use HighlightCategory::*;
    if kind == "identifier" {
        match (parent, field) {
            (Some("method_declaration"), Some("name"))
            | (Some("method_invocation"), Some("name")) => return Some(Function),
            _ => {}
        }
    }
    match kind {
        "line_comment" | "block_comment" => Some(Comment),
        "string_literal" | "character_literal" => Some(StringLit),
        "decimal_integer_literal"
        | "hex_integer_literal"
        | "decimal_floating_point_literal"
        | "true"
        | "false"
        | "null_literal" => Some(Number),
        "type_identifier"
        | "boolean_type"
        | "integral_type"
        | "floating_point_type"
        | "void_type" => Some(Type),
        "marker_annotation" | "annotation" => Some(Keyword),
        "public" | "private" | "protected" | "static" | "final" | "abstract" | "class"
        | "interface" | "enum" | "extends" | "implements" | "import" | "package" | "new"
        | "return" | "if" | "else" | "for" | "while" | "do" | "switch" | "case" | "default"
        | "break" | "continue" | "try" | "catch" | "finally" | "throw" | "throws" | "this"
        | "super" | "synchronized" | "volatile" | "transient" | "native" | "instanceof"
        | "void" => Some(Keyword),
        _ => None,
    }
}

/// tree-sitter-swift. Flutter's iOS side. Type names, function /
/// method declarations and calls, attributes, strings, numbers,
/// comments, keywords.
fn classify_swift(ctx: &NodeCtx) -> Option<HighlightCategory> {
    let NodeCtx { kind, parent, .. } = *ctx;
    use HighlightCategory::*;
    if kind == "simple_identifier" {
        match parent {
            Some("function_declaration") => return Some(Function),
            Some("call_expression") => return Some(Function),
            _ => {}
        }
    }
    match kind {
        "comment" | "multiline_comment" => Some(Comment),
        "line_str_text" | "str_escaped_char" | "raw_str_part" => Some(StringLit),
        "integer_literal" | "real_literal" | "hex_literal" | "oct_literal" | "bin_literal"
        | "boolean_literal" => Some(Number),
        "type_identifier" | "user_type" => Some(Type),
        "attribute" => Some(Keyword),
        "func" | "let" | "var" | "class" | "struct" | "enum" | "protocol" | "extension"
        | "import" | "return" | "if" | "else" | "guard" | "for" | "in" | "while" | "repeat"
        | "switch" | "case" | "default" | "break" | "continue" | "fallthrough" | "do" | "throw"
        | "throws" | "try" | "catch" | "defer" | "self" | "super" | "init" | "deinit"
        | "static" | "public" | "private" | "internal" | "fileprivate" | "open" | "final"
        | "lazy" | "weak" | "override" | "mutating" | "nil" | "true" | "false" | "async"
        | "await" => Some(Keyword),
        _ => None,
    }
}

/// tree-sitter-md (CommonMark). Block-structure grammar — we surface the
/// leaf markers and code spans, treat headings as keywords.
fn classify_markdown(ctx: &NodeCtx) -> Option<HighlightCategory> {
    let kind = ctx.kind;
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

/// tree-sitter-bash.
fn classify_bash(ctx: &NodeCtx) -> Option<HighlightCategory> {
    let kind = ctx.kind;
    use HighlightCategory::*;
    match kind {
        "comment" => Some(Comment),
        "string" | "raw_string" | "ansi_c_string" | "heredoc_body" => Some(StringLit),
        "number" => Some(Number),
        "variable_name" | "simple_expansion" => Some(Type),
        "function_definition" | "command_name" => Some(Function),
        "if" | "then" | "elif" | "else" | "fi" | "for" | "while" | "until" | "do" | "done"
        | "case" | "esac" | "in" | "function" | "return" | "break" | "continue" | "local"
        | "readonly" | "declare" | "export" | "unset" | "true" | "false" => Some(Keyword),
        _ => None,
    }
}

/// tree-sitter-lua.
fn classify_lua(ctx: &NodeCtx) -> Option<HighlightCategory> {
    let kind = ctx.kind;
    use HighlightCategory::*;
    match kind {
        "comment" => Some(Comment),
        "string" => Some(StringLit),
        "number" => Some(Number),
        "and" | "break" | "do" | "else" | "elseif" | "end" | "false" | "for" | "function"
        | "goto" | "if" | "in" | "local" | "nil" | "not" | "or" | "repeat" | "return" | "then"
        | "true" | "until" | "while" => Some(Keyword),
        _ => None,
    }
}

/// tree-sitter-ruby.
fn classify_ruby(ctx: &NodeCtx) -> Option<HighlightCategory> {
    let kind = ctx.kind;
    use HighlightCategory::*;
    match kind {
        "comment" => Some(Comment),
        "string" | "string_content" | "string_array" | "symbol_array" | "heredoc_body" => {
            Some(StringLit)
        }
        "integer" | "float" => Some(Number),
        "constant" => Some(Type),
        "simple_symbol" | "delimited_symbol" => Some(Function),
        "def" | "class" | "module" | "if" | "elsif" | "else" | "unless" | "while" | "until"
        | "for" | "in" | "do" | "end" | "begin" | "rescue" | "ensure" | "raise" | "return"
        | "break" | "next" | "redo" | "retry" | "yield" | "true" | "false" | "nil" | "self"
        | "and" | "or" | "not" | "case" | "when" | "then" | "require" | "require_relative"
        | "lambda" | "proc" => Some(Keyword),
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
    fn dart_highlights_annotation_as_keyword() {
        // `@override` should read as a keyword, not blend with plain
        // identifiers. The annotation form covers `@deprecated`,
        // `@JsonSerializable()`, etc. too.
        let src = "class C {\n  @override\n  String foo() => '';\n}\n";
        let cats = categories_with(Language::Dart, src);
        assert!(cats.contains(&HighlightCategory::Keyword));
    }

    #[test]
    fn html_highlights_tags_and_attributes() {
        let src = "<div class=\"x\">hi</div>\n";
        let cats = categories_with(Language::Html, src);
        assert!(cats.contains(&HighlightCategory::Type)); // tag_name
        assert!(cats.contains(&HighlightCategory::Function)); // attribute_name
        assert!(cats.contains(&HighlightCategory::StringLit)); // attribute_value
    }

    #[test]
    fn css_highlights_selectors_and_values() {
        let src = ".btn { color: #fff; padding: 8px; }\n";
        let cats = categories_with(Language::Css, src);
        assert!(cats.contains(&HighlightCategory::Function)); // class_name + property_name
        assert!(cats.contains(&HighlightCategory::Number)); // 8 + px
    }

    #[test]
    fn html_extension_routes_to_html() {
        assert_eq!(
            Language::for_path(Path::new("index.html")),
            Some(Language::Html),
        );
        assert_eq!(
            Language::for_path(Path::new("page.htm")),
            Some(Language::Html),
        );
    }

    #[test]
    fn css_extension_routes_to_css() {
        assert_eq!(
            Language::for_path(Path::new("main.css")),
            Some(Language::Css),
        );
    }

    #[test]
    fn java_highlights_class_and_string() {
        let src = "public class Foo {\n  String s = \"hi\";\n}\n";
        let cats = categories_with(Language::Java, src);
        assert!(cats.contains(&HighlightCategory::Keyword)); // public / class
        assert!(cats.contains(&HighlightCategory::StringLit));
    }

    #[test]
    fn swift_highlights_func_and_string() {
        let src = "func greet() {\n  let s = \"hi\"\n}\n";
        let cats = categories_with(Language::Swift, src);
        assert!(cats.contains(&HighlightCategory::Keyword)); // func / let
    }

    #[test]
    fn cpp_highlights_via_c_classifier() {
        let src = "int main() {\n  // hi\n  return 0;\n}\n";
        let cats = categories_with(Language::Cpp, src);
        assert!(cats.contains(&HighlightCategory::Comment));
    }

    #[test]
    fn native_extensions_route_correctly() {
        assert_eq!(
            Language::for_path(Path::new("A.java")),
            Some(Language::Java)
        );
        assert_eq!(
            Language::for_path(Path::new("App.swift")),
            Some(Language::Swift),
        );
        assert_eq!(Language::for_path(Path::new("x.cpp")), Some(Language::Cpp));
        assert_eq!(Language::for_path(Path::new("x.hpp")), Some(Language::Cpp));
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
    fn python_highlights_function_definition_name() {
        let src = "def hello():\n    pass\n";
        let hs = highlights_of(Language::Python, src);
        assert!(hs.iter().any(|h| h.category == HighlightCategory::Function));
    }

    #[test]
    fn go_highlights_function_declaration_name() {
        let src = "package main\nfunc hello() {}\n";
        let hs = highlights_of(Language::Go, src);
        assert!(hs.iter().any(|h| h.category == HighlightCategory::Function));
    }

    #[test]
    fn c_highlights_call_function() {
        let src = "int main() { printf(\"hi\"); return 0; }";
        let hs = highlights_of(Language::C, src);
        assert!(hs.iter().any(|h| h.category == HighlightCategory::Function));
    }

    #[test]
    fn bash_highlights_keywords_and_string() {
        let src = "if [ -f x ]; then\n  echo \"hi\"\nfi\n";
        let hs = highlights_of(Language::Bash, src);
        assert!(hs.iter().any(|h| h.category == HighlightCategory::Keyword));
        assert!(hs
            .iter()
            .any(|h| h.category == HighlightCategory::StringLit));
    }

    #[test]
    fn lua_highlights_function_and_string() {
        let src = "local function greet(n)\n  return \"hi \" .. n\nend\n";
        let hs = highlights_of(Language::Lua, src);
        assert!(hs.iter().any(|h| h.category == HighlightCategory::Keyword));
        assert!(hs
            .iter()
            .any(|h| h.category == HighlightCategory::StringLit));
    }

    #[test]
    fn ruby_highlights_def_and_string() {
        let src = "def greet(name)\n  puts \"hi #{name}\"\nend\n";
        let hs = highlights_of(Language::Ruby, src);
        assert!(hs.iter().any(|h| h.category == HighlightCategory::Keyword));
        assert!(hs
            .iter()
            .any(|h| h.category == HighlightCategory::StringLit));
    }

    #[test]
    fn typescript_highlights_function_declaration_name() {
        let src = "function greet() { return 1; }";
        let hs = highlights_of(Language::TypeScript, src);
        assert!(hs.iter().any(|h| h.category == HighlightCategory::Function));
    }

    /// Return the source slice for every highlight of `category`.
    fn spans_of(lang: Language, src: &str, category: HighlightCategory) -> Vec<String> {
        let chars: Vec<char> = src.chars().collect();
        highlights_of(lang, src)
            .into_iter()
            .filter(|h| h.category == category)
            .map(|h| chars[h.range].iter().collect())
            .collect()
    }

    #[test]
    fn js_method_call_is_a_function() {
        // `http.createServer` — the method name is a property of a
        // member_expression that is the callee. It must read as Function,
        // not Variable, while the receiver `http` stays a Variable.
        let src = "const s = http.createServer();";
        let funcs = spans_of(Language::JavaScript, src, HighlightCategory::Function);
        assert!(
            funcs.contains(&"createServer".to_string()),
            "method name should be Function, got {funcs:?}"
        );
        let vars = spans_of(Language::JavaScript, src, HighlightCategory::Variable);
        assert!(vars.contains(&"http".to_string()), "got {vars:?}");
        assert!(vars.contains(&"s".to_string()), "got {vars:?}");
        // The method name must NOT also be a Variable.
        assert!(!vars.contains(&"createServer".to_string()));
    }

    #[test]
    fn js_property_read_is_a_variable_not_a_function() {
        // `res.statusCode` (no call) — the property reads as Variable.
        let src = "res.statusCode = 200;";
        let vars = spans_of(Language::JavaScript, src, HighlightCategory::Variable);
        assert!(vars.contains(&"statusCode".to_string()), "got {vars:?}");
        let funcs = spans_of(Language::JavaScript, src, HighlightCategory::Function);
        assert!(!funcs.contains(&"statusCode".to_string()), "got {funcs:?}");
    }

    #[test]
    fn ts_direct_call_and_locals() {
        let src = "const x = require('m');";
        let funcs = spans_of(Language::TypeScript, src, HighlightCategory::Function);
        assert!(funcs.contains(&"require".to_string()), "got {funcs:?}");
        let vars = spans_of(Language::TypeScript, src, HighlightCategory::Variable);
        assert!(vars.contains(&"x".to_string()), "got {vars:?}");
    }

    #[test]
    fn dart_identifier_falls_through_to_variable() {
        let src = "var count = items;";
        let vars = spans_of(Language::Dart, src, HighlightCategory::Variable);
        assert!(vars.contains(&"count".to_string()), "got {vars:?}");
        assert!(vars.contains(&"items".to_string()), "got {vars:?}");
    }

    // ── incremental parsing ───────────────────────────────────────────────

    #[test]
    fn first_highlight_call_caches_a_tree() {
        let mut h = Highlighter::new(Language::Rust).unwrap();
        assert!(h.old_tree.is_none(), "fresh highlighter has no cached tree");
        h.highlight("let x = 1;");
        assert!(h.old_tree.is_some(), "highlight caches the parse tree");
    }

    #[test]
    fn reset_drops_the_cached_tree() {
        let mut h = Highlighter::new(Language::Rust).unwrap();
        h.highlight("let x = 1;");
        h.reset();
        assert!(h.old_tree.is_none());
    }

    #[test]
    fn empty_text_clears_the_cached_tree() {
        let mut h = Highlighter::new(Language::Rust).unwrap();
        h.highlight("let x = 1;");
        assert!(h.old_tree.is_some());
        h.highlight("");
        assert!(h.old_tree.is_none(), "empty input drops the stale tree");
    }

    #[test]
    fn apply_edit_without_cached_tree_is_a_noop() {
        let mut h = Highlighter::new(Language::Rust).unwrap();
        // Construct a delta by hand — it should not panic with no cached tree.
        let delta = BufferDelta {
            start_byte: 0,
            old_end_byte: 0,
            new_end_byte: 1,
            start_point: editor_buffer::BytePoint { row: 0, column: 0 },
            old_end_point: editor_buffer::BytePoint { row: 0, column: 0 },
            new_end_point: editor_buffer::BytePoint { row: 0, column: 1 },
        };
        h.apply_edit(&delta);
        assert!(h.old_tree.is_none());
    }

    #[test]
    fn incremental_reparse_produces_same_highlights_as_full_reparse() {
        // Start with one buffer, parse it, apply an edit + reparse;
        // compare with a fresh highlighter that parses the post-edit text
        // from scratch.
        let before = "fn a() { let x = 1; }";
        let after = "fn a() { let xx = 1; }"; // insert one char "x" at byte 14
        let mut incremental = Highlighter::new(Language::Rust).unwrap();
        incremental.highlight(before);
        let delta = BufferDelta {
            start_byte: 14,
            old_end_byte: 14,
            new_end_byte: 15,
            start_point: editor_buffer::BytePoint { row: 0, column: 14 },
            old_end_point: editor_buffer::BytePoint { row: 0, column: 14 },
            new_end_point: editor_buffer::BytePoint { row: 0, column: 15 },
        };
        incremental.apply_edit(&delta);
        let incremental_hs = incremental.highlight(after);

        let mut fresh = Highlighter::new(Language::Rust).unwrap();
        let fresh_hs = fresh.highlight(after);

        assert_eq!(incremental_hs, fresh_hs);
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
