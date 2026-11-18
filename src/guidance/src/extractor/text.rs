//! P2 plain-text extraction (port of `extraction/text/extractor.ts`):
//! line-budgeted windows with scored intra-line cuts and line overlap.
//! Budgets count UTF-16 code units (zvec parity); ranges carry byte
//! offsets (Rust-side slicing is byte-based — documented deviation).

use super::{
    utf16_len, ChunkOptions, ExtractError, ExtractSource, FragmentRange, PreparedFragment,
};

/// Extract whole-file text windows (port of `extractPlainTextFragments`).
pub fn extract_plain_text_fragments(
    source: &ExtractSource,
    options: ChunkOptions,
) -> Result<Vec<PreparedFragment>, ExtractError> {
    Ok(chunk_text(&source.text, options)
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| PreparedFragment {
            id: super::make_entity_id(&source.file_id, index),
            content_text: chunk.text,
            embedding_text: None,
            range: chunk.range,
            group: None,
            metadata: None,
            calls: Vec::new(),
        })
        .collect())
}

/// A text window with its range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextChunk {
    /// Window text.
    pub text: String,
    /// Source range.
    pub range: FragmentRange,
}

/// Split text into budgeted windows (port of `chunkText`).
#[must_use]
pub fn chunk_text(text: &str, options: ChunkOptions) -> Vec<TextChunk> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    let max = options.max_chunk_chars;
    let overlap = options.chunk_overlap_chars;
    let lines: Vec<&str> = text.split('\n').collect();
    let line_offsets = compute_line_offsets(&lines);
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < lines.len() {
        if utf16_len(lines[start]) + 1 > max {
            chunks.extend(split_long_line(&lines, &line_offsets, start, max));
            start += 1;
            continue;
        }
        let mut used = 0;
        let mut end = start;
        while end < lines.len() {
            let line_len = utf16_len(lines[end]) + 1;
            if used + line_len > max && end > start {
                break;
            }
            used += line_len;
            end += 1;
        }
        let chunk_text = lines[start..end].join("\n");
        if !chunk_text.trim().is_empty() {
            let end_line = end - 1;
            chunks.push(TextChunk {
                text: chunk_text,
                range: FragmentRange {
                    start_line: start + 1,
                    end_line: end,
                    start_offset: line_offsets[start],
                    end_offset: line_offsets[end_line] + lines[end_line].len(),
                },
            });
        }
        if end >= lines.len() {
            break;
        }
        start = compute_next_start_line(&lines, start, end, overlap);
    }
    chunks
}

/// Byte offsets of each line start.
#[must_use]
pub fn compute_line_offsets(lines: &[&str]) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(lines.len());
    let mut offset = 0;
    for line in lines {
        offsets.push(offset);
        offset += line.len() + 1;
    }
    offsets
}

fn split_long_line(
    lines: &[&str],
    line_offsets: &[usize],
    index: usize,
    max: usize,
) -> Vec<TextChunk> {
    let line = lines[index];
    let line_number = index + 1;
    let mut chunks = Vec::new();
    // M1.6: shared window splitter; the whitespace-only skip stays here.
    for (byte_start, slice) in super::split_long_units(line, max, find_line_cut) {
        if !slice.trim().is_empty() {
            chunks.push(TextChunk {
                text: slice.to_string(),
                range: FragmentRange {
                    start_line: line_number,
                    end_line: line_number,
                    start_offset: line_offsets[index] + byte_start,
                    end_offset: line_offsets[index] + byte_start + slice.len(),
                },
            });
        }
    }
    chunks
}

/// Scored intra-line cut (port of `findLineCut`): sentence > clause >
/// whitespace > hyphen/slash, scanned over `[0.7*max, max)`, latest wins
/// on ties (`>=`).
#[must_use]
pub fn find_line_cut(line: &str, max_chars: usize) -> usize {
    if utf16_len(line) <= max_chars {
        return utf16_len(line);
    }
    let min_position = (max_chars as f64 * 0.7).floor() as usize;
    let mut best_position = 0;
    let mut best_score = 0;
    for (index, ch) in line
        .encode_utf16()
        .enumerate()
        .skip(min_position)
        .take(max_chars.saturating_sub(min_position))
    {
        let c = char::from_u32(u32::from(ch)).unwrap_or('\0');
        let score = match c {
            '.' | '!' | '?' => 4,
            ',' | ';' | ':' => 3,
            ' ' | '\t' => 2,
            '-' | '/' | '\\' => 1,
            _ => 0,
        };
        if score > 0 && score >= best_score {
            best_score = score;
            best_position = index + 1;
        }
    }
    if best_position > 0 {
        best_position
    } else {
        max_chars
    }
}

fn compute_next_start_line(
    lines: &[&str],
    start_index: usize,
    end_index: usize,
    overlap_chars: usize,
) -> usize {
    // M1.5: shared backward-count core, text rule (the meeting line
    // counts); absolute-index contract and no-cap clamp stay here.
    if overlap_chars == 0 {
        return end_index;
    }
    let overlap = super::overlap_lines_back_inclusive(lines, start_index, end_index, overlap_chars);
    let next = end_index.saturating_sub(overlap);
    if next > start_index {
        next
    } else {
        end_index
    }
}

#[cfg(test)]
#[path = "../../tests/extract_text.rs"]
mod tests;
