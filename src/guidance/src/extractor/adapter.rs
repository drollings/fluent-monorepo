//! P2 language adapters (port of `extraction/code/adapter.ts` +
//! `languages/*` + `families/*`): per-format entity/scope tables and
//! node helpers over `tree_sitter::Node`. Dispatch is registry-based
//! (never a match); each adapter implements the P0 `LanguageAdapter`
//! trait. Node-type strings mirror the grammar versions pinned in the
//! workspace (tree-sitter 0.24–0.25 generation).

use crate::search_types::{CodeEntityModifier, CodeSymbolType, LanguageAdapter};

/// A borrowed tree-sitter node with its source text.
#[derive(Clone, Copy)]
pub struct NodeView<'a> {
    /// The node.
    pub node: tree_sitter::Node<'a>,
    /// Full source bytes (for text extraction).
    pub source: &'a [u8],
}

impl<'a> NodeView<'a> {
    /// Node text.
    #[must_use]
    pub fn text(self) -> &'a str {
        self.node.utf8_text(self.source).unwrap_or("")
    }

    /// Node type (`kind`).
    #[must_use]
    pub fn kind(self) -> &'static str {
        self.node.kind()
    }

    /// Child by field name.
    #[must_use]
    pub fn field(self, name: &str) -> Option<Self> {
        self.node
            .child_by_field_name(name)
            .map(|node| Self { node, source: self.source })
    }

    /// Named children.
    #[must_use]
    pub fn named_children(self) -> Vec<Self> {
        let mut cursor = self.node.walk();
        self.node
            .named_children(&mut cursor)
            .map(|node| Self { node, source: self.source })
            .collect()
    }

    /// All children (named or not).
    #[must_use]
    pub fn children_all(self) -> Vec<Self> {
        let mut cursor = self.node.walk();
        self.node
            .children(&mut cursor)
            .map(|node| Self { node, source: self.source })
            .collect()
    }

    /// Previous named sibling, if any.
    #[must_use]
    pub fn prev_named_sibling(self) -> Option<Self> {
        let mut current = self.node.prev_sibling()?;
        loop {
            if current.is_named() {
                return Some(Self { node: current, source: self.source });
            }
            current = current.prev_sibling()?;
        }
    }

    /// Parent node, if any.
    #[must_use]
    pub fn parent(self) -> Option<Self> {
        self.node
            .parent()
            .map(|node| Self { node, source: self.source })
    }

    /// Walk ancestors for a node type.
    #[must_use]
    pub fn closest_ancestor(self, kind: &str) -> Option<Self> {
        let mut current = self.parent();
        while let Some(view) = current {
            if view.kind() == kind {
                return Some(view);
            }
            current = view.parent();
        }
        None
    }

    /// First descendant (depth-first, named only) with the given type.
    #[must_use]
    pub fn find_descendant_by_type(self, kind: &str) -> Option<Self> {
        if self.kind() == kind {
            return Some(self);
        }
        for child in self.named_children() {
            if let Some(found) = child.find_descendant_by_type(kind) {
                return Some(found);
            }
        }
        None
    }
}

/// Adapter family (shared per-family helpers, mirroring `families/*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterFamily {
    /// Rust (`extractCommonModifiers` + generic signature/doc).
    Rust,
    /// Python (decorated definitions, async/static detection).
    Python,
    /// Go (receivers, exported-capitalized, interface/class mapping).
    Go,
    /// C/C++ (declarator diving, typedef classification).
    CFamily,
    /// Java (name-field extraction).
    NameField,
    /// JavaScript/TypeScript (function values, object entities).
    JsTs,
}

/// Per-format extraction adapter (port of `LanguageAdapter`).
pub struct TsAdapter {
    /// Canonical format id.
    pub format: &'static str,
    /// File extensions routed here.
    pub extensions: &'static [&'static str],
    /// Entity node types.
    pub entity_types: &'static [&'static str],
    /// Scope node types.
    pub scope_types: &'static [&'static str],
    /// Behavior family.
    pub family: AdapterFamily,
}

impl LanguageAdapter for TsAdapter {
    fn format(&self) -> &'static str {
        self.format
    }
    fn extensions(&self) -> &'static [&'static str] {
        self.extensions
    }
    fn scope_separator(&self) -> &'static str {
        "::"
    }
}

static ADAPTERS: &[TsAdapter] = &[
    TsAdapter {
        format: "rust",
        extensions: &["rs"],
        entity_types: &[
            "enum_item",
            "function_item",
            "function_signature_item",
            "impl_item",
            "struct_item",
            "trait_item",
            "type_item",
            "union_item",
        ],
        scope_types: &["impl_item", "mod_item", "trait_item"],
        family: AdapterFamily::Rust,
    },
    TsAdapter {
        format: "python",
        extensions: &["py"],
        entity_types: &["class_definition", "decorated_definition", "function_definition"],
        scope_types: &["class_definition", "decorated_definition"],
        family: AdapterFamily::Python,
    },
    TsAdapter {
        format: "typescript",
        extensions: &["ts", "mts", "cts"],
        entity_types: &[
            "abstract_class_declaration",
            "abstract_method_signature",
            "class_declaration",
            "enum_declaration",
            "field_definition",
            "function_declaration",
            "generator_function_declaration",
            "interface_declaration",
            "method_signature",
            "method_definition",
            "pair",
            "public_field_definition",
            "type_alias_declaration",
            "variable_declarator",
        ],
        scope_types: &[
            "abstract_class_declaration",
            "class_declaration",
            "internal_module",
            "interface_declaration",
            "module_declaration",
            "namespace_declaration",
        ],
        family: AdapterFamily::JsTs,
    },
    TsAdapter {
        format: "tsx",
        extensions: &["tsx"],
        entity_types: &[
            "abstract_class_declaration",
            "abstract_method_signature",
            "class_declaration",
            "enum_declaration",
            "field_definition",
            "function_declaration",
            "generator_function_declaration",
            "interface_declaration",
            "method_signature",
            "method_definition",
            "pair",
            "public_field_definition",
            "type_alias_declaration",
            "variable_declarator",
        ],
        scope_types: &[
            "abstract_class_declaration",
            "class_declaration",
            "internal_module",
            "interface_declaration",
            "module_declaration",
            "namespace_declaration",
        ],
        family: AdapterFamily::JsTs,
    },
    TsAdapter {
        format: "javascript",
        extensions: &["js", "mjs", "cjs"],
        entity_types: &[
            "class_declaration",
            "field_definition",
            "function_declaration",
            "generator_function_declaration",
            "method_definition",
            "pair",
            "variable_declarator",
        ],
        scope_types: &["class_declaration"],
        family: AdapterFamily::JsTs,
    },
    TsAdapter {
        format: "jsx",
        extensions: &["jsx"],
        entity_types: &[
            "class_declaration",
            "field_definition",
            "function_declaration",
            "generator_function_declaration",
            "method_definition",
            "pair",
            "variable_declarator",
        ],
        scope_types: &["class_declaration"],
        family: AdapterFamily::JsTs,
    },
    TsAdapter {
        format: "go",
        extensions: &["go"],
        entity_types: &[
            "function_declaration",
            "method_elem",
            "method_declaration",
            "type_alias",
            "type_spec",
        ],
        scope_types: &["type_spec"],
        family: AdapterFamily::Go,
    },
    TsAdapter {
        format: "java",
        extensions: &["java"],
        entity_types: &[
            "annotation_type_declaration",
            "class_declaration",
            "constructor_declaration",
            "enum_declaration",
            "interface_declaration",
            "method_declaration",
            "record_declaration",
        ],
        scope_types: &[
            "annotation_type_declaration",
            "class_declaration",
            "enum_declaration",
            "interface_declaration",
            "record_declaration",
        ],
        family: AdapterFamily::NameField,
    },
    TsAdapter {
        format: "c",
        extensions: &["c", "h"],
        entity_types: &[
            "declaration",
            "field_declaration",
            "function_definition",
            "macro_type_specifier",
            "struct_specifier",
            "union_specifier",
            "enum_specifier",
            "type_definition",
        ],
        scope_types: &["struct_specifier", "union_specifier"],
        family: AdapterFamily::CFamily,
    },
    TsAdapter {
        format: "cpp",
        extensions: &["cc", "cpp", "cxx", "hpp"],
        entity_types: &[
            "alias_declaration",
            "declaration",
            "field_declaration",
            "function_definition",
            "macro_type_specifier",
            "class_specifier",
            "struct_specifier",
            "union_specifier",
            "enum_specifier",
        ],
        scope_types: &[
            "namespace_definition",
            "class_specifier",
            "struct_specifier",
            "union_specifier",
        ],
        family: AdapterFamily::CFamily,
    },
];

/// Resolve the adapter for a format id (registry search, never a match).
#[must_use]
pub fn resolve_adapter(format: &str) -> Option<&'static TsAdapter> {
    // M5: registry find, not a fallback walk — one lookup, a miss is
    // terminal `None` (never "try the next adapter"). Stays.
    ADAPTERS.iter().find(|adapter| adapter.format == format)
}

/// Resolve the format id for a file extension (registry scan).
/// Delegates to the canonical `fluent_types::file_kind` adapter map
/// (`h` parses as C here — the ingest tagger maps it to C++ instead;
/// that divergence is documented at the canonical home, never unified
/// implicitly).
#[must_use]
pub fn format_for_extension(ext: &str) -> Option<&'static str> {
    let lowered = ext.to_ascii_lowercase();
    fluent_types::file_kind::code_format_for_extension(&lowered)
}

/// Structured code formats with grammars (port of `STRUCTURED_CODE_FORMATS`).
#[must_use]
pub fn structured_code_formats() -> Vec<&'static str> {
    ADAPTERS.iter().map(|adapter| adapter.format).collect()
}

/// Component script-block formats (port of `COMPONENT_CODE_FORMATS`).
pub const COMPONENT_CODE_FORMATS: &[&str] = &["vue", "svelte"];

impl TsAdapter {
    /// Entity name (family-specific `extractName`).
    #[must_use]
    pub fn extract_name(&self, node: NodeView<'_>) -> Option<String> {
        match self.family {
            AdapterFamily::Rust => {
                if node.kind() == "impl_item" {
                    return node.field("type").map(|t| t.text().to_string());
                }
                node.field("name").map(|n| n.text().to_string())
            }
            AdapterFamily::Python => {
                if node.kind() == "decorated_definition" {
                    let inner = node.named_children().into_iter().find(|child| {
                        child.kind() == "function_definition" || child.kind() == "class_definition"
                    })?;
                    return self.extract_name(inner);
                }
                node.field("name").map(|n| n.text().to_string())
            }
            AdapterFamily::Go => node.field("name").map(|n| n.text().to_string()),
            AdapterFamily::CFamily => extract_c_name(node),
            AdapterFamily::NameField => name_field(node),
            AdapterFamily::JsTs => extract_js_ts_name(node),
        }
    }

    /// Whether the node is indexed (family-specific `shouldIndexEntity`).
    #[must_use]
    pub fn should_index_entity(&self, node: NodeView<'_>) -> bool {
        match self.family {
            AdapterFamily::CFamily => should_index_c_entity(node),
            AdapterFamily::JsTs => should_index_js_ts_entity(node),
            _ => true,
        }
    }

    /// Whether a scope node is entered (family-specific `shouldEnterScope`).
    #[must_use]
    pub fn should_enter_scope(&self, node: NodeView<'_>) -> bool {
        match self.family {
            AdapterFamily::Go => {
                if node.kind() != "type_spec" {
                    return true;
                }
                matches!(
                    node.field("type").map(NodeView::kind),
                    Some("interface_type" | "struct_type")
                )
            }
            AdapterFamily::Python => {
                if node.kind() != "decorated_definition" {
                    return true;
                }
                node.named_children()
                    .iter()
                    .any(|child| child.kind() == "class_definition")
            }
            _ => true,
        }
    }

    /// Harvest raw import specifiers from a parsed file root (P3 graph
    /// edges). Family-specific node tables; string-like descendants are
    /// preferred, falling back to the trimmed node text. One walk, no
    /// second parser.
    #[must_use]
    pub fn import_specifiers(&self, root: NodeView<'_>) -> Vec<String> {
        let mut specifiers = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![root];
        while let Some(view) = stack.pop() {
            if is_import_node(self.family, view.kind()) {
                for specifier in import_specifier_texts(self.family, view) {
                    if seen.insert(specifier.clone()) {
                        specifiers.push(specifier);
                    }
                }
            }
            stack.extend(view.named_children());
        }
        specifiers
    }

    /// Resolve one node into indexable entities (js object members).
    #[must_use]
    #[allow(clippy::elidable_lifetime_names)]
    pub fn resolve_entities<'a>(&self, node: NodeView<'a>) -> Vec<NodeView<'a>> {
        if self.family == AdapterFamily::JsTs && node.kind() == "variable_declarator" {
            let members = exported_object_function_entities(node);
            if !members.is_empty() {
                return members;
            }
        }
        vec![node]
    }

    /// Scope node to descend into (go type bodies, python decorated classes).
    #[must_use]
    #[allow(clippy::elidable_lifetime_names)]
    pub fn enter_scope_node<'a>(&self, node: NodeView<'a>) -> NodeView<'a> {
        match self.family {
            AdapterFamily::Go => node.field("type").unwrap_or(node),
            AdapterFamily::Python => {
                if node.kind() == "decorated_definition" {
                    node.named_children()
                        .into_iter()
                        .find(|child| child.kind() == "class_definition")
                        .unwrap_or(node)
                } else {
                    node
                }
            }
            _ => node,
        }
    }

    /// Breadcrumb for an entity (go receivers, js object names, C++ qualifiers).
    #[must_use]
    pub fn scope_breadcrumb(&self, node: NodeView<'_>, breadcrumb: &[String]) -> Vec<String> {
        match self.family {
            AdapterFamily::Go => {
                if node.kind() == "method_declaration" {
                    if let Some(receiver) = extract_go_receiver_type(node) {
                        return breadcrumb
                            .iter()
                            .cloned()
                            .chain([receiver])
                            .collect();
                    }
                }
                breadcrumb.to_vec()
            }
            AdapterFamily::JsTs => {
                if let Some(object) = exported_object_variable_name(node) {
                    breadcrumb.iter().cloned().chain([object]).collect()
                } else {
                    breadcrumb.to_vec()
                }
            }
            AdapterFamily::CFamily => c_family_scope_breadcrumb(node, breadcrumb),
            _ => breadcrumb.to_vec(),
        }
    }

    /// Family-specific classification (`classifyNode`); `None` falls back
    /// to the generic [`classify_code_node`].
    #[must_use]
    pub fn classify_node(&self, node: NodeView<'_>) -> Option<CodeSymbolType> {
        match self.family {
            AdapterFamily::Go => classify_go_node(node),
            AdapterFamily::CFamily => classify_c_family_node(node),
            AdapterFamily::JsTs => classify_js_ts_node(node),
            _ => None,
        }
    }

    /// One-line signature (family-specific `extractSignature`).
    #[must_use]
    pub fn extract_signature(&self, node: NodeView<'_>) -> Option<String> {
        match self.family {
            AdapterFamily::Python => {
                let inner = inner_python_definition(node).unwrap_or(node);
                extract_generic_signature(inner)
            }
            AdapterFamily::JsTs => extract_js_ts_signature(node),
            _ => extract_generic_signature(node),
        }
    }

    /// Preceding doc comment (`extractDoc`).
    #[must_use]
    pub fn extract_doc(&self, node: NodeView<'_>) -> Option<String> {
        extract_preceding_doc(node)
    }

    /// Modifier set (family-specific `extractModifiers`).
    #[must_use]
    pub fn extract_modifiers(&self, node: NodeView<'_>) -> Vec<CodeEntityModifier> {
        match self.family {
            AdapterFamily::Python => extract_python_modifiers(node),
            AdapterFamily::Go => extract_go_modifiers(node),
            _ => extract_common_modifiers(node),
        }
    }
}

/// Generic node classification (port of `classifyCodeNode`).
#[must_use]
pub fn classify_code_node(node: NodeView<'_>, breadcrumb: &[String]) -> CodeSymbolType {
    let kind = node.kind();
    if kind == "decorated_definition" {
        let inner = node.named_children().into_iter().find(|child| {
            child.kind() == "function_definition" || child.kind() == "class_definition"
        });
        return inner.map_or(CodeSymbolType::Value, |node| classify_code_node(node, breadcrumb));
    }
    if (kind == "field_definition"
        || kind == "public_field_definition"
        || kind == "variable_declarator")
        && has_js_ts_function_value(node)
    {
        return CodeSymbolType::Function;
    }
    if kind.contains("method") || kind.contains("constructor") {
        return CodeSymbolType::Function;
    }
    if !breadcrumb.is_empty()
        && (kind.contains("function") || kind == "declaration" || kind == "function_item")
    {
        return CodeSymbolType::Function;
    }
    if kind.contains("function") {
        return CodeSymbolType::Function;
    }
    if kind == "declaration" || kind == "macro_type_specifier" {
        return CodeSymbolType::Function;
    }
    if kind.contains("class")
        || kind.contains("struct")
        || kind.contains("impl")
        || kind.contains("enum")
        || kind.contains("union")
        || kind.contains("record")
    {
        return CodeSymbolType::Class;
    }
    if kind.contains("interface") || kind.contains("protocol") || kind.contains("trait") {
        return CodeSymbolType::Interface;
    }
    if kind.contains("module") || kind.contains("namespace") || kind == "mod_item" {
        return CodeSymbolType::Module;
    }
    if kind.contains("alias")
        || kind.contains("typedef")
        || kind == "type_definition"
        || kind == "type_item"
    {
        return CodeSymbolType::Alias;
    }
    CodeSymbolType::Value
}

/// Structural symbol types (port of `STRUCTURAL_SYMBOL_TYPES`).
#[must_use]
pub fn is_structural_symbol(symbol: CodeSymbolType) -> bool {
    matches!(
        symbol,
        CodeSymbolType::Class | CodeSymbolType::Interface | CodeSymbolType::Module
    )
}

/// Generic signature: text before the body block, one line, trailing
/// `{`/`;` trimmed (port of `extractGenericSignature`).
#[must_use]
pub fn extract_generic_signature(node: NodeView<'_>) -> Option<String> {
    let body = node.field("body").or_else(|| {
        node.named_children().into_iter().find(|child| {
            matches!(
                child.kind(),
                "statement_block"
                    | "compound_statement"
                    | "block"
                    | "class_body"
                    | "declaration_list"
                    | "field_declaration_list"
            )
        })
    });
    let text = match body {
        Some(body) if body.node.start_byte() > node.node.start_byte() => {
            let end = body.node.start_byte() - node.node.start_byte();
            node.text().as_bytes().get(..end).and_then(|bytes| std::str::from_utf8(bytes).ok()).unwrap_or("").trim_end().to_string()
        }
        _ => first_non_empty_line(node.text()),
    };
    let normalized = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(['{', ';'])
        .trim()
        .to_string();
    if normalized.is_empty() { None } else { Some(normalized) }
}

/// First non-empty line, trimmed.
#[must_use]
pub fn first_non_empty_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_string()
}

/// Preceding doc comment (port of `extractPrecedingDoc`).
#[must_use]
pub fn extract_preceding_doc(node: NodeView<'_>) -> Option<String> {
    let mut comments = Vec::new();
    let mut sibling = node.prev_named_sibling();
    while let Some(view) = sibling {
        if !matches!(
            view.kind(),
            "comment" | "line_comment" | "block_comment" | "documentation_comment"
        ) {
            break;
        }
        comments.push(clean_comment_text(view.text()));
        sibling = view.prev_named_sibling();
    }
    comments.reverse();
    let doc = comments.join("\n").trim().to_string();
    if doc.is_empty() { None } else { Some(doc) }
}

fn clean_comment_text(text: &str) -> String {
    let mut out = text.to_string();
    if let Some(stripped) = out.strip_prefix("/**").or_else(|| out.strip_prefix("/*")) {
        out = stripped.to_string();
    }
    if let Some(stripped) = out.strip_suffix("*/") {
        out = stripped.to_string();
    }
    let mut lines = Vec::new();
    for line in out.lines() {
        let stripped = if let Some(rest) = line.strip_prefix("//") {
            // `//`, optional third `/`, one optional space (zvec parity).
            let rest = rest.strip_prefix('/').unwrap_or(rest);
            rest.strip_prefix(' ').unwrap_or(rest)
        } else if let Some(rest) = line.strip_prefix('#') {
            rest.strip_prefix(' ').unwrap_or(rest)
        } else {
            // `*`-led block-comment continuations only (zvec parity:
            // lines without `*` keep their indentation).
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix('*') {
                rest.strip_prefix(' ').unwrap_or(rest)
            } else {
                line
            }
        };
        lines.push(stripped);
    }
    lines.join("\n").trim().to_string()
}

/// Common modifiers from the signature tokens (port of
/// `extractCommonModifiers`); `pub` normalizes to `public`.
#[must_use]
pub fn extract_common_modifiers(node: NodeView<'_>) -> Vec<CodeEntityModifier> {
    let mut modifiers = Vec::new();
    let push = |m: CodeEntityModifier, list: &mut Vec<CodeEntityModifier>| {
        if !list.contains(&m) {
            list.push(m);
        }
    };
    if node.closest_ancestor("export_statement").is_some() {
        push(CodeEntityModifier::Exported, &mut modifiers);
    }
    let signature =
        extract_generic_signature(node).unwrap_or_else(|| first_non_empty_line(node.text()));
    for token in signature.split(|c: char| !c.is_alphanumeric() && c != '_') {
        match token {
            "public" | "pub" => push(CodeEntityModifier::Public, &mut modifiers),
            "private" => push(CodeEntityModifier::Private, &mut modifiers),
            "protected" => push(CodeEntityModifier::Protected, &mut modifiers),
            "internal" => push(CodeEntityModifier::Internal, &mut modifiers),
            "static" => push(CodeEntityModifier::Static, &mut modifiers),
            "async" => push(CodeEntityModifier::Async, &mut modifiers),
            _ => {}
        }
    }
    modifiers
}

// -- Go ---------------------------------------------------------------------

fn extract_go_receiver_type(node: NodeView<'_>) -> Option<String> {
    let receiver = node.field("receiver")?;
    let text = receiver.text();
    let end = text.rfind(')')?;
    let before = text[..end].trim_end();
    let name: String = before
        .trim_start_matches('*')
        .trim()
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    if name.is_empty() || !name.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_') {
        return None;
    }
    Some(name)
}

fn classify_go_node(node: NodeView<'_>) -> Option<CodeSymbolType> {
    match node.kind() {
        "type_alias" => Some(CodeSymbolType::Alias),
        "type_spec" => {
            let type_kind = node.field("type").map(NodeView::kind);
            match type_kind {
                Some("interface_type") => Some(CodeSymbolType::Interface),
                Some("struct_type") => Some(CodeSymbolType::Class),
                _ => Some(CodeSymbolType::Alias),
            }
        }
        "method_elem" | "method_spec" => Some(CodeSymbolType::Function),
        _ => None,
    }
}

fn extract_go_modifiers(node: NodeView<'_>) -> Vec<CodeEntityModifier> {
    let name = node.field("name").map_or("", NodeView::text);
    if name.chars().next().is_some_and(char::is_uppercase) {
        vec![CodeEntityModifier::Exported]
    } else {
        Vec::new()
    }
}

// -- Python ------------------------------------------------------------------

fn inner_python_definition(node: NodeView<'_>) -> Option<NodeView<'_>> {
    if node.kind() != "decorated_definition" {
        return None;
    }
    node.named_children().into_iter().find(|child| {
        child.kind() == "function_definition" || child.kind() == "class_definition"
    })
}

fn extract_python_modifiers(node: NodeView<'_>) -> Vec<CodeEntityModifier> {
    let mut modifiers = extract_common_modifiers(node);
    let mut push = |m: CodeEntityModifier| {
        if !modifiers.contains(&m) {
            modifiers.push(m);
        }
    };
    let text = node.text();
    if text.lines().any(|line| {
        let words: Vec<&str> = line.split_whitespace().take(2).collect();
        words == ["async", "def"]
    }) {
        push(CodeEntityModifier::Async);
    }
    if text.lines().any(|line| line.trim_start().starts_with("@staticmethod")) {
        push(CodeEntityModifier::Static);
    }
    modifiers
}

// -- C family -----------------------------------------------------------------

const C_FAMILY_FUNCTION_TYPES: &[&str] = &[
    "declaration",
    "field_declaration",
    "function_definition",
    "macro_type_specifier",
];

const C_FAMILY_FUNCTION_DECLARATION_TYPES: &[&str] = &["declaration", "field_declaration"];

fn should_index_c_entity(node: NodeView<'_>) -> bool {
    if !C_FAMILY_FUNCTION_DECLARATION_TYPES.contains(&node.kind()) {
        if node.kind() == "macro_type_specifier" {
            return extract_c_function_name(node.text()).is_some();
        }
        return true;
    }
    node.find_descendant_by_type("function_declarator").is_some()
}

/// Import-node tables per family (tree-sitter node kinds for the pinned
/// workspace grammar versions; kept next to the entity tables above).
fn is_import_node(family: AdapterFamily, kind: &str) -> bool {
    match family {
        AdapterFamily::Rust => matches!(kind, "use_declaration" | "mod_item"),
        AdapterFamily::Python => matches!(kind, "import_statement" | "import_from_statement"),
        AdapterFamily::Go => matches!(kind, "import_spec" | "import_declaration"),
        AdapterFamily::CFamily => matches!(kind, "preproc_include"),
        AdapterFamily::NameField => matches!(kind, "import_declaration"),
        AdapterFamily::JsTs => matches!(
            kind,
            "import_statement" | "export_statement" | "import_require_clause"
        ),
    }
}

/// Extract the raw specifiers from an import node. Multi-leaf nodes
/// (`import os, sys`) emit one specifier per leaf; single-source nodes
/// (JS `import {a} from "mod"`) emit only the module source — imported
/// names are symbols, never edges.
fn import_specifier_texts(family: AdapterFamily, node: NodeView<'_>) -> Vec<String> {
    // Rust `mod foo;` (bodyless) declares a module file, not a use path.
    if family == AdapterFamily::Rust && node.kind() == "mod_item" {
        if node.field("body").is_some() {
            return Vec::new();
        }
        return node
            .field("name")
            .map(|name| format!("mod:{}", name.text()))
            .into_iter()
            .collect();
    }
    // Python `import a, b [as c]` — one specifier per direct leaf.
    if family == AdapterFamily::Python && node.kind() == "import_statement" {
        return node
            .named_children()
            .into_iter()
            .filter(|child| {
                matches!(child.kind(), "dotted_name" | "aliased_import" | "wildcard_import")
            })
            .filter_map(|child| first_leaf_text(child))
            .collect();
    }
    // Python `from [.]module import names` — the module leaf only; the
    // wildcard/import-list leaves are symbols, never edges.
    if family == AdapterFamily::Python && node.kind() == "import_from_statement" {
        return node
            .named_children()
            .into_iter()
            .find(|child| matches!(child.kind(), "dotted_name" | "relative_import"))
            .and_then(first_leaf_text)
            .into_iter()
            .collect();
    }
    // JS/TS re-export (`export ... from "mod"`): only the `source`
    // field is an edge. A plain `export function/class/const` has no
    // source — its body strings (literals, never modules) must not
    // become edges.
    if family == AdapterFamily::JsTs && node.kind() == "export_statement" {
        return node
            .field("source")
            .map(|source| unquote(source.text()))
            .filter(|text| !text.is_empty())
            .into_iter()
            .collect();
    }
    // JS/TS — only the module source string is an edge.
    if family == AdapterFamily::JsTs {
        let mut stack = vec![node];
        while let Some(view) = stack.pop() {
            match view.kind() {
                "string" | "string_fragment" => {
                    let text = unquote(view.text());
                    if !text.is_empty() {
                        return vec![text];
                    }
                }
                _ => stack.extend(view.named_children()),
            }
        }
        return Vec::new();
    }
    first_leaf_text(node).into_iter().collect()
}

/// First string-like or dotted-name leaf under a node (depth-first).
fn first_leaf_text(node: NodeView<'_>) -> Option<String> {
    let mut stack = vec![node];
    while let Some(view) = stack.pop() {
        match view.kind() {
            "string" | "string_literal" | "interpreted_string_literal"
            | "system_lib_string" | "string_fragment" => {
                let text = unquote(view.text());
                if !text.is_empty() {
                    return Some(text);
                }
            }
            "scoped_identifier" | "dotted_name" | "identifier" => {
                let text = view.text().trim().to_string();
                if !text.is_empty() && !text.contains(char::is_whitespace) {
                    return Some(text);
                }
                stack.extend(view.named_children());
            }
            _ => stack.extend(view.named_children()),
        }
    }
    let fallback = node.text().trim().to_string();
    if fallback.is_empty() {
        return None;
    }
    Some(fallback)
}

/// Strip one layer of quotes/backticks/angle brackets from an include or
/// module string.
fn unquote(text: &str) -> String {
    let trimmed = text.trim();
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' && last == b'"')
            || (first == b'\'' && last == b'\'')
            || (first == b'`' && last == b'`')
            || (first == b'<' && last == b'>')
        {
            return trimmed[1..trimmed.len() - 1].to_string();
        }
    }
    trimmed.to_string()
}

fn extract_c_name(node: NodeView<'_>) -> Option<String> {
    if C_FAMILY_FUNCTION_TYPES.contains(&node.kind()) {
        let name = extract_raw_c_function_name(node)?;
        return Some(last_qualified_part(&name));
    }
    if let Some(name) = node.field("name") {
        return Some(find_identifier_leaf(name).map_or_else(
            || name.text().to_string(),
            |leaf| leaf.text().to_string(),
        ));
    }
    if node.kind() == "type_definition" {
        return node
            .field("declarator")
            .and_then(find_identifier_leaf)
            .map(|leaf| leaf.text().to_string());
    }
    None
}

fn extract_raw_c_function_name(node: NodeView<'_>) -> Option<String> {
    let declarator = node
        .field("declarator")
        .or_else(|| node.find_descendant_by_type("function_declarator"));
    let name = declarator.and_then(find_identifier_leaf).map(|leaf| leaf.text().to_string());
    match name {
        Some(name) if is_simple_c_identifier(&name) => Some(name),
        _ => extract_c_function_name(declarator.map_or(node.text(), NodeView::text)),
    }
}

fn is_simple_c_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '~')
        && value.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '~')
}

fn extract_c_function_name(text: &str) -> Option<String> {
    let mut last = None;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'(' {
            let mut j = i;
            while j > 0 && (bytes[j - 1].is_ascii_alphanumeric() || matches!(bytes[j - 1], b'_' | b'~' | b':')) {
                j -= 1;
            }
            let candidate = text[j..i].trim().to_string();
            if !candidate.is_empty() {
                last = Some(candidate);
            }
        }
        i += 1;
    }
    last
}

fn last_qualified_part(name: &str) -> String {
    name.split("::").filter(|p| !p.is_empty()).last().unwrap_or(name).to_string()
}

fn qualifier_parts(name: &str) -> Vec<String> {
    let parts: Vec<&str> = name.split("::").filter(|p| !p.is_empty()).collect();
    if parts.len() > 1 {
        parts[..parts.len() - 1].iter().map(ToString::to_string).collect()
    } else {
        Vec::new()
    }
}

fn find_identifier_leaf(node: NodeView<'_>) -> Option<NodeView<'_>> {
    const WRAPPERS: &[&str] = &[
        "array_declarator",
        "function_declarator",
        "init_declarator",
        "parenthesized_declarator",
        "pointer_declarator",
        "reference_declarator",
    ];
    let mut current = Some(node);
    for _ in 0..16 {
        let view = current?;
        if view.kind() == "identifier" || view.kind().ends_with("_identifier") {
            return Some(view);
        }
        if view.kind() == "destructor_name" || view.kind() == "operator_name" {
            return Some(view);
        }
        if !WRAPPERS.contains(&view.kind()) {
            // Descend into the first named child that can hold a name.
            current = view.named_children().into_iter().next();
            current?;
            continue;
        }
        current = view.named_children().into_iter().next();
    }
    None
}

fn c_family_scope_breadcrumb(node: NodeView<'_>, breadcrumb: &[String]) -> Vec<String> {
    if !C_FAMILY_FUNCTION_TYPES.contains(&node.kind()) {
        return breadcrumb.to_vec();
    }
    let Some(name) = extract_raw_c_function_name(node) else {
        return breadcrumb.to_vec();
    };
    let qualifier = qualifier_parts(&name);
    if qualifier.is_empty() {
        return breadcrumb.to_vec();
    }
    let mut out = breadcrumb.to_vec();
    let skip_first = !out.is_empty() && out.last() == qualifier.first();
    for part in qualifier.iter().skip(usize::from(skip_first)) {
        out.push(part.clone());
    }
    out
}

fn classify_c_family_node(node: NodeView<'_>) -> Option<CodeSymbolType> {
    if node.kind() == "type_definition" {
        return Some(if typedef_wraps_class_like_body(node) {
            CodeSymbolType::Class
        } else {
            CodeSymbolType::Alias
        });
    }
    if node.kind() == "alias_declaration" {
        return Some(CodeSymbolType::Alias);
    }
    if node.kind() == "field_declaration"
        && node.find_descendant_by_type("function_declarator").is_some()
    {
        return Some(CodeSymbolType::Function);
    }
    None
}

fn typedef_wraps_class_like_body(node: NodeView<'_>) -> bool {
    ["struct_specifier", "union_specifier", "enum_specifier"]
        .iter()
        .any(|kind| {
            node.find_descendant_by_type(kind)
                .and_then(|child| child.field("body"))
                .is_some()
        })
}

// -- Name-field (Java) --------------------------------------------------------

fn name_field(node: NodeView<'_>) -> Option<String> {
    node.field("name")
        .map(|n| n.text().to_string())
        .or_else(|| {
            node.named_children()
                .into_iter()
                .find(|child| {
                    matches!(child.kind(), "identifier" | "property_identifier" | "type_identifier")
                })
                .map(|child| child.text().to_string())
        })
}

// -- JavaScript/TypeScript -----------------------------------------------------

const JS_TS_FUNCTION_VALUE_DECLARATION_TYPES: &[&str] = &[
    "field_definition",
    "public_field_definition",
    "variable_declarator",
];

const JS_TS_FUNCTION_VALUE_TYPES: &[&str] = &["arrow_function", "function_expression"];

fn should_index_js_ts_entity(node: NodeView<'_>) -> bool {
    if node.kind() == "method_definition" && is_object_member(node) {
        return false;
    }
    if node.kind() == "pair" {
        return has_function_value(node) && exported_object_variable_name(node).is_some();
    }
    if !JS_TS_FUNCTION_VALUE_DECLARATION_TYPES.contains(&node.kind()) {
        return true;
    }
    if node.kind() == "variable_declarator" && !exported_object_function_entities(node).is_empty() {
        return true;
    }
    has_function_value(node)
}

fn has_js_ts_function_value(node: NodeView<'_>) -> bool {
    has_function_value(node)
}

fn has_function_value(node: NodeView<'_>) -> bool {
    let value = node.field("value").or_else(|| {
        node.named_children()
            .into_iter()
            .find(|child| JS_TS_FUNCTION_VALUE_TYPES.contains(&child.kind()))
    });
    value.is_some_and(contains_function_value)
}

fn contains_function_value(node: NodeView<'_>) -> bool {
    if JS_TS_FUNCTION_VALUE_TYPES.contains(&node.kind()) {
        return true;
    }
    if node.kind() != "call_expression" && node.kind() != "arguments" {
        return false;
    }
    node.named_children().iter().any(|child| contains_function_value(*child))
}

fn exported_object_function_entities(node: NodeView<'_>) -> Vec<NodeView<'_>> {
    if !is_exported_variable_declarator(node) {
        return Vec::new();
    }
    let Some(value) = node.field("value") else {
        return Vec::new();
    };
    if value.kind() != "object" && value.kind() != "object_expression" {
        return Vec::new();
    }
    value
        .named_children()
        .into_iter()
        .filter(|child| {
            (child.kind() == "pair" && has_function_value(*child))
                || child.kind() == "method_definition"
        })
        .collect()
}

fn exported_object_variable_name(node: NodeView<'_>) -> Option<String> {
    let object = node
        .closest_ancestor("object")
        .or_else(|| node.closest_ancestor("object_expression"))?;
    let variable = object.parent()?;
    if variable.kind() != "variable_declarator" || !is_exported_variable_declarator(variable) {
        return None;
    }
    extract_js_ts_name(variable)
}

fn is_exported_variable_declarator(node: NodeView<'_>) -> bool {
    node.kind() == "variable_declarator"
        && node.closest_ancestor("export_statement").is_some()
}

fn is_object_member(node: NodeView<'_>) -> bool {
    node.parent()
        .is_some_and(|parent| parent.kind() == "object" || parent.kind() == "object_expression")
}

fn extract_js_ts_name(node: NodeView<'_>) -> Option<String> {
    if node.kind() == "pair" {
        return node.field("key").map(|key| {
            key.text().trim_matches(['\'', '"', '`']).to_string()
        });
    }
    node.field("name")
        .map(|n| n.text().to_string())
        .or_else(|| {
            node.named_children()
                .into_iter()
                .find(|child| {
                    matches!(child.kind(), "identifier" | "property_identifier" | "type_identifier")
                })
                .map(|child| child.text().to_string())
        })
}

fn extract_js_ts_signature(node: NodeView<'_>) -> Option<String> {
    if node.kind() == "pair" {
        let key = extract_js_ts_name(node);
        let value_signature = node.field("value").and_then(extract_generic_signature);
        if let (Some(key), Some(signature)) = (key, value_signature) {
            return Some(format!("{key}: {signature}"));
        }
        return extract_generic_signature(node);
    }
    extract_generic_signature(node)
}

fn classify_js_ts_node(node: NodeView<'_>) -> Option<CodeSymbolType> {
    if node.kind() == "pair" || JS_TS_FUNCTION_VALUE_DECLARATION_TYPES.contains(&node.kind()) {
        // Non-function values fall through to the generic classifier.
        return has_function_value(node).then_some(CodeSymbolType::Function);
    }
    None
}
