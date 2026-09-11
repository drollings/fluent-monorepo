//! P2 code extraction (port of `extraction/code/extractor.ts`):
//! adapter-driven entity walk, one fragment per member (outline major +
//! grouped source windows for oversize entities), plain-text fallback,
//! component script-block remap. Ranges carry byte offsets; budgets
//! count UTF-16 units (zvec parity); windows never split characters.

use std::collections::HashSet;

use super::{
    ChunkOptions, ExtractError, ExtractSource, FragmentMetadata, FragmentRange, PreparedFragment,
    make_entity_id, utf16_len, utf16_slice,
    adapter::{NodeView, TsAdapter, classify_code_node, is_structural_symbol, resolve_adapter},
    text::extract_plain_text_fragments,
    vector_content::chunk_options_for_metadata,
};
use crate::ast_parser::AstParser;
use crate::zg_constants::{
    OUTLINE_HEADER_MAX_CHARS, OUTLINE_HEADER_MAX_LINES, OUTLINE_MAX_CALLS, OUTLINE_MAX_LINE_CHARS,
    OUTLINE_MAX_MEMBERS, OUTLINE_WALK_MAX_DEPTH,
};
use crate::zg_types::{CodeEntityModifier, FileKind};

/// Code fragment extractor (one impl; formats dispatch via the adapter
/// registry, never a match).
pub struct CodeExtractor;

impl CodeExtractor {
    /// Extract-then-prepare (port of `extractForIndexing`).
    pub fn extract_for_indexing(
        &self,
        parser: &mut AstParser,
        source: &ExtractSource,
        options: ChunkOptions,
    ) -> Result<Vec<PreparedFragment>, ExtractError> {
        Ok(self.extract_with_harvest(parser, source, options)?.0)
    }

    /// Extract fragments plus the file's graph inputs from a single
    /// parse and walk (one tree, one entity pass — never a second parse
    /// for the graph tables).
    pub fn extract_with_harvest(
        &self,
        parser: &mut AstParser,
        source: &ExtractSource,
        options: ChunkOptions,
    ) -> Result<(Vec<PreparedFragment>, HarvestedFile), ExtractError> {
        if source.kind != FileKind::Code {
            return Ok((Vec::new(), HarvestedFile::default()));
        }
        if super::adapter::COMPONENT_CODE_FORMATS.contains(&source.format.as_str()) {
            let blocks = extract_script_blocks(parser, source, options)?;
            if !blocks.is_empty() {
                return Ok((blocks, HarvestedFile::default()));
            }
            return Ok((fallback_fragments(source, options)?, HarvestedFile::default()));
        }
        let Some(adapter) = resolve_adapter(&source.format) else {
            return Ok((fallback_fragments(source, options)?, HarvestedFile::default()));
        };
        if !AstParser::has_grammar(&source.format) {
            return Ok((fallback_fragments(source, options)?, HarvestedFile::default()));
        }
        let tree = parser
            .parse_tree(&source.format, &source.text)
            .map_err(|e| ExtractError::Parse(e.to_string()))?;
        let source_bytes = source.text.as_bytes();
        let root = NodeView { node: tree.root_node(), source: source_bytes };
        let mut entities = Vec::new();
        walk_code_node(root, adapter, &[], &mut entities);
        // Imports ride the same tree — an entity-less file (pure
        // re-exports) still contributes its edges on the fallback path.
        let imports = adapter.import_specifiers(root);
        if entities.is_empty() {
            return Ok((
                fallback_fragments(source, options)?,
                HarvestedFile { symbols: Vec::new(), imports },
            ));
        }
        let mut fragments = Vec::new();
        for entity in &entities {
            append_entity(source, adapter, entity, options, &mut fragments);
        }
        if fragments.is_empty() {
            return Ok((fallback_fragments(source, options)?, HarvestedFile::default()));
        }
        let harvested = HarvestedFile {
            symbols: entities
                .into_iter()
                .map(|entity| HarvestedSymbol {
                    name: entity.name,
                    scope: entity.breadcrumb,
                    calls: entity.calls,
                })
                .collect(),
            imports,
        };
        Ok((fragments, harvested))
    }
}

/// A harvested code symbol (graph edges; no fragments, no embeddings).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarvestedSymbol {
    /// Declared name, if any.
    pub name: Option<String>,
    /// Lexical scope breadcrumb.
    pub scope: Vec<String>,
    /// Called symbol names (raw, unresolved).
    pub calls: Vec<String>,
}

/// Harvested per-file graph inputs: symbols with call names plus raw
/// import specifiers. Reuses the entity walker (same trees, same
/// adapters) — never a second parser.
#[derive(Debug, Clone, Default)]
pub struct HarvestedFile {
    /// Defined symbols.
    pub symbols: Vec<HarvestedSymbol>,
    /// Raw import specifiers.
    pub imports: Vec<String>,
}

/// Harvest graph inputs for one code file (P3 `graph_index` consumer).
pub fn harvest_file_entities(
    parser: &mut AstParser,
    source: &ExtractSource,
) -> Result<HarvestedFile, ExtractError> {
    let mut harvested = HarvestedFile::default();
    if source.kind != FileKind::Code {
        return Ok(harvested);
    }
    let Some(adapter) = resolve_adapter(&source.format) else {
        return Ok(harvested);
    };
    if !AstParser::has_grammar(&source.format) {
        return Ok(harvested);
    }
    let tree = parser
        .parse_tree(&source.format, &source.text)
        .map_err(|e| ExtractError::Parse(e.to_string()))?;
    let root = NodeView { node: tree.root_node(), source: source.text.as_bytes() };
    let mut entities = Vec::new();
    walk_code_node(root, adapter, &[], &mut entities);
    harvested.symbols = entities
        .into_iter()
        .map(|entity| HarvestedSymbol {
            name: entity.name,
            scope: entity.breadcrumb,
            calls: entity.calls,
        })
        .collect();
    harvested.imports = adapter.import_specifiers(root);
    Ok(harvested)
}

fn fallback_fragments(
    source: &ExtractSource,
    options: ChunkOptions,
) -> Result<Vec<PreparedFragment>, ExtractError> {
    extract_plain_text_fragments(source, options)
}

/// A statement span for window grouping (bytes into the file source).
#[derive(Debug, Clone)]
struct StmtSpan {
    start_byte: usize,
    end_byte: usize,
    start_row: usize,
    end_row: usize,
}

/// An extracted code entity with owned strings (collected in one borrowed
/// tree walk; fragments render from owned data).
struct OwnedEntity {
    name: Option<String>,
    symbol_type: crate::zg_types::CodeSymbolType,
    breadcrumb: Vec<String>,
    signature: Option<String>,
    doc: Option<String>,
    modifiers: Vec<CodeEntityModifier>,
    node_kind: String,
    text: String,
    start_byte: usize,
    end_byte: usize,
    start_row: usize,
    end_row: usize,
    members: Vec<OutlineMember>,
    calls: Vec<String>,
    statements: Vec<StmtSpan>,
}

struct OutlineMember {
    symbol_type: crate::zg_types::CodeSymbolType,
    name: Option<String>,
    signature: Option<String>,
}

fn walk_code_node(
    node: NodeView<'_>,
    adapter: &TsAdapter,
    breadcrumb: &[String],
    out: &mut Vec<OwnedEntity>,
) {
    for child in node.children_all() {
        let is_scope =
            adapter.scope_types.contains(&child.kind()) && adapter.should_enter_scope(child);
        let is_entity =
            adapter.entity_types.contains(&child.kind()) && adapter.should_index_entity(child);
        if is_entity {
            for resolved in adapter.resolve_entities(child) {
                let name = adapter.extract_name(resolved);
                let entity_breadcrumb = adapter.scope_breadcrumb(resolved, breadcrumb);
                let symbol_type = adapter
                    .classify_node(resolved)
                    .unwrap_or_else(|| classify_code_node(resolved, &entity_breadcrumb));
                let body = resolved.field("body").unwrap_or(resolved);
                let statements = body
                    .named_children()
                    .into_iter()
                    .map(|stmt| StmtSpan {
                        start_byte: stmt.node.start_byte(),
                        end_byte: stmt.node.end_byte(),
                        start_row: stmt.node.start_position().row,
                        end_row: stmt.node.end_position().row,
                    })
                    .collect();
                out.push(OwnedEntity {
                    name,
                    symbol_type,
                    breadcrumb: entity_breadcrumb.clone(),
                    signature: adapter.extract_signature(resolved),
                    doc: adapter.extract_doc(resolved),
                    modifiers: adapter.extract_modifiers(resolved),
                    node_kind: resolved.kind().to_string(),
                    text: resolved.text().to_string(),
                    start_byte: resolved.node.start_byte(),
                    end_byte: resolved.node.end_byte(),
                    start_row: resolved.node.start_position().row,
                    end_row: resolved.node.end_position().row,
                    members: collect_members(adapter, resolved, &entity_breadcrumb),
                    calls: collect_call_names(resolved),
                    statements,
                });
            }
        }
        if is_scope {
            let scope_node = adapter.enter_scope_node(child);
            let mut next = breadcrumb.to_vec();
            if let Some(name) = adapter.extract_name(child) {
                next.push(name);
            }
            walk_code_node(scope_node, adapter, &next, out);
            continue;
        }
        if !is_entity {
            walk_code_node(child, adapter, breadcrumb, out);
        }
    }
}

fn append_entity(
    source: &ExtractSource,
    _adapter: &TsAdapter,
    entity: &OwnedEntity,
    options: ChunkOptions,
    fragments: &mut Vec<PreparedFragment>,
) {
    let metadata = Some(code_metadata(entity));
    let (content_max, content_overlap) = chunk_options_for_metadata(
        options.max_chunk_chars,
        options.chunk_overlap_chars,
        &metadata,
    );
    if utf16_len(&entity.text) <= content_max {
        fragments.push(PreparedFragment {
            id: make_entity_id(&source.file_id, fragments.len()),
            content_text: entity.text.clone(),
            embedding_text: None,
            range: FragmentRange {
                start_line: entity.start_row + 1,
                end_line: entity.end_row + 1,
                start_offset: entity.start_byte,
                end_offset: entity.end_byte,
            },
            group: None,
            metadata,
            calls: entity.calls.clone(),
        });
        return;
    }
    let major_id = make_entity_id(&source.file_id, fragments.len());
    fragments.push(PreparedFragment {
        id: major_id.clone(),
        content_text: truncate_outline(
            &code_entity_outline(entity, content_max),
            content_max,
        ),
        embedding_text: None,
        range: FragmentRange {
            start_line: entity.start_row + 1,
            end_line: entity.end_row + 1,
            start_offset: entity.start_byte,
            end_offset: entity.end_byte,
        },
        group: Some(major_id.clone()),
        metadata,
        calls: entity.calls.clone(),
    });
    for window in split_large_entity(entity, &source.text, content_max, content_overlap) {
        let id = make_entity_id(&source.file_id, fragments.len());
        fragments.push(PreparedFragment {
            id,
            content_text: window.text,
            embedding_text: window.embedding_text,
            range: window.range,
            group: Some(major_id.clone()),
            metadata: Some(code_metadata(entity)),
            calls: entity.calls.clone(),
        });
    }
}

fn code_metadata(entity: &OwnedEntity) -> FragmentMetadata {
    FragmentMetadata::Code {
        symbol_type: entity.symbol_type,
        symbol_name: entity.name.clone(),
        scope: if entity.breadcrumb.is_empty() {
            None
        } else {
            Some(entity.breadcrumb.join("::"))
        },
        node_type: entity.node_kind.clone(),
        signature: entity.signature.clone(),
        doc: entity.doc.clone(),
        modifiers: entity
            .modifiers
            .iter()
            .map(|m| match m {
                CodeEntityModifier::Exported => "exported",
                CodeEntityModifier::Async => "async",
                CodeEntityModifier::Static => "static",
                CodeEntityModifier::Public => "public",
                CodeEntityModifier::Private => "private",
                CodeEntityModifier::Protected => "protected",
                CodeEntityModifier::Internal => "internal",
            })
            .map(ToString::to_string)
            .collect(),
    }
}

// -- Outlines ---------------------------------------------------------------

fn code_entity_outline(entity: &OwnedEntity, max_chars: usize) -> String {
    let header = extract_code_header(&entity.text);
    let mut lines = vec![if header.is_empty() {
        entity.name.clone().unwrap_or_else(|| format!("{:?}", entity.symbol_type).to_lowercase())
    } else {
        header
    }];
    if is_structural_symbol(entity.symbol_type) {
        if !entity.members.is_empty() {
            lines.push(String::new());
            lines.push("members:".to_string());
            for member in &entity.members {
                lines.push(format!("- {}", format_outline_member(member)));
            }
        }
    } else if entity.symbol_type == crate::zg_types::CodeSymbolType::Function && !entity.calls.is_empty() {
        lines.push(String::new());
        lines.push(format!("calls: {}", entity.calls.join(", ")));
    }
    truncate_outline(lines.join("\n").trim(), max_chars)
}

fn extract_code_header(text: &str) -> String {
    let mut lines = Vec::new();
    for line in text.split('\n') {
        lines.push(line);
        if line.contains('{') || lines.len() >= OUTLINE_HEADER_MAX_LINES {
            break;
        }
    }
    let header = lines.join("\n").trim().to_string();
    if header.len() > OUTLINE_HEADER_MAX_CHARS {
        format!("{}\n...", header[..OUTLINE_HEADER_MAX_CHARS].trim_end())
    } else {
        header
    }
}

fn format_outline_member(member: &OutlineMember) -> String {
    let name = member.name.clone().unwrap_or_default();
    let signature = member
        .signature
        .as_deref()
        .map(|s| truncate_outline_line(&one_line(s)))
        .unwrap_or_default();
    if !signature.is_empty() {
        let type_name = symbol_type_name(member.symbol_type);
        if !name.is_empty() && !signature.contains(&name) {
            return format!("{type_name} {name}: {signature}");
        }
        return format!("{type_name} {signature}");
    }
    if !name.is_empty() {
        return format!("{} {name}", symbol_type_name(member.symbol_type));
    }
    symbol_type_name(member.symbol_type).to_string()
}

fn symbol_type_name(symbol: crate::zg_types::CodeSymbolType) -> &'static str {
    match symbol {
        crate::zg_types::CodeSymbolType::Module => "module",
        crate::zg_types::CodeSymbolType::Class => "class",
        crate::zg_types::CodeSymbolType::Interface => "interface",
        crate::zg_types::CodeSymbolType::Function => "function",
        crate::zg_types::CodeSymbolType::Value => "value",
        crate::zg_types::CodeSymbolType::Alias => "alias",
    }
}

fn truncate_outline(outline: &str, max_chars: usize) -> String {
    if utf16_len(outline) <= max_chars {
        return outline.to_string();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    format!("{}...", utf16_prefix_trim_end(outline, max_chars - 3))
}

fn utf16_prefix_trim_end(text: &str, max_units: usize) -> String {
    super::utf16_slice(text, 0, max_units).trim_end().to_string()
}

fn truncate_outline_line(value: &str) -> String {
    if utf16_len(value) > OUTLINE_MAX_LINE_CHARS {
        format!(
            "{}...",
            super::utf16_slice(value, 0, OUTLINE_MAX_LINE_CHARS - 3).trim_end()
        )
    } else {
        value.to_string()
    }
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn collect_members(
    adapter: &TsAdapter,
    entity: NodeView<'_>,
    breadcrumb: &[String],
) -> Vec<OutlineMember> {
    struct Visitor<'a> {
        adapter: &'a TsAdapter,
        entity_kind: &'static str,
        entity_start: usize,
        entity_end: usize,
        breadcrumb: Vec<String>,
        members: Vec<OutlineMember>,
        seen: HashSet<String>,
    }
    fn visit(view: NodeView<'_>, visitor: &mut Visitor<'_>, depth: u32) {
        if visitor.members.len() >= OUTLINE_MAX_MEMBERS || depth > OUTLINE_WALK_MAX_DEPTH {
            return;
        }
        let is_self = view.kind() == visitor.entity_kind
            && view.node.start_byte() == visitor.entity_start
            && view.node.end_byte() == visitor.entity_end;
        if !is_self && visitor.adapter.entity_types.contains(&view.kind()) {
            if visitor.adapter.should_index_entity(view) {
                for resolved in visitor.adapter.resolve_entities(view) {
                    if visitor.members.len() >= OUTLINE_MAX_MEMBERS {
                        break;
                    }
                    if resolved.kind() == visitor.entity_kind
                        && resolved.node.start_byte() == visitor.entity_start
                        && resolved.node.end_byte() == visitor.entity_end
                    {
                        continue;
                    }
                    let name = visitor.adapter.extract_name(resolved);
                    let symbol_type = visitor
                        .adapter
                        .classify_node(resolved)
                        .unwrap_or_else(|| classify_code_node(resolved, &visitor.breadcrumb));
                    let signature = visitor.adapter.extract_signature(resolved);
                    let key = format!(
                        "{symbol_type:?}:{}:{}:{}",
                        name.as_deref().unwrap_or(""),
                        signature.as_deref().unwrap_or(""),
                        resolved.node.start_byte()
                    );
                    if visitor.seen.insert(key) {
                        visitor.members.push(OutlineMember { symbol_type, name, signature });
                    }
                }
            }
            return;
        }
        let mut cursor = view.node.walk();
        for child in view.node.named_children(&mut cursor) {
            visit(NodeView { node: child, source: view.source }, visitor, depth + 1);
        }
    }
    let mut visitor = Visitor {
        adapter,
        entity_kind: entity.kind(),
        entity_start: entity.node.start_byte(),
        entity_end: entity.node.end_byte(),
        breadcrumb: breadcrumb.to_vec(),
        members: Vec::new(),
        seen: HashSet::new(),
    };
    // Member classification uses the entity breadcrumb (zvec parity).
    visit(entity, &mut visitor, 0);
    visitor.members
}

fn collect_call_names(node: NodeView<'_>) -> Vec<String> {
    const CALL_TYPES: &[&str] = &[
        "call",
        "call_expression",
        "function_call_expression",
        "method_invocation",
        "object_creation_expression",
        "new_expression",
    ];
    let mut calls = Vec::new();
    let mut seen = HashSet::new();
    fn visit(view: NodeView<'_>, calls: &mut Vec<String>, seen: &mut HashSet<String>) {
        if calls.len() >= OUTLINE_MAX_CALLS {
            return;
        }
        if CALL_TYPES.contains(&view.kind()) {
            if let Some(name) = extract_call_name(view) {
                if seen.insert(name.clone()) {
                    calls.push(name);
                }
            }
        }
        for child in view.named_children() {
            visit(child, calls, seen);
        }
    }
    visit(node, &mut calls, &mut seen);
    calls
}

fn extract_call_name(node: NodeView<'_>) -> Option<String> {
    let target = node
        .field("function")
        .or_else(|| node.field("name"))
        .or_else(|| node.field("constructor"))
        .or_else(|| node.field("type"))
        .or_else(|| node.named_children().into_iter().next())?;
    normalize_call_name(target.text())
}

fn normalize_call_name(value: &str) -> Option<String> {
    let cleaned = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();
    let cleaned = cleaned.strip_prefix("new ").unwrap_or(&cleaned).trim().to_string();
    if cleaned.is_empty()
        || utf16_len(&cleaned) > OUTLINE_MAX_LINE_CHARS
        || cleaned.contains(['\n', '\r'])
        || !cleaned
            .chars()
            .any(|c| c.is_alphanumeric() || c == '_' || c == '$')
    {
        return None;
    }
    Some(cleaned)
}

// -- Statement windows --------------------------------------------------------

struct CodeWindow {
    text: String,
    embedding_text: Option<String>,
    range: FragmentRange,
}

fn split_large_entity(
    entity: &OwnedEntity,
    source: &str,
    max_chars: usize,
    overlap_chars: usize,
) -> Vec<CodeWindow> {
    // Statement-grouped windows (zvec `splitLargeNode` parity): the body
    // statements group into budgeted slices; oversize statements fall back
    // to line windows. Window text is the source slice (gaps preserved);
    // the embedding text joins statements (gaps compacted).
    if entity.statements.len() <= 1 {
        return split_text_by_lines_code(
            &entity.text,
            max_chars,
            entity.start_row + 1,
            entity.start_byte,
            overlap_chars,
        );
    }
    let mut windows = Vec::new();
    let mut group_start = 0;
    let mut group_chars = 0;
    for (index, span) in entity.statements.iter().enumerate() {
        let statement_chars = utf16_len(stmt_text(source, span));
        if statement_chars > max_chars {
            if index > group_start {
                windows.push(slice_statements(source, &entity.statements, group_start, index - 1));
            }
            windows.extend(split_text_by_lines_code(
                stmt_text(source, span),
                max_chars,
                span.start_row + 1,
                span.start_byte,
                overlap_chars,
            ));
            group_start = index + 1;
            group_chars = 0;
            continue;
        }
        let separator = usize::from(index > group_start);
        if group_chars + separator + statement_chars > max_chars && index > group_start {
            windows.push(slice_statements(source, &entity.statements, group_start, index - 1));
            let overlap_start =
                compute_overlap_start(&entity.statements, group_start, index - 1, overlap_chars, source);
            let mut candidate_start = overlap_start.min(index);
            let mut candidate_chars = statement_chars;
            for prev in (candidate_start..index).rev() {
                let added = utf16_len(stmt_text(source, &entity.statements[prev])) + 1;
                if candidate_chars + added > max_chars {
                    candidate_start = prev + 1;
                    break;
                }
                candidate_chars += added;
            }
            group_start = candidate_start;
            group_chars = candidate_chars;
            continue;
        }
        group_chars += separator + statement_chars;
    }
    if group_start < entity.statements.len() {
        windows.push(slice_statements(
            source,
            &entity.statements,
            group_start,
            entity.statements.len() - 1,
        ));
    }
    windows
}

fn slice_statements(source: &str, statements: &[StmtSpan], start: usize, end: usize) -> CodeWindow {
    let first = &statements[start];
    let last = &statements[end];
    let text = source
        .as_bytes()
        .get(first.start_byte..last.end_byte)
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .unwrap_or("")
        .to_string();
    let embedding = statements[start..=end]
        .iter()
        .map(|span| {
            source
                .as_bytes()
                .get(span.start_byte..span.end_byte)
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
                .unwrap_or("")
        })
        .collect::<Vec<_>>()
        .join("\n");
    CodeWindow {
        text,
        embedding_text: Some(embedding),
        range: FragmentRange {
            start_line: first.start_row + 1,
            end_line: last.end_row + 1,
            start_offset: first.start_byte,
            end_offset: last.end_byte,
        },
    }
}

fn stmt_text<'a>(source: &'a str, span: &StmtSpan) -> &'a str {
    source
        .as_bytes()
        .get(span.start_byte..span.end_byte)
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .unwrap_or("")
}

fn compute_overlap_start(
    statements: &[StmtSpan],
    group_start: usize,
    group_end: usize,
    overlap_chars: usize,
    source: &str,
) -> usize {
    if overlap_chars == 0 {
        return group_end + 1;
    }
    let mut chars = 0;
    let mut index = group_end as isize;
    while index >= group_start as isize && chars < overlap_chars {
        chars += utf16_len(stmt_text(source, &statements[index as usize]));
        if (index as usize) < group_end {
            chars += 1;
        }
        index -= 1;
    }
    (index + 1).max(0) as usize
}

/// Line-budgeted windows over a span of text (port of `splitTextByLines`;
/// offsets are absolute into the file source).
fn split_text_by_lines_code(
    text: &str,
    max_chars: usize,
    start_line: usize,
    start_offset: usize,
    overlap_chars: usize,
) -> Vec<CodeWindow> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut windows = Vec::new();
    let mut line_index = 0;
    let mut offset = start_offset;
    while line_index < lines.len() {
        if utf16_len(lines[line_index]) > max_chars {
            windows.extend(split_long_line_by_chars(
                lines[line_index],
                max_chars,
                start_line + line_index,
                offset,
                overlap_chars,
            ));
            offset += lines[line_index].len() + 1;
            line_index += 1;
            continue;
        }
        let mut end_index = line_index;
        let mut used = 0;
        while end_index < lines.len() {
            let line_len = utf16_len(lines[end_index]) + 1;
            if used + line_len > max_chars && end_index > line_index {
                break;
            }
            used += line_len;
            end_index += 1;
        }
        let chunk = lines[line_index..end_index].join("\n");
        windows.push(CodeWindow {
            text: chunk.clone(),
            embedding_text: None,
            range: FragmentRange {
                start_line: start_line + line_index,
                end_line: start_line + end_index - 1,
                start_offset: offset,
                end_offset: offset + chunk.len(),
            },
        });
        if end_index >= lines.len() {
            break;
        }
        let overlap_lines = compute_line_overlap_code(&lines, line_index, end_index, overlap_chars);
        let next_index = end_index - overlap_lines;
        offset += lines[line_index..next_index].join("\n").len();
        if next_index > line_index {
            offset += 1;
        }
        line_index = next_index;
    }
    windows
}

fn compute_line_overlap_code(
    lines: &[&str],
    start_index: usize,
    end_index: usize,
    overlap_chars: usize,
) -> usize {
    if overlap_chars == 0 {
        return 0;
    }
    let mut chars = 0;
    let mut count = 0;
    for index in (start_index..end_index).rev() {
        chars += utf16_len(lines[index]) + 1;
        if chars > overlap_chars {
            break;
        }
        count += 1;
    }
    count.min((end_index - start_index) / 2)
}

/// Raw character cuts with overlap (port of `splitLongLineByChars`;
/// char-boundary-safe instead of surrogate-aware — same observable).
fn split_long_line_by_chars(
    text: &str,
    max_chars: usize,
    line: usize,
    start_offset: usize,
    overlap_chars: usize,
) -> Vec<CodeWindow> {
    let total = utf16_len(text);
    let mut windows = Vec::new();
    let mut relative_start = 0;
    while relative_start < total {
        let raw_end = (relative_start + max_chars).min(total);
        let slice = utf16_slice(text, relative_start, raw_end - relative_start);
        let relative_end = relative_start + utf16_len(slice);
        let byte_start = byte_offset_for_units(text, relative_start);
        windows.push(CodeWindow {
            text: slice.to_string(),
            embedding_text: None,
            range: FragmentRange {
                start_line: line,
                end_line: line,
                start_offset: start_offset + byte_start,
                end_offset: start_offset + byte_start + slice.len(),
            },
        });
        if relative_end >= total {
            break;
        }
        let raw_start = (relative_start + 1).max(relative_end.saturating_sub(overlap_chars));
        relative_start = snap_forward_units(text, raw_start);
    }
    windows
}

fn snap_forward_units(line: &str, units: usize) -> usize {
    let mut seen = 0;
    for ch in line.chars() {
        if seen >= units {
            return seen;
        }
        seen += ch.len_utf16();
    }
    seen
}

fn byte_offset_for_units(line: &str, units: usize) -> usize {
    let mut seen = 0;
    for (byte, ch) in line.char_indices() {
        if seen >= units {
            return byte;
        }
        seen += ch.len_utf16();
    }
    line.len()
}

// -- Script blocks -------------------------------------------------------------

struct ScriptBlock {
    text: String,
    format: String,
    start_line: usize,
    start_offset: usize,
}

fn find_script_blocks(text: &str) -> Vec<ScriptBlock> {
    let pattern = regex::Regex::new(r"(?i)<script(\s[^>]*)?>([\s\S]*?)</script>").expect("script pattern");
    let mut blocks = Vec::new();
    for captures in pattern.captures_iter(text) {
        let full = captures.get(0).expect("full match").as_str();
        let attrs = captures.get(1).map_or("", |m| m.as_str());
        let block_text = captures.get(2).map_or("", |m| m.as_str());
        let tag_end = full.find('>').unwrap_or(0);
        let start_offset = captures.get(0).expect("full match").start() + tag_end + 1;
        blocks.push(ScriptBlock {
            text: block_text.to_string(),
            format: script_block_format(attrs),
            start_line: 1 + text[..start_offset.min(text.len())].matches('\n').count(),
            start_offset,
        });
    }
    blocks
}

fn script_block_format(attrs: &str) -> String {
    let lang = attrs
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0].eq_ignore_ascii_case("lang"))
        .and_then(|w| w.get(1))
        .map(|s| s.trim_matches(['"', '\'']).to_lowercase())
        .unwrap_or_default();
    match lang.as_str() {
        "ts" | "typescript" => "typescript".to_string(),
        "tsx" => "tsx".to_string(),
        "jsx" => "jsx".to_string(),
        _ => "javascript".to_string(),
    }
}

fn extract_script_blocks(
    parser: &mut AstParser,
    source: &ExtractSource,
    options: ChunkOptions,
) -> Result<Vec<PreparedFragment>, ExtractError> {
    let mut fragments = Vec::new();
    for block in find_script_blocks(&source.text) {
        let sub = ExtractSource {
            file_id: source.file_id.clone(),
            text: block.text.clone(),
            format: block.format.clone(),
            kind: FileKind::Code,
        };
        let extractor = CodeExtractor;
        let mut items = extractor.extract_for_indexing(parser, &sub, options)?;
        // Remap ids + ranges into file coordinates.
        let mut id_map = std::collections::HashMap::new();
        for (index, item) in items.iter().enumerate() {
            id_map.insert(item.id.clone(), make_entity_id(&source.file_id, fragments.len() + index));
        }
        for item in items.drain(..) {
            let id = id_map.get(&item.id).cloned().unwrap_or(item.id);
            let group = item.group.and_then(|g| id_map.get(&g).cloned());
            fragments.push(PreparedFragment {
                id,
                content_text: item.content_text,
                embedding_text: item.embedding_text,
                range: FragmentRange {
                    start_line: block.start_line + item.range.start_line - 1,
                    end_line: block.start_line + item.range.end_line - 1,
                    start_offset: block.start_offset + item.range.start_offset,
                    end_offset: block.start_offset + item.range.end_offset,
                },
                group,
                metadata: item.metadata,
                calls: item.calls,
            });
        }
    }
    Ok(fragments)
}

#[cfg(test)]
#[path = "../../tests/extract_code.rs"]
mod tests;
