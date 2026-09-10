//! P2 extraction core: shared fragment types, entity ids, and UTF-16
//! width arithmetic (zvec counts UTF-16 code units; chunking parity
//! requires the same unit here, with char-boundary-safe slicing).

use crate::zg_types::{CodeSymbolType, FileKind};

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

/// UTF-16 width of a string (zvec parity unit for chunk budgets).
#[must_use]
pub fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Slice `text` to at most `max_units` UTF-16 code units without splitting
/// a character (char boundaries imply surrogate-pair safety in Rust).
#[must_use]
pub fn utf16_prefix(text: &str, max_units: usize) -> &str {
    utf16_slice(text, 0, max_units)
}

/// UTF-16 window `[start_units, start_units + len_units)` snapped to char
/// boundaries (start snaps forward to the containing character, end snaps
/// back so the window never exceeds the budget).
#[must_use]
pub fn utf16_slice(text: &str, start_units: usize, len_units: usize) -> &str {
    let end_units = start_units + len_units;
    let mut byte_start = None;
    let mut byte_end = None;
    let mut units = 0;
    for (byte, ch) in text.char_indices() {
        let width = ch.len_utf16();
        if byte_start.is_none() && units + width > start_units {
            byte_start = Some(byte);
        }
        if byte_start.is_some() && units + width > end_units {
            byte_end = Some(byte);
            break;
        }
        units += width;
    }
    match (byte_start, byte_end) {
        (Some(start), Some(end)) => &text[start..end],
        (Some(start), None) => &text[start..],
        (None, _) => "",
    }
}

pub mod adapter;
pub mod code;
pub mod markdown;
pub mod text;
pub mod vector_content;
