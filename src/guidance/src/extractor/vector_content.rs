//! P2 metadata-prefixed embedding text (port of
//! `extraction/vector-content.ts`): `symbol:`/`scope:`/`signature:`/
//! `modifiers:`/`doc:` (code) or `heading:`/`heading_level:`/`scope:`
//! (markdown) lines, capped at 25% of the chunk budget, prepended to the
//! fragment text. Char-boundary-safe truncation (the surrogate-pair rule).

use super::{FragmentMetadata, utf16_len, utf16_prefix};

/// Metadata budget ratio (port of `MAX_METADATA_BUDGET_RATIO`).
pub const METADATA_BUDGET_RATIO: f64 = 0.25;

/// Metadata lines for a fragment (port of `vectorMetadataText`).
#[must_use]
pub fn metadata_lines(metadata: &Option<FragmentMetadata>) -> Vec<String> {
    let Some(metadata) = metadata else {
        return Vec::new();
    };
    match metadata {
        FragmentMetadata::Code {
            symbol_type,
            symbol_name,
            scope,
            signature,
            doc,
            modifiers,
            ..
        } => {
            let type_name = match symbol_type {
                crate::zg_types::CodeSymbolType::Module => "module",
                crate::zg_types::CodeSymbolType::Class => "class",
                crate::zg_types::CodeSymbolType::Interface => "interface",
                crate::zg_types::CodeSymbolType::Function => "function",
                crate::zg_types::CodeSymbolType::Value => "value",
                crate::zg_types::CodeSymbolType::Alias => "alias",
            };
            let mut lines = vec![match symbol_name {
                Some(name) => format!("symbol: {type_name} {name}"),
                None => format!("symbol: {type_name}"),
            }];
            if let Some(scope) = scope {
                lines.push(format!("scope: {scope}"));
            }
            if let Some(signature) = signature {
                lines.push(format!("signature: {}", one_line(signature)));
            }
            if !modifiers.is_empty() {
                lines.push(format!("modifiers: {}", modifiers.join(" ")));
            }
            if let Some(doc) = doc {
                lines.push(format!("doc: {}", one_line(doc)));
            }
            lines
        }
        FragmentMetadata::Markdown { heading, level, scope } => {
            let mut lines = Vec::new();
            if let Some(heading) = heading {
                lines.push(format!("heading: {heading}"));
            }
            if let Some(level) = level {
                lines.push(format!("heading_level: {level}"));
            }
            if let Some(scope) = scope {
                lines.push(format!("scope: {scope}"));
            }
            lines
        }
    }
}

/// Budget for the metadata prefix (port of `metadataBudget`).
#[must_use]
pub fn metadata_budget(max_chars: Option<usize>) -> Option<usize> {
    max_chars.map(|max| (max as f64 * METADATA_BUDGET_RATIO).floor() as usize)
}

/// Join metadata lines, truncated to `max_chars` (port of
/// `compactMetadataLines` + `fitTextToChars`).
#[must_use]
pub fn compact_metadata_lines(lines: &[String], max_chars: Option<usize>) -> String {
    let text = lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    match max_chars {
        None => text,
        Some(max) => fit_text_to_chars(&text, max),
    }
}

/// Embedding text for a fragment: metadata prefix + content (port of
/// `vectorContentForFragment`). Widths are UTF-16 units (zvec parity).
#[must_use]
pub fn vector_content_for_fragment(
    metadata: &Option<FragmentMetadata>,
    content: &str,
    max_chars: Option<usize>,
) -> String {
    let prefix = compact_metadata_lines(&metadata_lines(metadata), metadata_budget(max_chars));
    if prefix.is_empty() {
        return content.to_string();
    }
    format!("{prefix}\n{content}")
}

/// Shrink the content budget by the metadata prefix (port of
/// `chunkOptionsForMetadata`).
#[must_use]
pub fn chunk_options_for_metadata(
    max_chunk_chars: usize,
    chunk_overlap_chars: usize,
    metadata: &Option<FragmentMetadata>,
) -> (usize, usize) {
    let prefix = compact_metadata_lines(
        &metadata_lines(metadata),
        metadata_budget(Some(max_chunk_chars)),
    );
    let separator = usize::from(!prefix.is_empty());
    let max = max_chunk_chars
        .saturating_sub(utf16_len(&prefix))
        .saturating_sub(separator)
        .max(1);
    let overlap = chunk_overlap_chars.min(max.saturating_sub(1));
    (max, overlap)
}

/// Truncate to `max_chars` UTF-16 units with a `...` marker (port of
/// `fitTextToChars`); never splits a character.
#[must_use]
pub fn fit_text_to_chars(value: &str, max_chars: usize) -> String {
    if utf16_len(value) <= max_chars {
        return value.to_string();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    let end = utf16_prefix(value, max_chars - 3);
    format!("{}...", end.trim_end())
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
#[path = "../../tests/extract_vector_content.rs"]
mod tests;
