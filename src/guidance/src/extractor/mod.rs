//! P2 extraction core: shared fragment types, entity ids, and UTF-16
//! width arithmetic (zvec counts UTF-16 code units; chunking parity
//! requires the same unit here, with char-boundary-safe slicing).

use crate::search_types::{CodeSymbolType, FileKind};
use common_core::string::{utf16_len, utf16_slice};

/// Default code chunk budget (port of `DEFAULT_CODE_CHUNK_CHARS`).
pub const DEFAULT_CHUNK_CHARS: usize = 3600;
/// Default chunk overlap (port of `DEFAULT_CODE_CHUNK_OVERLAP_CHARS`).
pub const DEFAULT_CHUNK_OVERLAP_CHARS: usize = 540;

/// A source file ready for extraction.
#[derive(Debug, Clone)]
pub struct ExtractSource {
    /// Stable file id.
    pub file_id: String,
    /// Full file text.
    pub text: String,
    /// Format tag (`rust`, `markdown`, …).
    pub format: String,
    /// Kind tag.
    pub kind: FileKind,
}

/// Chunk budget (port of `ChunkOptions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkOptions {
    /// Maximum chunk size (UTF-16 code units, zvec parity).
    pub max_chunk_chars: usize,
    /// Overlap between adjacent windows (UTF-16 code units).
    pub chunk_overlap_chars: usize,
}

impl ChunkOptions {
    /// Resolve with zvec defaults + validation (positive size, overlap < size).
    pub fn resolve(max: Option<usize>, overlap: Option<usize>) -> Result<Self, ExtractError> {
        let max_chunk_chars = max.unwrap_or(DEFAULT_CHUNK_CHARS);
        let chunk_overlap_chars = overlap.unwrap_or(DEFAULT_CHUNK_OVERLAP_CHARS);
        if max_chunk_chars == 0 {
            return Err(ExtractError::InvalidChunkSize(format!(
                "maxChunkChars={max_chunk_chars}"
            )));
        }
        if chunk_overlap_chars >= max_chunk_chars {
            return Err(ExtractError::InvalidChunkOverlap(format!(
                "maxChunkChars={max_chunk_chars} chunkOverlapChars={chunk_overlap_chars}"
            )));
        }
        Ok(Self {
            max_chunk_chars,
            chunk_overlap_chars,
        })
    }
}

/// Line-anchored text range (port of `TextRange`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FragmentRange {
    /// 1-based first line.
    pub start_line: usize,
    /// 1-based last line.
    pub end_line: usize,
    /// Byte offset of the first character.
    pub start_offset: usize,
    /// Byte offset past the last character.
    pub end_offset: usize,
}

/// Fragment metadata (shared scope type for code + markdown, P2.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FragmentMetadata {
    /// Code entity metadata (port of `CodeEntityMetadata`).
    Code {
        /// Closed symbol-type set.
        symbol_type: CodeSymbolType,
        /// Entity name, when known.
        symbol_name: Option<String>,
        /// `::`-joined breadcrumb scope.
        scope: Option<String>,
        /// Tree-sitter node type.
        node_type: String,
        /// Normalized signature line.
        signature: Option<String>,
        /// Preceding doc comment.
        doc: Option<String>,
        /// Modifier set.
        modifiers: Vec<String>,
    },
    /// Markdown section metadata.
    Markdown {
        /// Section heading.
        heading: Option<String>,
        /// Heading level (1–6).
        level: Option<u32>,
        /// `::`-joined heading breadcrumb scope.
        scope: Option<String>,
    },
}

/// One extracted fragment before id assignment (ids are positional:
/// `make_entity_id(file_id, index)` in output order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedFragment {
    /// Entity id (`make_entity_id(file_id, position)`).
    pub id: String,
    /// Fragment text.
    pub content_text: String,
    /// Embedding-text override (`embeddingText`); `None` = content text.
    pub embedding_text: Option<String>,
    /// Source range.
    pub range: FragmentRange,
    /// Group id: own id for singles/majors, the major id for windows;
    /// `None` for ungrouped singles (public id falls back to `id`).
    pub group: Option<String>,
    /// Fragment metadata.
    pub metadata: Option<FragmentMetadata>,
    /// Harvested call names (`calls:` outline + P3 graph edges).
    pub calls: Vec<String>,
}

impl PreparedFragment {
    /// Effective text handed to the embedding model.
    #[must_use]
    pub fn embedding_source(&self) -> &str {
        self.embedding_text.as_deref().unwrap_or(&self.content_text)
    }
}

/// Extraction failure (per-file isolation: stored, never thrown past prepare).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExtractError {
    /// Non-text source handed to a text extractor.
    #[error("extractor received a non-text source: {0}")]
    UnsupportedSource(String),
    /// Non-positive chunk size.
    #[error("extractor requires a positive integer chunk size: {0}")]
    InvalidChunkSize(String),
    /// Overlap not smaller than the chunk size.
    #[error("extractor requires overlap to be smaller than chunk size: {0}")]
    InvalidChunkOverlap(String),
    /// Grammar or parse failure (falls back to text windows upstream).
    #[error("failed to parse source: {0}")]
    Parse(String),
}

/// Entity id: `sha256(fileId + NUL + index)` hex (64 chars, zvec parity).
#[must_use]
pub fn make_entity_id(file_id: &str, index: usize) -> String {
    common_core::hash::sha256_hex(format!("{file_id}\0{index}").as_bytes())
}

pub mod adapter;
pub mod code;
pub mod markdown;
pub mod text;
pub mod vector_content;

/// M1.2 shared text-shape core (C1/C3/C4): UTF-16-unit offset,
/// backward overlap counting, and long-line window splitting.
/// Call-site wrappers in `code`, `markdown`, and `text` preserve
/// their exact edge contracts; see `tests/extract_text_shape.rs`.
#[must_use]
pub(crate) fn byte_offset_for_units(line: &str, units: usize) -> usize {
    let mut seen = 0;
    for (byte, ch) in line.char_indices() {
        if seen >= units {
            return byte;
        }
        seen += ch.len_utf16();
    }
    line.len()
}

/// Backward overlap walk shared by the three overlap counters.
/// `inclusive` selects the text rule (count the line that
/// meets/exceeds the budget) versus the code/markdown rule (stop
/// before it). `overlap_units == 0` counts nothing either way.
fn count_trailing_overlap(
    lines: &[&str],
    start: usize,
    end: usize,
    overlap_units: usize,
    inclusive: bool,
) -> usize {
    if overlap_units == 0 {
        return 0;
    }
    let mut weight = 0usize;
    let mut count = 0usize;
    for index in (start..end).rev() {
        weight += utf16_len(lines[index]) + 1;
        if weight > overlap_units {
            return if inclusive { count + 1 } else { count };
        }
        count += 1;
        if inclusive && weight >= overlap_units {
            return count;
        }
    }
    count
}

/// Trailing-line overlap count, code/markdown rule: lines whose
/// cumulative `utf16_len + 1` weight stays within budget, walking
/// back from `end` (exclusive) to `start` (inclusive). No cap here;
/// callers apply their own window clamp.
#[must_use]
pub(crate) fn overlap_lines_back(
    lines: &[&str],
    start: usize,
    end: usize,
    overlap_units: usize,
) -> usize {
    count_trailing_overlap(lines, start, end, overlap_units, false)
}

/// Trailing-line overlap count, text rule: as above but the line
/// that meets the budget still counts.
#[must_use]
pub(crate) fn overlap_lines_back_inclusive(
    lines: &[&str],
    start: usize,
    end: usize,
    overlap_units: usize,
) -> usize {
    count_trailing_overlap(lines, start, end, overlap_units, true)
}

/// Long-line window split over UTF-16 units: `(byte_start, slice)`
/// pairs in order. `cut(window, max)` picks the slice length for an
/// over-budget remainder (pass `find_line_cut`); callers map pairs
/// to their window struct and own skipping. The byte start is the
/// historical `byte_offset_for_units` value (it can sit past an
/// astral char the slice starts inside — preserved exactly as the
/// historical ranges did, never re-sliced here, so no new panics).
/// Mirrors the historical splitters exactly, including advancing
/// past whitespace-only slices and no `max == 0` guard (budgets
/// are validated upstream in `ChunkOptions::resolve`).
#[must_use]
pub(crate) fn split_long_units(
    line: &str,
    max_units: usize,
    cut: fn(&str, usize) -> usize,
) -> Vec<(usize, &str)> {
    let total = utf16_len(line);
    let mut out = Vec::new();
    let mut offset = 0;
    while offset < total {
        let remaining = total - offset;
        let slice_len = if remaining <= max_units {
            remaining
        } else {
            cut(utf16_slice(line, offset, remaining), max_units)
        };
        let slice = utf16_slice(line, offset, slice_len);
        let byte_start = byte_offset_for_units(line, offset);
        out.push((byte_start, slice));
        offset += slice_len;
    }
    out
}

#[cfg(test)]
#[path = "../../tests/extract_text_shape.rs"]
mod text_shape_tests;

#[cfg(test)]
#[path = "../../tests/split_m0_utf16.rs"]
mod split_m0_utf16_tests;
