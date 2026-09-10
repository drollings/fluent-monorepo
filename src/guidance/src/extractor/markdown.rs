//! P2 markdown extraction (port of `extraction/markdown/extractor.ts`):
//! fence-aware ATX/setext heading scan, breadcrumb sections, outline
//! majors for multi-window sections, break-score window splitting.

use super::{
    ChunkOptions, ExtractError, ExtractSource, FragmentMetadata, FragmentRange, PreparedFragment,
    utf16_len,
    text::{compute_line_offsets, find_line_cut},
    vector_content::chunk_options_for_metadata,
};

/// Extract markdown sections (empty when the format is not markdown or no
/// headings exist — the caller falls back to text windows).
pub fn extract_markdown_fragments(
    source: &ExtractSource,
    options: ChunkOptions,
) -> Result<Vec<PreparedFragment>, ExtractError> {
    if source.format != "markdown" {
        return Ok(Vec::new());
    }
    let lines: Vec<&str> = source.text.split('\n').collect();
    let headings = scan_headings(&lines);
    if headings.is_empty() {
        return Ok(Vec::new());
    }
    let line_offsets = compute_line_offsets(&lines);
    let fence_lines = compute_fence_lines(&lines);
    let sections = build_sections(&headings, &lines);
    let mut fragments: Vec<PreparedFragment> = Vec::new();
    for section in &sections {
        let metadata = markdown_metadata(section);
        let meta_opt = Some(metadata.clone());
        let (max, overlap) =
            chunk_options_for_metadata(options.max_chunk_chars, options.chunk_overlap_chars, &meta_opt);
        let windows = split_markdown_section(&lines, &line_offsets, &fence_lines, section, max, overlap);
        if windows.len() > 1 {
            let major_id = super::make_entity_id(&source.file_id, fragments.len());
            fragments.push(PreparedFragment {
                id: major_id.clone(),
                content_text: super::vector_content::fit_text_to_chars(
                    &markdown_outline(&metadata),
                    max,
                ),
                embedding_text: None,
                range: lines_to_range(&lines, &line_offsets, section.start_index, section.end_index),
                group: Some(major_id.clone()),
                metadata: Some(metadata),
                calls: Vec::new(),
            });
            for window in windows {
                let id = super::make_entity_id(&source.file_id, fragments.len());
                fragments.push(PreparedFragment {
                    id,
                    content_text: window.text,
                    embedding_text: None,
                    range: window.range,
                    group: Some(major_id.clone()),
                    metadata: Some(markdown_metadata(section)),
                    calls: Vec::new(),
                });
            }
            continue;
        }
        for window in windows {
            let id = super::make_entity_id(&source.file_id, fragments.len());
            fragments.push(PreparedFragment {
                id,
                content_text: window.text,
                embedding_text: None,
                range: window.range,
                group: None,
                metadata: Some(markdown_metadata(section)),
                calls: Vec::new(),
            });
        }
    }
    Ok(fragments)
}

/// A markdown heading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heading {
    /// Level 1–6.
    pub level: u32,
    /// Heading text.
    pub text: String,
    /// 0-based line index.
    pub line_index: usize,
}

/// A headed section with its ancestor breadcrumb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// Section heading (`None` = preamble before the first heading).
    pub heading: Option<Heading>,
    /// 0-based first line index.
    pub start_index: usize,
    /// 0-based last line index (inclusive).
    pub end_index: usize,
    /// Ancestor heading texts.
    pub breadcrumb: Vec<String>,
}

/// Fence-aware heading scan (ATX `#` + setext `=`/`-`; port of `scanHeadings`).
#[must_use]
pub fn scan_headings(lines: &[&str]) -> Vec<Heading> {
    let mut headings = Vec::new();
    let mut fence: Option<&str> = None;
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim_start();
        if let Some(open) = fence {
            if trimmed.starts_with(open) {
                fence = None;
            }
            index += 1;
            continue;
        }
        if trimmed.starts_with("```") {
            fence = Some("```");
            index += 1;
            continue;
        }
        if trimmed.starts_with("~~~") {
            fence = Some("~~~");
            index += 1;
            continue;
        }
        if let Some(atx) = parse_atx(line) {
            headings.push(Heading { level: atx.0, text: atx.1, line_index: index });
            index += 1;
            continue;
        }
        let next = lines.get(index + 1).map(|l| l.trim());
        if !line.trim().is_empty() {
            if let Some(next) = next {
                if !next.is_empty() && next.chars().all(|c| c == '=') {
                    headings.push(Heading { level: 1, text: line.trim().to_string(), line_index: index });
                    index += 2;
                    continue;
                }
                if !next.is_empty() && next.chars().all(|c| c == '-') {
                    headings.push(Heading { level: 2, text: line.trim().to_string(), line_index: index });
                    index += 2;
                    continue;
                }
            }
        }
        index += 1;
    }
    headings
}

fn parse_atx(line: &str) -> Option<(u32, String)> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &line[hashes..];
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    let mut text = rest.trim().to_string();
    while text.ends_with('#') {
        text.pop();
    }
    let text = text.trim().to_string();
    if text.is_empty() {
        return None;
    }
    Some((hashes as u32, text))
}

/// Nest sections under ancestor headings (port of `buildSections`).
#[must_use]
pub fn build_sections(headings: &[Heading], lines: &[&str]) -> Vec<Section> {
    let mut sections = Vec::new();
    let first = &headings[0];
    if first.line_index > 0
        && !lines[..first.line_index].join("\n").trim().is_empty()
    {
        sections.push(Section {
            heading: None,
            start_index: 0,
            end_index: first.line_index - 1,
            breadcrumb: Vec::new(),
        });
    }
    let mut stack: Vec<&Heading> = Vec::new();
    for (i, heading) in headings.iter().enumerate() {
        while stack.last().is_some_and(|top| top.level >= heading.level) {
            stack.pop();
        }
        sections.push(Section {
            heading: Some(heading.clone()),
            start_index: heading.line_index,
            end_index: headings.get(i + 1).map_or(lines.len() - 1, |next| next.line_index - 1),
            breadcrumb: stack.iter().map(|h| h.text.clone()).collect(),
        });
        stack.push(heading);
    }
    sections
}

fn markdown_metadata(section: &Section) -> FragmentMetadata {
    FragmentMetadata::Markdown {
        heading: section.heading.as_ref().map(|h| h.text.clone()),
        level: section.heading.as_ref().map(|h| h.level),
        scope: if section.breadcrumb.is_empty() {
            None
        } else {
            Some(section.breadcrumb.join("::"))
        },
    }
}

fn markdown_outline(metadata: &FragmentMetadata) -> String {
    match metadata {
        FragmentMetadata::Markdown { heading, .. } => {
            heading.clone().unwrap_or_else(|| "markdown section".to_string())
        }
        FragmentMetadata::Code { .. } => "markdown section".to_string(),
    }
}

/// A markdown window with its range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownWindow {
    /// Window text.
    pub text: String,
    /// Source range.
    pub range: FragmentRange,
}

fn split_markdown_section(
    lines: &[&str],
    line_offsets: &[usize],
    fence_lines: &[bool],
    section: &Section,
    max_chars: usize,
    overlap_chars: usize,
) -> Vec<MarkdownWindow> {
    let mut windows = Vec::new();
    let mut start = section.start_index;
    while start <= section.end_index {
        if utf16_len(lines[start]) + 1 > max_chars {
            windows.extend(split_long_md_line(lines, line_offsets, start, max_chars));
            start += 1;
            continue;
        }
        let mut end = start;
        let mut used = 0;
        while end <= section.end_index {
            let line_len = utf16_len(lines[end]) + 1;
            if used + line_len > max_chars && end > start {
                break;
            }
            used += line_len;
            end += 1;
        }
        if end <= section.end_index && end - start > 1 {
            end = choose_markdown_break(lines, fence_lines, start, end);
        }
        windows.push(lines_to_window(lines, line_offsets, start, end - 1));
        if end > section.end_index {
            break;
        }
        let overlap_lines = compute_markdown_overlap_lines(lines, start, end, overlap_chars);
        let next = end.saturating_sub(overlap_lines);
        start = if next > start { next } else { end };
    }
    windows.into_iter().filter(|w| !w.text.trim().is_empty()).collect()
}

fn choose_markdown_break(
    lines: &[&str],
    fence_lines: &[bool],
    start_index: usize,
    end_index: usize,
) -> usize {
    let min_break = (start_index + ((end_index - start_index) as f64 * 0.7).floor() as usize)
        .max(start_index + 1);
    let mut best = end_index;
    let mut best_score = markdown_break_score(lines, fence_lines, end_index);
    for index in min_break..=end_index {
        let score = markdown_break_score(lines, fence_lines, index);
        if score > best_score {
            best = index;
            best_score = score;
        }
    }
    best
}

fn markdown_break_score(lines: &[&str], fence_lines: &[bool], index: usize) -> u32 {
    if index == 0 || index >= lines.len() || fence_lines[index] {
        return 0;
    }
    let current = lines[index].trim();
    let previous = lines[index - 1].trim();
    if is_atx_heading(current) {
        return 100;
    }
    if previous.is_empty() && current.is_empty() {
        return 70;
    }
    if previous.is_empty() {
        return 60;
    }
    if is_list_item(current) {
        return 35;
    }
    if current.starts_with('>') {
        return 25;
    }
    10
}

fn is_atx_heading(line: &str) -> bool {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&hashes) && line[hashes..].starts_with([' ', '\t'])
}

fn is_list_item(line: &str) -> bool {
    let mut chars = line.chars();
    match chars.next() {
        Some('-' | '*' | '+') => chars.next().is_some_and(char::is_whitespace),
        Some(d) if d.is_ascii_digit() => {
            let rest: String = chars.collect();
            let digits = rest.chars().take_while(char::is_ascii_digit).count();
            let after = &rest[digits..];
            after.starts_with(['.', ')']) && after[1..].starts_with([' ', '\t'])
        }
        _ => false,
    }
}

fn compute_markdown_overlap_lines(
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
    let mut index = end_index.saturating_sub(1);
    loop {
        if index <= start_index {
            break;
        }
        chars += utf16_len(lines[index]) + 1;
        if chars > overlap_chars {
            break;
        }
        count += 1;
        if index == 0 {
            break;
        }
        index -= 1;
    }
    count.min((end_index - start_index) / 2)
}

fn split_long_md_line(
    lines: &[&str],
    line_offsets: &[usize],
    index: usize,
    max_chars: usize,
) -> Vec<MarkdownWindow> {
    let line = lines[index];
    let total = utf16_len(line);
    let mut windows = Vec::new();
    let mut offset = 0;
    while offset < total {
        let remaining = total - offset;
        let slice_len = if remaining <= max_chars {
            remaining
        } else {
            find_line_cut(super::utf16_slice(line, offset, remaining), max_chars)
        };
        let text = super::utf16_slice(line, offset, slice_len);
        windows.push(MarkdownWindow {
            text: text.to_string(),
            range: FragmentRange {
                start_line: index + 1,
                end_line: index + 1,
                start_offset: line_offsets[index] + byte_offset_for_units(line, offset),
                end_offset: line_offsets[index] + byte_offset_for_units(line, offset) + text.len(),
            },
        });
        offset += slice_len;
    }
    windows
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

fn lines_to_window(
    lines: &[&str],
    line_offsets: &[usize],
    start_index: usize,
    end_index: usize,
) -> MarkdownWindow {
    MarkdownWindow {
        text: lines[start_index..=end_index].join("\n"),
        range: lines_to_range(lines, line_offsets, start_index, end_index),
    }
}

fn lines_to_range(
    lines: &[&str],
    line_offsets: &[usize],
    start_index: usize,
    end_index: usize,
) -> FragmentRange {
    FragmentRange {
        start_line: start_index + 1,
        end_line: end_index + 1,
        start_offset: line_offsets[start_index],
        end_offset: line_offsets[end_index] + lines[end_index].len(),
    }
}

/// Fence membership per line (port of `computeFenceLines`).
#[must_use]
pub fn compute_fence_lines(lines: &[&str]) -> Vec<bool> {
    let mut in_fence = vec![false; lines.len()];
    let mut fence: Option<&str> = None;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if let Some(open) = fence {
            in_fence[index] = true;
            if trimmed.starts_with(open) {
                fence = None;
            }
            continue;
        }
        if trimmed.starts_with("```") {
            fence = Some("```");
        } else if trimmed.starts_with("~~~") {
            fence = Some("~~~");
        }
    }
    in_fence
}

#[cfg(test)]
#[path = "../../tests/extract_markdown.rs"]
mod tests;
