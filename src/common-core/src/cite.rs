//! `file:line` citation scanner — pure string math, no domain import.

#[derive(Debug, Clone, PartialEq, Eq)]
/// `file:line` citation scanner — pure string math, no domain import.
pub struct Citation {
    pub file: String,
    pub line: u32,
    pub span: (usize, usize),
}

/// Extract all `file:line` citations from text.
///
/// Matches patterns like `src/foo.rs:42`, `src/foo.rs:42:10`, or `path/file.zig:7`.
/// Returns a `Citation` per match with byte `span` covering the `file:line` text.
pub fn extract_citations(text: &str) -> Vec<Citation> {
    let mut citations = Vec::new();
    for (line_idx, line) in text.lines().enumerate() {
        let line_start = text_lines_offset(text, line_idx);
        let mut search_start = 0;
        while search_start < line.len() {
            if let Some(colon_pos) = line[search_start..].find(':') {
                let abs_colon = search_start + colon_pos;
                if let Some(citation) = extract_citation_at_colon(line, abs_colon, line_start) {
                    let line_rel_end = citation.span.1 - line_start;
                    citations.push(citation);
                    search_start = line_rel_end;
                } else {
                    search_start = abs_colon + 1;
                }
            } else {
                break;
            }
        }
    }
    citations
}

fn text_lines_offset(text: &str, target_line_idx: usize) -> usize {
    let mut offset = 0;
    for (idx, line) in text.lines().enumerate() {
        if idx == target_line_idx {
            return offset;
        }
        offset += line.len() + 1; // +1 for '\n'
    }
    offset
}

/// Shared file-token scan: the file token ending at `colon_pos`
/// plus its start offset. Both citation parsers funnel through here.
fn scan_file_token(line: &str, colon_pos: usize) -> Option<(String, usize)> {
    let before = &line[..colon_pos];
    let file_start = before
        .rfind(|c: char| {
            !c.is_alphanumeric() && c != '/' && c != '\\' && c != '.' && c != '_' && c != '-'
        })
        .map_or(0, |p| p + 1);
    let file_part = &before[file_start..];
    if file_part.is_empty() || !file_part.contains('.') {
        return None;
    }
    Some((file_part.to_string(), file_start))
}

/// Shared line-spec scan: leading `start` digits plus an optional
/// `-end` suffix. Returns the normalized range and its byte length.
fn parse_range_spec(after: &str) -> Option<(u32, u32, usize)> {
    let mut digits_end = 0;
    for (i, c) in after.char_indices() {
        if c.is_ascii_digit() {
            digits_end = i + c.len_utf8();
        } else {
            break;
        }
    }
    if digits_end == 0 {
        return None;
    }
    let start: u32 = after[..digits_end].parse().ok()?;
    let mut end = start;
    let mut spec_len = digits_end;
    let rest = &after[digits_end..];
    if let Some(tail) = rest.strip_prefix('-') {
        let mut tail_end = 0;
        for (i, c) in tail.char_indices() {
            if c.is_ascii_digit() {
                tail_end = i + c.len_utf8();
            } else {
                break;
            }
        }
        if tail_end > 0 {
            end = tail[..tail_end].parse().ok()?;
            spec_len += 1 + tail_end;
        }
    }
    Some((start.min(end), start.max(end), spec_len))
}

fn extract_citation_at_colon(
    line: &str,
    colon_pos: usize,
    line_abs_start: usize,
) -> Option<Citation> {
    let after = &line[colon_pos + 1..];
    let (file, file_start) = scan_file_token(line, colon_pos)?;

    let mut digits_end = 0;
    for (i, c) in after.char_indices() {
        if c.is_ascii_digit() {
            digits_end = i + c.len_utf8();
        } else {
            break;
        }
    }

    if digits_end == 0 {
        return None;
    }

    let line_str = &after[..digits_end];
    let line_num: u32 = line_str.parse().ok()?;

    let span_start = line_abs_start + file_start;
    let span_end = line_abs_start + colon_pos + 1 + digits_end;

    Some(Citation {
        file,
        line: line_num,
        span: (span_start, span_end),
    })
}

/// Range twin of [`extract_citation_at_colon`]: same file token,
/// full `start-end` span.
fn extract_range_at_colon(
    line: &str,
    colon_pos: usize,
    line_abs_start: usize,
) -> Option<CitationRange> {
    let after = &line[colon_pos + 1..];
    let (file, file_start) = scan_file_token(line, colon_pos)?;
    let (start, end, spec_len) = parse_range_spec(after)?;
    let span_start = line_abs_start + file_start;
    let span_end = line_abs_start + colon_pos + 1 + spec_len;
    Some(CitationRange {
        file,
        start,
        end,
        span: (span_start, span_end),
    })
}

/// A `file:start-end` citation range: the file token plus the
/// cited line range. `start == end` for a bare `file:line` input,
/// where the span agrees byte-for-byte with [`extract_citations`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitationRange {
    pub file: String,
    pub start: u32,
    pub end: u32,
    pub span: (usize, usize),
}

/// Extract all `file:line` and `file:start-end` citation ranges.
/// Same file-token scan as [`extract_citations`], extended with an
/// optional `-end` line suffix; the span covers the full range text.
pub fn extract_citation_ranges(text: &str) -> Vec<CitationRange> {
    let mut ranges = Vec::new();
    for (line_idx, line) in text.lines().enumerate() {
        let line_start = text_lines_offset(text, line_idx);
        let mut search_start = 0;
        while search_start < line.len() {
            if let Some(colon_pos) = line[search_start..].find(':') {
                let abs_colon = search_start + colon_pos;
                if let Some(range) = extract_range_at_colon(line, abs_colon, line_start) {
                    let line_rel_end = range.span.1 - line_start;
                    ranges.push(range);
                    search_start = line_rel_end;
                } else {
                    search_start = abs_colon + 1;
                }
            } else {
                break;
            }
        }
    }
    ranges
}
/// Try to extract a `file:line` citation at the given byte offset.
///
/// If `offset` lies inside a citation's `span` (as returned by `extract_citations`),
/// returns that citation; otherwise `None`.
pub fn extract_citation_at(text: &str, offset: usize) -> Option<Citation> {
    extract_citations(text)
        .into_iter()
        .find(|citation| offset >= citation.span.0 && offset < citation.span.1)
}
