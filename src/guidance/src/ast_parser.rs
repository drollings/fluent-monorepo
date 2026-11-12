use std::path::Path;

use fluent_types::{FileType, GuidanceDoc, Member, MemberType, Meta, Param, Span};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ParseError {
    #[error("failed to read file: {0}")]
    Io(#[from] common_core::error::IoError),
    #[error("failed to parse source: {0}")]
    Syntax(String),
    #[error("unsupported language for file: {0}")]
    UnsupportedLanguage(String),
}

common_core::impl_from_io_error!(ParseError);

/// One comment scope: the contiguous comment text attached to a node.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One comment scope: the contiguous comment text attached to a node.
pub struct CommentBlock {
    /// Stable identity (`scope::name`, or `$file` for the module comment).
    pub node_key: String,
    /// Grammar node kind of the documented node (`module` for top-of-file).
    pub node_kind: String,
    /// Declared name (`$file` for the module comment).
    pub name: String,
    /// Lexical scope breadcrumb (`Widget` for methods).
    pub scope: String,
    /// Byte span from the first comment line through the node end.
    pub start_byte: usize,
    /// Byte span from the first comment line through the node end.
    pub end_byte: usize,
    /// 1-based first comment line.
    pub start_line: u32,
    /// 1-based node end line.
    pub end_line: u32,
    /// Contiguous comment text in source order, prefixes stripped.
    pub text: String,
    /// Blake3 of the normalized text; the summary cache key input.
    pub comment_hash: String,
}

/// Python docstrings (first string in the body) join the `#` comments.
pub const COLLECT_PYTHON_DOCSTRINGS: bool = true;

/// Strip one comment's prefixes (`///`, `//!`, `//`, `#`, `/*`, `*`).
#[must_use]
/// Strip one comment's prefixes (`///`, `//!`, `//`, `#`, `/*`, `*`).
pub fn clean_comment_block_text(text: &str) -> String {
    let mut out = text.to_string();
    if let Some(stripped) = out.strip_prefix("/**").or_else(|| out.strip_prefix("/*")) {
        out = stripped.to_string();
    }
    if let Some(stripped) = out.strip_suffix("*/") {
        out = stripped.to_string();
    }
    let mut lines = Vec::new();
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("//!") {
            lines.push(rest.strip_prefix(' ').unwrap_or(rest));
        } else if let Some(rest) = line.strip_prefix("///") {
            lines.push(rest.strip_prefix(' ').unwrap_or(rest));
        } else if let Some(rest) = line.strip_prefix("//") {
            let rest = rest.strip_prefix('/').unwrap_or(rest);
            lines.push(rest.strip_prefix(' ').unwrap_or(rest));
        } else if let Some(rest) = line.strip_prefix('#') {
            lines.push(rest.strip_prefix(' ').unwrap_or(rest));
        } else {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix('*') {
                lines.push(rest.strip_prefix(' ').unwrap_or(rest));
            } else {
                lines.push(line);
            }
        }
    }
    lines.join("\n").trim().to_string()
}

/// Whether a sibling kind carries comment text.
fn is_comment_kind(kind: &str) -> bool {
    matches!(
        kind,
        "comment" | "doc_comment" | "line_comment" | "block_comment" | "documentation_comment"
    )
}

/// Whether a sibling kind is an attribute that comments can span across.
fn is_attribute_kind(kind: &str) -> bool {
    matches!(
        kind,
        "attribute_item" | "outer_attribute" | "inner_attribute" | "attribute"
    ) || kind.contains("attribute")
}

/// Contiguous cleaned comment lines above `node`, oldest first, with spans.
fn preceding_doc_parts(node: &tree_sitter::Node, source: &str) -> Vec<(String, usize, u32)> {
    let bytes = source.as_bytes();
    let mut parts = Vec::new();
    let mut current = node.prev_sibling();
    while let Some(sibling) = current {
        let kind = sibling.kind();
        if is_comment_kind(kind) {
            if let Ok(text) = sibling.utf8_text(bytes) {
                let cleaned = clean_comment_block_text(text);
                parts.push((
                    cleaned,
                    sibling.start_byte(),
                    sibling.start_position().row as u32,
                ));
            }
            current = sibling.prev_sibling();
        } else if is_attribute_kind(kind) {
            current = sibling.prev_sibling();
        } else {
            break;
        }
    }
    parts.reverse();
    parts
}

/// Contiguous doc text above `node`, walking across attributes.
#[must_use]
/// Contiguous doc text above `node`, walking across attributes.
pub fn preceding_doc_text(node: &tree_sitter::Node, source: &str) -> Option<String> {
    let lines: Vec<String> = preceding_doc_parts(node, source)
        .into_iter()
        .map(|(text, _, _)| text)
        .collect();
    let joined = lines.join("\n").trim().to_string();
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

/// Leading comment lines at the top of the file, in source order.
fn module_comment_parts(root: &tree_sitter::Node, source: &str) -> Vec<(String, usize, u32)> {
    let bytes = source.as_bytes();
    let mut parts = Vec::new();
    let mut cursor = root.walk();
    for node in root.children(&mut cursor) {
        let kind = node.kind();
        if is_comment_kind(kind) {
            if let Ok(text) = node.utf8_text(bytes) {
                let cleaned = clean_comment_block_text(text);
                parts.push((cleaned, node.start_byte(), node.start_position().row as u32));
            }
        } else if !is_attribute_kind(kind) {
            break;
        }
    }
    parts
}

/// Leading file comment, if any.
fn module_comment_text(root: &tree_sitter::Node, source: &str) -> Option<String> {
    let lines: Vec<String> = module_comment_parts(root, source)
        .into_iter()
        .map(|(text, _, _)| text)
        .collect();
    let joined = lines.join("\n").trim().to_string();
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

/// First string literal in a Python body (the docstring), unquoted.
fn python_docstring(node: &tree_sitter::Node, source: &str) -> Option<String> {
    if !COLLECT_PYTHON_DOCSTRINGS {
        return None;
    }
    let body = node.child_by_field_name("body")?;
    let bytes = source.as_bytes();
    let mut cursor = body.walk();
    let first = body
        .children(&mut cursor)
        .find(tree_sitter::Node::is_named)?;
    let mut stack = vec![first];
    while let Some(view) = stack.pop() {
        let kind = view.kind();
        if kind.contains("string") && !kind.contains("statement") && !kind.contains("expression") {
            if let Ok(text) = view.utf8_text(bytes) {
                let doc = unquote_python_string(text);
                if !doc.is_empty() {
                    return Some(doc);
                }
            }
            return None;
        }
        if kind == "expression_statement" {
            let mut inner = view.walk();
            let mut children: Vec<tree_sitter::Node> = view.children(&mut inner).collect();
            children.reverse();
            stack.extend(children);
            continue;
        }
        return None;
    }
    None
}

/// Strip one layer of Python string quotes, then trim.
fn unquote_python_string(text: &str) -> String {
    let trimmed = text.trim();
    for quote in ["\"\"\"", "'''"] {
        if trimmed.len() >= 6 && trimmed.starts_with(quote) && trimmed.ends_with(quote) {
            return trimmed[quote.len()..trimmed.len() - quote.len()]
                .trim()
                .to_string();
        }
    }
    if trimmed.len() >= 2 {
        let bytes = trimmed.as_bytes();
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return trimmed[1..trimmed.len() - 1].trim().to_string();
        }
    }
    trimmed.to_string()
}

/// Scope breadcrumb entries for documentable containers.
fn scope_name_for(node: &tree_sitter::Node, source: &str) -> Option<String> {
    if node.kind() == "impl_item" {
        return node
            .child_by_field_name("type")
            .and_then(|name| name.utf8_text(source.as_bytes()).ok())
            .map(str::trim_matching_trailing_generics)
            .map(str::to_string)
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());
    }
    node.child_by_field_name("name")
        .and_then(|name| name.utf8_text(source.as_bytes()).ok())
        .map(str::to_string)
}

/// Whether `kind` opens a scope whose children inherit its name.
fn is_scope_kind(kind: &str) -> bool {
    matches!(
        kind,
        "impl_item"
            | "mod_item"
            | "trait_item"
            | "class_definition"
            | "decorated_definition"
            | "struct_item"
            | "enum_item"
    )
}

/// Human-readable name for an impl block's type node.
trait TrimGenerics {
    /// Cut a trailing `<...>` generic list for scope breadcrumbs.
    fn trim_matching_trailing_generics(&self) -> &str;
}

impl TrimGenerics for str {
    fn trim_matching_trailing_generics(&self) -> &str {
        match self.find('<') {
            Some(index) => self[..index].trim_end(),
            None => self,
        }
    }
}

/// Walk one tree collecting documentable comment scopes.
pub(crate) fn collect_comment_blocks(
    root: &tree_sitter::Node,
    source: &str,
    format: &str,
) -> Vec<CommentBlock> {
    let mut blocks = Vec::new();
    let bytes = source.as_bytes();
    if let Some(text) = module_comment_text(root, source) {
        blocks.push(CommentBlock {
            node_key: "$file".to_string(),
            node_kind: "module".to_string(),
            name: "$file".to_string(),
            scope: String::new(),
            start_byte: 0,
            end_byte: root.end_byte(),
            start_line: 1,
            end_line: root.end_position().row as u32 + 1,
            comment_hash: common_core::hash::blake3_hex(text.as_bytes()),
            text,
        });
    }
    let mut breadcrumb: Vec<String> = Vec::new();
    walk_comment_nodes(root, source, format, bytes, &mut breadcrumb, &mut blocks);
    blocks
}

/// Depth-first comment walk sharing one tree and its source bytes.
fn walk_comment_nodes(
    node: &tree_sitter::Node,
    source: &str,
    format: &str,
    bytes: &[u8],
    breadcrumb: &mut Vec<String>,
    out: &mut Vec<CommentBlock>,
) {
    let mut cursor = node.walk();
    let children: Vec<tree_sitter::Node> = node.children(&mut cursor).collect();
    for child in children {
        if !child.is_named() {
            continue;
        }
        let kind = child.kind();
        if is_comment_kind(kind) || is_attribute_kind(kind) {
            continue;
        }
        if is_scope_kind(kind) {
            if kind == "impl_item" {
                if let Some(name) = scope_name_for(&child, source) {
                    breadcrumb.push(name);
                    walk_comment_nodes(&child, source, format, bytes, breadcrumb, out);
                    breadcrumb.pop();
                    continue;
                }
            } else if let Some(name) = scope_name_for(&child, source) {
                let doc = preceding_doc_text(&child, source);
                if let Some(text) = doc {
                    push_comment_block(&child, source, bytes, kind, &text, breadcrumb, out);
                }
                breadcrumb.push(name);
                walk_comment_nodes(&child, source, format, bytes, breadcrumb, out);
                breadcrumb.pop();
                continue;
            }
        }
        let name = child
            .child_by_field_name("name")
            .and_then(|name| name.utf8_text(bytes).ok())
            .map(str::to_string);
        if name.is_some() {
            let mut text = preceding_doc_text(&child, source);
            if format == "python" {
                let docstring = python_docstring(&child, source);
                text = match (text, docstring) {
                    (Some(comments), Some(doc)) => Some(format!("{comments}\n{doc}")),
                    (Some(comments), None) => Some(comments),
                    (None, Some(doc)) => Some(doc),
                    (None, None) => None,
                };
            }
            if let Some(text) = text {
                push_comment_block(&child, source, bytes, kind, &text, breadcrumb, out);
            }
        }
        walk_comment_nodes(&child, source, format, bytes, breadcrumb, out);
    }
}

/// Push one block spanning from its first comment line to the node end.
fn push_comment_block(
    node: &tree_sitter::Node,
    source: &str,
    bytes: &[u8],
    kind: &str,
    text: &str,
    breadcrumb: &[String],
    out: &mut Vec<CommentBlock>,
) {
    let _ = bytes;
    let parts = preceding_doc_parts(node, source);
    let scope = breadcrumb.join("::");
    let name = scope_name_for(node, source).unwrap_or_default();
    if name.is_empty() {
        return;
    }
    let node_key = if scope.is_empty() {
        name.clone()
    } else {
        format!("{scope}::{name}")
    };
    let start_byte = parts.first().map_or(node.start_byte(), |part| part.1);
    let start_line = parts
        .first()
        .map_or(node.start_position().row as u32 + 1, |part| part.2 + 1);
    out.push(CommentBlock {
        node_key,
        node_kind: kind.to_string(),
        name,
        scope,
        start_byte,
        end_byte: node.end_byte(),
        start_line,
        end_line: node.end_position().row as u32 + 1,
        comment_hash: common_core::hash::blake3_hex(text.as_bytes()),
        text: text.to_string(),
    });
}

pub struct AstParser {
    zig: tree_sitter::Parser,
    python: tree_sitter::Parser,
    rust: tree_sitter::Parser,
    c: tree_sitter::Parser,
    cpp: tree_sitter::Parser,
    go: tree_sitter::Parser,
    java: tree_sitter::Parser,
    javascript: tree_sitter::Parser,
    typescript: tree_sitter::Parser,
    tsx: tree_sitter::Parser,
    jsx: tree_sitter::Parser,
}

impl Default for AstParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a parser for one grammar. Takes ownership because
/// `tree_sitter::Language` handles are moved into `set_language`.
#[allow(clippy::needless_pass_by_value)]
/// Build a parser for one grammar. Takes ownership because
/// `tree_sitter::Language` handles are moved into `set_language`.
fn parser_for(language: tree_sitter::Language) -> tree_sitter::Parser {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&language)
        .expect("tree-sitter language should be valid");
    parser
}

impl AstParser {
    pub fn new() -> Self {
        Self {
            zig: parser_for(tree_sitter_zig::LANGUAGE.into()),
            python: parser_for(tree_sitter_python::LANGUAGE.into()),
            rust: parser_for(tree_sitter_rust::LANGUAGE.into()),
            c: parser_for(tree_sitter_c::LANGUAGE.into()),
            cpp: parser_for(tree_sitter_cpp::LANGUAGE.into()),
            go: parser_for(tree_sitter_go::LANGUAGE.into()),
            java: parser_for(tree_sitter_java::LANGUAGE.into()),
            javascript: parser_for(tree_sitter_javascript::LANGUAGE.into()),
            typescript: parser_for(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
            tsx: parser_for(tree_sitter_typescript::LANGUAGE_TSX.into()),
            jsx: parser_for(tree_sitter_javascript::LANGUAGE.into()),
        }
    }

    /// Map file extension to language name.
    fn language_from_ext(ext: &str) -> Option<&'static str> {
        match ext {
            "zig" | "zon" => Some("zig"),
            "py" => Some("python"),
            "rs" => Some("rust"),
            _ => None,
        }
    }

    /// Parse source in the given extractor format (`rust`, `tsx`, …) into
    /// a concrete syntax tree (P2 fragment extraction; the legacy
    /// `parse_file` member pipeline is untouched).
    /// M5: single dispatch on `format`, not a fallback walk — exactly one
    /// parser applies and unknown is terminal. A ladder here would be one
    /// rung (obfuscation), and `&mut self` field selection does not fit
    /// `FnMut` rungs. Stays.
    pub fn parse_tree(
        &mut self,
        format: &str,
        source: &str,
    ) -> Result<tree_sitter::Tree, ParseError> {
        let parser = match format {
            "zig" => &mut self.zig,
            "rust" => &mut self.rust,
            "python" => &mut self.python,
            "c" => &mut self.c,
            "cpp" => &mut self.cpp,
            "go" => &mut self.go,
            "java" => &mut self.java,
            "javascript" => &mut self.javascript,
            "jsx" => &mut self.jsx,
            "typescript" => &mut self.typescript,
            "tsx" => &mut self.tsx,
            _ => return Err(ParseError::UnsupportedLanguage(format.to_string())),
        };
        parser
            .parse(source, None)
            .ok_or_else(|| ParseError::Syntax("tree-sitter returned no tree".into()))
    }

    /// Whether the extractor has a grammar for `format`.
    #[must_use]
    pub fn has_grammar(format: &str) -> bool {
        matches!(
            format,
            "zig"
                | "rust"
                | "python"
                | "c"
                | "cpp"
                | "go"
                | "java"
                | "javascript"
                | "jsx"
                | "typescript"
                | "tsx"
        )
    }

    /// Comment scopes for one file over a single parse.
    pub fn comment_blocks(
        &mut self,
        format: &str,
        source: &str,
    ) -> Result<Vec<CommentBlock>, ParseError> {
        let tree = self.parse_tree(format, source)?;
        Ok(collect_comment_blocks(&tree.root_node(), source, format))
    }

    /// Doc text for one named node, if the collector finds it.
    #[must_use]
    pub fn doc_comment_for_node(
        &mut self,
        format: &str,
        source: &str,
        node_name: &str,
    ) -> Option<String> {
        self.comment_blocks(format, source)
            .ok()?
            .into_iter()
            .find(|block| block.name == node_name)
            .map(|block| block.text)
    }

    pub fn parse_file(&mut self, path: &Path, source: &str) -> Result<GuidanceDoc, ParseError> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language = Self::language_from_ext(ext)
            .ok_or_else(|| ParseError::UnsupportedLanguage(path.display().to_string()))?;

        let tree = match language {
            "zig" => self
                .zig
                .parse(source, None)
                .ok_or_else(|| ParseError::Syntax("tree-sitter returned no tree".into()))?,
            "python" => self
                .python
                .parse(source, None)
                .ok_or_else(|| ParseError::Syntax("tree-sitter returned no tree".into()))?,
            "rust" => self
                .rust
                .parse(source, None)
                .ok_or_else(|| ParseError::Syntax("tree-sitter returned no tree".into()))?,
            _ => unreachable!(),
        };

        let source_path = path.to_string_lossy().to_string();
        let _module_name = source_path
            .strip_suffix(&format!(".{ext}"))
            .unwrap_or(&source_path)
            .replace(['/', '\\'], ".");

        let root = tree.root_node();
        let cursor = &mut tree.walk();

        let module_comment = extract_module_comment(&root, source);
        let members = extract_members(&root, source, cursor, language);

        let meta = Meta {
            module: source_path.as_str().into(),
            source: source_path.as_str().into(),
            language: language.into(),
        };

        Ok(GuidanceDoc {
            meta,
            comment: module_comment.map(|c| c.as_str().into()),
            members,
            ..GuidanceDoc::default()
        })
    }

    pub fn has_errors(&mut self, source: &str, language: &str) -> bool {
        let parser = match language {
            "zig" => &mut self.zig,
            "python" => &mut self.python,
            "rust" => &mut self.rust,
            _ => return true,
        };
        let Some(tree) = parser.parse(source, None) else {
            return true;
        };
        tree.root_node().has_error()
    }

    /// Names of top-level items carrying `#[cfg(...)]` attributes, with
    /// their 1-based lines. Rust-only; cfg-gated items are never
    /// claimed clean without a matrix cell that exercises them.
    pub(crate) fn rust_cfg_gated(&mut self, text: &str) -> Vec<(String, u32)> {
        let Ok(tree) = self.parse_tree("rust", text) else {
            return Vec::new();
        };
        let root = tree.root_node();
        let mut cursor = root.walk();
        let nodes: Vec<tree_sitter::Node> = root
            .children(&mut cursor)
            .filter(tree_sitter::Node::is_named)
            .collect();
        let mut out = Vec::new();
        let mut pending_cfg = false;
        for node in nodes {
            if node.kind() == "attribute_item" {
                if node
                    .utf8_text(text.as_bytes())
                    .is_ok_and(|attr| attr.starts_with("#[cfg"))
                {
                    pending_cfg = true;
                }
                continue;
            }
            if pending_cfg || has_cfg_attr(&node, text) {
                let name = node
                    .child_by_field_name("name")
                    .and_then(|name| name.utf8_text(text.as_bytes()).ok())
                    .unwrap_or(node.kind())
                    .to_string();
                let line = node.start_position().row + 1;
                out.push((name, line as u32));
            }
            pending_cfg = false;
        }
        out
    }

    /// One parsed top-level Rust item with its test status.
    /// Leading attributes merge into their item (the grammar splits
    /// `#[test]` into a sibling node); test status folds the whole run.
    pub(crate) fn rust_top_items(&mut self, text: &str) -> Vec<TopItem> {
        let Ok(tree) = self.parse_tree("rust", text) else {
            return Vec::new();
        };
        let root = tree.root_node();
        let mut cursor = root.walk();
        let nodes: Vec<tree_sitter::Node> = root
            .children(&mut cursor)
            .filter(tree_sitter::Node::is_named)
            .collect();
        let mut items = Vec::new();
        let mut pending: Vec<(u64, u64, bool)> = Vec::new();
        for node in nodes {
            if node.kind() == "attribute_item" {
                let testy = node
                    .utf8_text(text.as_bytes())
                    .is_ok_and(|attr| attr.starts_with("#[test") || attr.starts_with("#[cfg(test"));
                pending.push((node.start_byte() as u64, node.end_byte() as u64, testy));
                continue;
            }
            let start = pending
                .first()
                .map_or(node.start_byte() as u64, |attr| attr.0);
            let is_test = pending.iter().any(|attr| attr.2) || item_is_test(&node, text);
            pending.clear();
            items.push(TopItem {
                start,
                end: node.end_byte() as u64,
                kind: node.kind().to_string(),
                is_test,
            });
        }
        for (start, end, _) in pending {
            items.push(TopItem {
                start,
                end,
                kind: "attribute_item".to_string(),
                is_test: false,
            });
        }
        items
    }

    /// Byte spans of Rust test regions: `#[test]`/`#[cfg(test)]` items
    /// and test modules, merged and sorted.
    pub(crate) fn rust_test_spans(&mut self, text: &str) -> Vec<(u64, u64)> {
        let mut spans: Vec<(u64, u64)> = self
            .rust_top_items(text)
            .into_iter()
            .filter(|item| item.is_test)
            .map(|item| (item.start, item.end))
            .collect();
        spans.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::new();
        for span in spans {
            if let Some(last) = merged.last_mut() {
                if span.0 <= last.1 {
                    last.1 = last.1.max(span.1);
                    continue;
                }
            }
            merged.push(span);
        }
        merged
    }

    /// Count of ERROR and MISSING nodes: a fix must never raise it.
    pub(crate) fn rust_error_count(&mut self, text: &str) -> usize {
        let Ok(tree) = self.parse_tree("rust", text) else {
            return usize::MAX;
        };
        count_error_nodes(&tree.root_node())
    }

    /// Prose units in order: comments and string literals, trimmed.
    /// Code between them never enters; prose equality is multiset.
    pub(crate) fn rust_prose_units(&mut self, text: &str) -> Vec<String> {
        let Ok(tree) = self.parse_tree("rust", text) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        collect_prose(&tree.root_node(), text, &mut out);
        out.into_iter()
            .map(|unit| unit.trim_start().to_string())
            .collect()
    }

    /// Normalized headers of public items and impl blocks: the API
    /// surface a patch must not move without an explicit flag.
    pub(crate) fn rust_pub_headers(&mut self, text: &str) -> Vec<String> {
        let Ok(tree) = self.parse_tree("rust", text) else {
            return Vec::new();
        };
        let root = tree.root_node();
        let mut cursor = root.walk();
        let mut headers = Vec::new();
        for node in root
            .children(&mut cursor)
            .filter(tree_sitter::Node::is_named)
        {
            let node_text = node.utf8_text(text.as_bytes()).unwrap_or("").to_string();
            let header = header_before_body(&node_text);
            let is_pub = header.split_whitespace().any(|token| token == "pub")
                || node.kind() == "impl_item" && header.contains(" for ");
            if is_pub {
                headers.push(crate::item_hashes::normalize_signature_text(&header));
            }
        }
        headers.sort();
        headers
    }
}

/// One top-level item span with its test status.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One top-level item span with its test status.
pub struct TopItem {
    /// Byte start of the item (attributes included).
    pub(crate) start: u64,
    /// Byte end (exclusive).
    pub(crate) end: u64,
    /// Grammar node kind (`function_item`, `mod_item`, …).
    pub(crate) kind: String,
    /// Test region (`#[test]`, `#[cfg(test)]`, or test module).
    pub(crate) is_test: bool,
}

/// Whether a top-level node carries its own `#[cfg]` attribute child
/// (some shapes nest attributes instead of splitting siblings).
fn has_cfg_attr(node: &tree_sitter::Node, text: &str) -> bool {
    (0..node.child_count())
        .filter_map(|i| node.child(i))
        .any(|child| {
            child.kind() == "attribute_item"
                && child
                    .utf8_text(text.as_bytes())
                    .is_ok_and(|attr| attr.starts_with("#[cfg"))
        })
}

/// Whether a top-level node is a test region.
fn item_is_test(node: &tree_sitter::Node, text: &str) -> bool {
    let mut cursor = node.walk();
    let test_attr = node.children(&mut cursor).any(|child| {
        child.kind() == "attribute_item"
            && child
                .utf8_text(text.as_bytes())
                .is_ok_and(|attr| attr.starts_with("#[test") || attr.starts_with("#[cfg(test"))
    });
    if test_attr {
        return true;
    }
    if node.kind() == "mod_item" {
        let mut inner = node.walk();
        let name = node
            .child_by_field_name("name")
            .and_then(|name| name.utf8_text(text.as_bytes()).ok())
            .unwrap_or_default();
        if name == "tests" {
            return true;
        }
        return node.children(&mut inner).any(|child| {
            child.kind() == "attribute_item"
                && child
                    .utf8_text(text.as_bytes())
                    .is_ok_and(|attr| attr.starts_with("#[cfg(test"))
        });
    }
    false
}

/// Header text before the first body opener: signatures, even multiline,
/// end where the body starts. Attributes rarely brace, and a miss only
/// widens the header (safe toward refusal).
fn header_before_body(item_text: &str) -> String {
    let mut depth = 0usize;
    for (index, c) in item_text.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            '{' | ';' if depth == 0 => return item_text[..index].to_string(),
            _ => {}
        }
    }
    item_text.to_string()
}

/// Recursively count ERROR and MISSING nodes.
fn count_error_nodes(node: &tree_sitter::Node) -> usize {
    let mut count = usize::from(node.is_error() || node.is_missing());
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        count += count_error_nodes(&child);
    }
    count
}

/// Collect comment and string-literal node texts in source order.
fn collect_prose(node: &tree_sitter::Node, text: &str, out: &mut Vec<String>) {
    if node.kind() == "line_comment"
        || node.kind() == "block_comment"
        || node.kind() == "string_literal"
        || node.kind() == "char_literal"
    {
        if let Ok(unit) = node.utf8_text(text.as_bytes()) {
            out.push(unit.to_string());
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_prose(&child, text, out);
    }
}

fn extract_module_comment(root: &tree_sitter::Node, source: &str) -> Option<String> {
    module_comment_text(root, source)
}

fn extract_members(
    root: &tree_sitter::Node,
    source: &str,
    cursor: &mut tree_sitter::TreeCursor,
    language: &str,
) -> Vec<Member> {
    let mut members = Vec::new();
    let mut impl_methods: Vec<(String, Vec<Member>)> = Vec::new();
    let mut child = root.walk();
    for node in root.children(&mut child) {
        if node.kind() == "impl_item" {
            if let Some((type_name, methods)) = collect_impl_methods(&node, source, cursor) {
                impl_methods.push((type_name, methods));
            }
        } else if let Some(member) = extract_member(&node, source, cursor, language) {
            members.push(member);
        }
    }
    for (type_name, methods) in &impl_methods {
        if let Some(target) = members
            .iter_mut()
            .find(|m| m.name.as_str() == type_name.as_str())
        {
            target.members.extend(methods.iter().cloned());
        }
    }
    members
}

fn extract_member(
    node: &tree_sitter::Node,
    source: &str,
    cursor: &mut tree_sitter::TreeCursor,
    language: &str,
) -> Option<Member> {
    match node.kind() {
        "function_declaration" | "function_definition" => {
            extract_function(node, source, cursor, language)
        }
        "function_item" => extract_rust_function_or_test(node, source, cursor),
        "struct_declaration" | "class_definition" | "struct_item" | "trait_item" => {
            extract_struct_or_class(node, source, MemberType::Struct, language, true, None)
        }
        "enum_declaration" | "enum_item" => {
            extract_struct_or_class(node, source, MemberType::Enum, language, true, None)
        }
        "union_declaration" => {
            extract_struct_or_class(node, source, MemberType::Union, language, true, None)
        }
        "type_item" => extract_rust_type_alias(node, source),
        "macro_definition" => extract_rust_macro(node, source),
        "test_declaration" => extract_test(node, source),
        "variable_declaration" => extract_var_decl(node, source, cursor),
        "comptime_expression" => Some(extract_comptime_block(node, source, cursor, language)),
        _ => None,
    }
}

fn extract_comptime_block(
    node: &tree_sitter::Node,
    source: &str,
    cursor: &mut tree_sitter::TreeCursor,
    language: &str,
) -> Member {
    let line = node.start_position().row + 1;
    // Walk children of the comptime block to find inner declarations
    let mut child_members = Vec::new();
    let mut child = node.walk();
    for child_node in node.children(&mut child) {
        if let Some(inner) = extract_member(&child_node, source, cursor, language) {
            child_members.push(inner);
        }
    }

    let name = node
        .child_by_field_name("name")
        .or_else(|| {
            // For inline comptime { ... } blocks without name, use first child's name
            node.children(&mut node.walk())
                .find_map(|c| c.child_by_field_name("name"))
        })
        .and_then(|n| n.utf8_text(source.as_bytes()).ok())
        .unwrap_or("comptime");

    Member {
        type_name: MemberType::ComptimeBlock,
        name: name.into(),
        line: Some(line as u32),
        members: child_members,
        ..Member::default()
    }
}

fn extract_var_decl(
    node: &tree_sitter::Node,
    source: &str,
    _cursor: &mut tree_sitter::TreeCursor,
) -> Option<Member> {
    let name = node
        .children(&mut node.walk())
        .find(|c| c.kind() == "identifier")
        .and_then(|c| c.utf8_text(source.as_bytes()).ok())?;
    let is_pub = check_visibility(node, source, "zig");

    let mut child = node.walk();
    for child_node in node.children(&mut child) {
        match child_node.kind() {
            "struct_declaration" => {
                return extract_struct_or_class(
                    &child_node,
                    source,
                    MemberType::Struct,
                    "zig",
                    is_pub,
                    Some(name),
                );
            }
            "enum_declaration" => {
                return extract_struct_or_class(
                    &child_node,
                    source,
                    MemberType::Enum,
                    "zig",
                    is_pub,
                    Some(name),
                );
            }
            "union_declaration" => {
                return extract_struct_or_class(
                    &child_node,
                    source,
                    MemberType::Union,
                    "zig",
                    is_pub,
                    Some(name),
                );
            }
            _ => {}
        }
    }

    None
}

/// Span and item hashes for one member from its node slice: byte span,
/// normalized item identity, and the byte-exact range hash citations
/// re-hash live bytes against. `None` fields when the slice is invalid.
fn span_hashes(
    language: &str,
    node: &tree_sitter::Node,
    source: &str,
    sig: &str,
    doc: Option<&str>,
) -> (Option<u64>, Option<u64>, Option<String>, Option<String>) {
    let start = node.start_byte() as u64;
    let end = node.end_byte() as u64;
    let Some(body) = source.get(start as usize..end as usize) else {
        return (None, None, None, None);
    };
    let parts = crate::item_hashes::item_hashes_for(language, sig, body, doc);
    (
        Some(start),
        Some(end),
        Some(parts.item),
        Some(common_core::hash::blake3_hex(body.as_bytes())),
    )
}

fn extract_function(
    node: &tree_sitter::Node,
    source: &str,
    _cursor: &mut tree_sitter::TreeCursor,
    language: &str,
) -> Option<Member> {
    let name_node = node.child_by_field_name("name")?;
    let name = name_node.utf8_text(source.as_bytes()).ok()?;

    let doc_comment = extract_preceding_doc_comment(node, source);

    let params = match language {
        "zig" => extract_zig_params(node, source),
        "python" => extract_python_params(node, source)?,
        "rust" => extract_rust_params(node, source).unwrap_or_default(),
        _ => Vec::new(),
    };

    let parameters = params
        .iter()
        .map(|(n, t)| Param {
            name: n.as_str().into(),
            type_name: t.as_ref().map(|s| s.as_str().into()),
            default: None,
        })
        .collect();

    let return_type = match language {
        "zig" => extract_zig_return_type(node, source),
        "python" => extract_python_return_type(node, source),
        "rust" => extract_rust_return_type(node, source),
        _ => None,
    };

    let sig = build_signature(
        if language == "python" { "def" } else { "fn" },
        name,
        &params,
        return_type.as_deref(),
    );

    let is_pub = check_visibility(node, source, language);
    let line = node.start_position().row + 1;
    let (span_start, span_end, item_hash, range_hash) =
        span_hashes(language, node, source, &sig, doc_comment.as_deref());

    Some(Member {
        type_name: if is_pub {
            MemberType::FnDecl
        } else {
            MemberType::FnPrivate
        },
        name: name.into(),
        signature: Some(sig.as_str().into()),
        match_hash: Some(
            crate::sync::staleness::match_hash_from_signature(&sig)
                .as_str()
                .into(),
        ),
        span_start,
        span_end,
        item_hash: item_hash.as_deref().map(Into::into),
        range_hash: range_hash.as_deref().map(Into::into),
        params: parameters,
        returns: return_type.map(|r| r.as_str().into()),
        comment: doc_comment.map(|c| c.as_str().into()),
        is_pub,
        line: Some(line as u32),
        ..Member::default()
    })
}

fn extract_zig_params(node: &tree_sitter::Node, source: &str) -> Vec<(String, Option<String>)> {
    let mut params = Vec::new();
    let mut child = node.walk();
    for child_node in node.children(&mut child) {
        if child_node.kind() == "parameter" {
            let param_name = child_node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                .map_or_else(|| "_".into(), String::from);

            let param_type = child_node
                .child_by_field_name("type")
                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                .map(String::from);

            params.push((param_name, param_type));
        }
    }
    params
}

fn extract_zig_return_type(node: &tree_sitter::Node, source: &str) -> Option<String> {
    let mut child = node.walk();
    for child_node in node.children(&mut child) {
        for sub in child_node.children(&mut child_node.walk()) {
            if sub.kind() == "return_type" {
                return sub.utf8_text(source.as_bytes()).ok().map(String::from);
            }
        }
    }
    None
}

fn extract_python_params(
    node: &tree_sitter::Node,
    source: &str,
) -> Option<Vec<(String, Option<String>)>> {
    let mut params = Vec::new();
    let parameters = node.child_by_field_name("parameters")?;
    let mut child = parameters.walk();
    for child_node in parameters.children(&mut child) {
        let param_name = child_node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .map(String::from);

        let param_type = child_node
            .child_by_field_name("type")
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .map(String::from);

        if let Some(ref name) = param_name {
            if name != "self" && name != "cls" {
                params.push((param_name.unwrap(), param_type));
            }
        }
    }
    Some(params)
}

fn extract_python_return_type(node: &tree_sitter::Node, source: &str) -> Option<String> {
    let return_type = node.child_by_field_name("return_type")?;
    return_type
        .utf8_text(source.as_bytes())
        .ok()
        .map(String::from)
}

fn extract_rust_function_or_test(
    node: &tree_sitter::Node,
    source: &str,
    cursor: &mut tree_sitter::TreeCursor,
) -> Option<Member> {
    if has_rust_test_attribute(node, source) {
        let name = node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())?;
        let line = node.start_position().row + 1;
        return Some(Member {
            type_name: MemberType::TestDecl,
            name: name.into(),
            line: Some(line as u32),
            ..Member::default()
        });
    }
    extract_function(node, source, cursor, "rust")
}

fn has_rust_test_attribute(node: &tree_sitter::Node, source: &str) -> bool {
    let mut child = node.walk();
    for child_node in node.children(&mut child) {
        if child_node.kind() == "attribute_item" {
            let Ok(text) = child_node.utf8_text(source.as_bytes()) else {
                continue;
            };
            if text.contains("#[test]") || text.contains("#[test(") {
                return true;
            }
        }
    }
    false
}

fn collect_impl_methods(
    node: &tree_sitter::Node,
    source: &str,
    _cursor: &mut tree_sitter::TreeCursor,
) -> Option<(String, Vec<Member>)> {
    let type_name = node
        .child_by_field_name("type")
        .and_then(|n| n.utf8_text(source.as_bytes()).ok())?;
    let child_members = extract_body_members(node, source, "rust").unwrap_or_default();
    Some((type_name.into(), child_members))
}

fn extract_rust_type_alias(node: &tree_sitter::Node, source: &str) -> Option<Member> {
    let name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source.as_bytes()).ok())?;
    let line = node.start_position().row + 1;
    Some(Member {
        type_name: MemberType::Struct,
        name: name.into(),
        line: Some(line as u32),
        ..Member::default()
    })
}

fn extract_rust_macro(node: &tree_sitter::Node, source: &str) -> Option<Member> {
    let name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source.as_bytes()).ok())?;
    let line = node.start_position().row + 1;
    Some(Member {
        type_name: MemberType::FnDecl,
        name: name.into(),
        line: Some(line as u32),
        ..Member::default()
    })
}

fn extract_rust_params(
    node: &tree_sitter::Node,
    source: &str,
) -> Option<Vec<(String, Option<String>)>> {
    let parameters = node.child_by_field_name("parameters")?;
    let mut params = Vec::new();
    let mut child = parameters.walk();
    for child_node in parameters.children(&mut child) {
        if child_node.kind() == "parameter" {
            let param_name = child_node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                .map(String::from);
            let param_type = child_node
                .child_by_field_name("type")
                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                .map(String::from);
            if let Some(name) = param_name {
                if name != "self" && name != "&self" && name != "&mut self" && name != "mut self" {
                    params.push((name, param_type));
                }
            }
        }
    }
    Some(params)
}

fn extract_rust_return_type(node: &tree_sitter::Node, source: &str) -> Option<String> {
    let return_type = node.child_by_field_name("return_type")?;
    return_type
        .utf8_text(source.as_bytes())
        .ok()
        .map(String::from)
}

fn extract_struct_or_class(
    node: &tree_sitter::Node,
    source: &str,
    member_type: MemberType,
    language: &str,
    is_pub: bool,
    override_name: Option<&str>,
) -> Option<Member> {
    let name = override_name.map_or_else(
        || {
            node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                .map(String::from)
        },
        |n| Some(n.to_string()),
    )?;
    let doc_comment = extract_preceding_doc_comment(node, source);

    let child_members = extract_body_members(node, source, language)?;

    let sig = format!(
        "{} {} {{ ... }}",
        match member_type {
            MemberType::Struct => "struct",
            MemberType::Enum => "enum",
            MemberType::Union => "union",
            _ => "class",
        },
        name
    );

    let line = node.start_position().row + 1;
    let (span_start, span_end, item_hash, range_hash) =
        span_hashes(language, node, source, &sig, doc_comment.as_deref());

    Some(Member {
        type_name: member_type,
        name: name.into(),
        signature: Some(sig.as_str().into()),
        match_hash: Some(
            crate::sync::staleness::match_hash_from_signature(&sig)
                .as_str()
                .into(),
        ),
        span_start,
        span_end,
        item_hash: item_hash.as_deref().map(Into::into),
        range_hash: range_hash.as_deref().map(Into::into),
        comment: doc_comment.map(|c| c.as_str().into()),
        is_pub,
        members: child_members,
        line: Some(line as u32),
        ..Member::default()
    })
}

fn extract_body_members(
    node: &tree_sitter::Node,
    source: &str,
    language: &str,
) -> Option<Vec<Member>> {
    let mut members = Vec::new();

    let children_iter: Vec<tree_sitter::Node> = if let Some(body) = node.child_by_field_name("body")
    {
        let mut child = body.walk();
        body.children(&mut child).collect()
    } else {
        let mut child = node.walk();
        node.children(&mut child).collect()
    };

    for child_node in children_iter {
        match child_node.kind() {
            "function_declaration" | "function_definition" | "function_item" => {
                let name_node = child_node.child_by_field_name("name")?;
                let name = name_node.utf8_text(source.as_bytes()).ok()?;
                let line = child_node.start_position().row + 1;
                let dc = extract_preceding_doc_comment(&child_node, source);
                let is_pub = check_visibility(&child_node, source, language);
                let params = match language {
                    "rust" => extract_rust_params(&child_node, source).unwrap_or_default(),
                    _ => Vec::new(),
                };
                let return_type = match language {
                    "rust" => extract_rust_return_type(&child_node, source),
                    _ => None,
                };
                let sig = build_signature("fn", name, &params, return_type.as_deref());
                let (span_start, span_end, item_hash, range_hash) =
                    span_hashes(language, &child_node, source, &sig, dc.as_deref());
                members.push(Member {
                    type_name: if is_pub {
                        MemberType::Method
                    } else {
                        MemberType::MethodPrivate
                    },
                    name: name.into(),
                    signature: Some(sig.as_str().into()),
                    match_hash: Some(
                        crate::sync::staleness::match_hash_from_signature(&sig)
                            .as_str()
                            .into(),
                    ),
                    span_start,
                    span_end,
                    item_hash: item_hash.as_deref().map(Into::into),
                    range_hash: range_hash.as_deref().map(Into::into),
                    comment: dc.map(|c| c.as_str().into()),
                    is_pub,
                    line: Some(line as u32),
                    ..Member::default()
                });
            }
            "macro_definition" => {
                let name_node = child_node.child_by_field_name("name")?;
                let name = name_node.utf8_text(source.as_bytes()).ok()?;
                let line = child_node.start_position().row + 1;
                members.push(Member {
                    type_name: MemberType::Method,
                    name: name.into(),
                    line: Some(line as u32),
                    ..Member::default()
                });
            }
            _ => {}
        }
    }
    Some(members)
}

fn extract_test(node: &tree_sitter::Node, source: &str) -> Option<Member> {
    // Try field name first
    if let Some(name_node) = node.child_by_field_name("name") {
        let raw_name = name_node.utf8_text(source.as_bytes()).ok()?;
        let clean_name = raw_name
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(raw_name);
        let line = node.start_position().row + 1;
        return Some(Member {
            type_name: MemberType::TestDecl,
            name: clean_name.into(),
            line: Some(line as u32),
            ..Member::default()
        });
    }

    // Fallback: find string child among direct children
    let mut child = node.walk();
    let name_node = node
        .children(&mut child)
        .find(|c| c.kind() == "string_literal" || c.kind() == "string")?;
    let raw_name = name_node.utf8_text(source.as_bytes()).ok()?;
    let clean_name = raw_name
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(raw_name);
    // clean_name already set above

    let line = node.start_position().row + 1;

    Some(Member {
        type_name: MemberType::TestDecl,
        name: clean_name.into(),
        line: Some(line as u32),
        ..Member::default()
    })
}

fn extract_preceding_doc_comment(node: &tree_sitter::Node, source: &str) -> Option<String> {
    preceding_doc_text(node, source)
}

fn check_visibility(node: &tree_sitter::Node, source: &str, language: &str) -> bool {
    if language == "python" {
        let Some(name_node) = node.child_by_field_name("name") else {
            return true;
        };
        if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
            return !name.starts_with('_');
        }
        return true;
    }

    let mut child = node.walk();
    for child_node in node.children(&mut child) {
        let kind = child_node.kind();
        if kind == "pub" || kind == "visibility_modifier" {
            return true;
        }
    }
    false
}

fn build_signature(
    kind: &str,
    name: &str,
    params: &[(String, Option<String>)],
    return_type: Option<&str>,
) -> String {
    let mut sig = String::new();
    sig.push_str(kind);
    sig.push(' ');
    sig.push_str(name);
    sig.push('(');
    for (i, (pname, ptype)) in params.iter().enumerate() {
        if i > 0 {
            sig.push_str(", ");
        }
        sig.push_str(pname);
        if let Some(t) = ptype {
            sig.push_str(": ");
            sig.push_str(t);
        }
    }
    sig.push(')');
    if let Some(r) = return_type {
        sig.push_str(" -> ");
        sig.push_str(r);
    }
    sig
}

pub fn file_type_from_extension(ext: &str) -> FileType {
    if ext.starts_with('.') {
        FileType::from_extension(ext)
    } else {
        FileType::from_extension(&format!(".{ext}"))
    }
}

pub fn resolve_span(path: &Path, member_name: &str, member_type: MemberType) -> Option<Span> {
    let source = common_core::io::read_to_string_err(path).ok()?;
    let ext = path.extension().and_then(|e| e.to_str())?;
    let mut parser = AstParser::new();
    let language = AstParser::language_from_ext(ext)?;
    let tree = match language {
        "zig" => parser.zig.parse(&source, None)?,
        "python" => parser.python.parse(&source, None)?,
        "rust" => parser.rust.parse(&source, None)?,
        _ => return None,
    };
    let root = tree.root_node();
    find_member_span(root, &source, member_name, member_type)
}

fn find_member_span(
    root: tree_sitter::Node,
    source: &str,
    member_name: &str,
    member_type: MemberType,
) -> Option<Span> {
    let mut cursor = root.walk();
    for node in root.children(&mut cursor) {
        if let Some(span) = try_match_node_span(&node, source, member_name, member_type) {
            return Some(span);
        }
    }
    None
}

fn try_match_node_span(
    node: &tree_sitter::Node,
    source: &str,
    member_name: &str,
    member_type: MemberType,
) -> Option<Span> {
    let name_node = node.child_by_field_name("name");
    let node_name = name_node
        .as_ref()
        .and_then(|n| n.utf8_text(source.as_bytes()).ok())?;

    let matches_type = match node.kind() {
        "function_declaration" | "function_definition" | "function_item" => {
            matches!(
                member_type,
                MemberType::FnDecl
                    | MemberType::FnPrivate
                    | MemberType::Method
                    | MemberType::MethodPrivate
            )
        }
        "struct_declaration" | "struct_item" | "class_definition" => {
            member_type == MemberType::Struct
        }
        "enum_declaration" | "enum_item" => member_type == MemberType::Enum,
        "union_declaration" => member_type == MemberType::Union,
        "test_declaration" => member_type == MemberType::TestDecl,
        _ => false,
    };

    if matches_type && node_name == member_name {
        return Some(node_span(node, source));
    }

    if node.kind() == "impl_item" {
        let type_name = name_node?;
        let type_text = type_name.utf8_text(source.as_bytes()).ok()?;
        if type_text == member_name
            && matches!(
                member_type,
                MemberType::Struct | MemberType::FnDecl | MemberType::Method
            )
        {
            return Some(node_span(node, source));
        }
        let mut child_cursor = node.walk();
        for child in node.children(&mut child_cursor) {
            if let Some(span) = try_match_node_span(&child, source, member_name, member_type) {
                return Some(span);
            }
        }
    }

    if node.kind() == "declaration_list" || node.kind() == "block" {
        let mut child_cursor = node.walk();
        for child in node.children(&mut child_cursor) {
            if let Some(span) = try_match_node_span(&child, source, member_name, member_type) {
                return Some(span);
            }
        }
    }

    None
}

fn node_span(node: &tree_sitter::Node, source: &str) -> Span {
    let start_line = find_doc_comment_start(node, source);
    let end_line = node.end_position().row as u32 + 1;
    Span {
        start_line: start_line + 1,
        end_line,
    }
}

fn find_doc_comment_start(node: &tree_sitter::Node, source: &str) -> u32 {
    let mut current = *node;
    let mut candidate_start = node.start_position().row as u32;

    while let Some(prev) = current.prev_sibling() {
        let kind = prev.kind();
        if kind == "doc_comment" || kind == "comment" {
            let text = prev.utf8_text(source.as_bytes()).unwrap_or("");
            if text.starts_with("///") || text.starts_with("//!") || text.starts_with("#[") {
                candidate_start = prev.start_position().row as u32;
                current = prev;
                continue;
            }
        }
        if kind == "attribute_item" || kind == "outer_attribute" {
            candidate_start = prev.start_position().row as u32;
            current = prev;
            continue;
        }
        break;
    }
    candidate_start
}

/// Extracts a code snippet for a member from its source file using tree-sitter AST.
///
/// Given a source path (relative to workspace), member name, member type, and
/// workspace root, reads the source file, parses it with tree-sitter to find
/// the exact span, and returns the code lines along with 1-based line bounds.
///
/// Follows the Zig `cmdExplain` pattern: AST-driven line extraction, not
/// heuristic brace counting. Uses the tree-sitter span end line for exact
/// boundaries (no arbitrary truncation).
///
/// Returns `(snippet, start_line, end_line)` where lines are 1-based.
pub fn extract_code_snippet(
    source_path: &str,
    member_name: &str,
    member_type: MemberType,
    workspace: &std::path::Path,
    max_lines: usize,
) -> Option<(String, u32, u32)> {
    let abs_path = workspace.join(source_path);
    let source = common_core::io::read_to_string_err(&abs_path).ok()?;
    let span = resolve_span(&abs_path, member_name, member_type)?;

    let start = (span.start_line as usize).saturating_sub(1);
    let span_end = span.end_line as usize;
    let limited_end = start + max_lines;
    let end = span_end.min(limited_end);
    let lines: Vec<&str> = source.lines().collect();
    if start >= lines.len() {
        return None;
    }
    let end = end.min(lines.len());
    let snippet: String = lines[start..end].join("\n");
    if snippet.trim().is_empty() {
        None
    } else {
        Some((snippet, span.start_line, span.end_line))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_parse_helper(source: &str, filename: &str) -> GuidanceDoc {
        let mut parser = AstParser::new();
        parser
            .parse_file(Path::new(filename), source)
            .expect("should parse")
    }

    #[test]
    fn test_parse_zig_file() {
        let source = r#"/// Sample Zig file for AST parsing tests
const std = @import("std");

pub fn greet(name: []const u8) []const u8 {
    return "Hello, " ++ name;
}

pub const Config = struct {
    port: u16 = 8079,
    host: []const u8 = "localhost",
};
"#;

        let doc = test_parse_helper(source, "main.zig");
        assert_eq!(doc.meta.language.as_str(), "zig");
        assert!(
            !doc.members.is_empty(),
            "should have at least one member, got 0"
        );
        for m in &doc.members {
            eprintln!("member: name={:?} type={:?}", m.name, m.type_name);
        }
        let greet = doc.members.iter().find(|m| m.name == "greet");
        assert!(greet.is_some(), "should find greet function");
        let config = doc.members.iter().find(|m| m.name == "Config");
        assert!(config.is_some(), "should find Config struct");
    }

    #[test]
    fn test_parse_python_file() {
        let source = r#"def hello(name: str) -> str:
    return f"Hello, {name}"

class MyClass:
    def method(self, x: int) -> int:
        return x * 2
"#;

        let doc = test_parse_helper(source, "main.py");
        assert_eq!(doc.meta.language.as_str(), "python");
        assert!(doc.members.len() >= 2);
        let hello = doc.members.iter().find(|m| m.name == "hello");
        assert!(hello.is_some(), "should find hello function");
        let cls = doc.members.iter().find(|m| m.name == "MyClass");
        assert!(cls.is_some(), "should find MyClass");
    }

    #[test]
    fn test_extract_zig_doc_comment() {
        let doc = test_parse_helper(
            "/// This is a test function\npub fn test_fn() void {}\n",
            "test.zig",
        );
        let func = doc.members.first().expect("should have a function");
        assert_eq!(
            func.comment.as_ref().map(smol_str::SmolStr::as_str),
            Some("This is a test function")
        );
    }

    #[test]
    fn test_extract_line_numbers() {
        let doc = test_parse_helper("pub fn foo() void {}\n\npub fn bar() void {}", "foo.zig");
        assert!(!doc.members.is_empty());
    }

    #[test]
    fn test_unsupported_language() {
        let mut parser = AstParser::new();
        let result = parser.parse_file(Path::new("main.c"), "int main() {}");
        assert!(result.is_err());
        match result {
            Err(ParseError::UnsupportedLanguage(_)) => {}
            _ => panic!("expected UnsupportedLanguage error"),
        }
    }

    #[test]
    fn m5_dispatch_misses_are_terminal_not_fallback() {
        // M5.1: the format/language matches are single dispatch, not a
        // fallback walk — a miss is terminal, never "try the next parser".
        let mut parser = AstParser::new();
        match parser.parse_tree("bogus", "fn f() {}") {
            Err(ParseError::UnsupportedLanguage(_)) => {}
            other => panic!("expected UnsupportedLanguage, got {other:?}"),
        }
        assert!(!AstParser::has_grammar("bogus"));
        assert!(AstParser::has_grammar("rust"));
        // Unknown language degrades to "has errors" (fail-closed), not to
        // another grammar.
        assert!(parser.has_errors("fn f() {}", "bogus"));
    }

    #[test]
    fn test_extract_comptime_block() {
        let source = r#"const std = @import("std");

pub fn main() void {
    comptime {
        const x: u32 = 42;
        const y: u32 = x + 1;
    }
}
"#;
        let doc = test_parse_helper(source, "comptime.zig");
        assert_eq!(doc.meta.language.as_str(), "zig");
        let main_fn = doc.members.iter().find(|m| m.name == "main");
        assert!(main_fn.is_some(), "should find main function");
    }

    #[test]
    fn test_extract_test_decl() {
        let source = r#"test "hello test" {
    try std.testing.expectEqual(1, 1);
}
"#;
        let doc = test_parse_helper(source, "test.zig");
        let test_decl = doc.members.iter().find(|m| m.name == "hello test");
        assert!(test_decl.is_some(), "should find test declaration");
        if let Some(t) = test_decl {
            assert_eq!(t.type_name, MemberType::TestDecl, "should be TestDecl type");
        }
    }

    #[test]
    fn test_file_type_from_extension() {
        assert_eq!(file_type_from_extension("zig"), FileType::Source);
        assert_eq!(file_type_from_extension("py"), FileType::Source);
        assert_eq!(file_type_from_extension(".zig"), FileType::Source);
        assert_eq!(file_type_from_extension("md"), FileType::Markdown);
        assert_eq!(file_type_from_extension("json"), FileType::Config);
        assert_eq!(file_type_from_extension("xyz"), FileType::Unknown);
    }
}

#[cfg(test)]
#[path = "../tests/comment_scope.rs"]
mod comment_scope_tests;
