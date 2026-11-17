//! P2 markdown-extraction tests: heading scan (ATX + setext, fences),
//! section nesting, outline majors, window budgets.

use super::*;
use crate::extractor::{ExtractSource, FragmentMetadata};
use crate::search_types::FileKind;

fn source(text: &str) -> ExtractSource {
    ExtractSource {
        file_id: "f".to_string(),
        text: text.to_string(),
        format: "markdown".to_string(),
        kind: FileKind::Text,
    }
}

fn opts(max: usize, overlap: usize) -> ChunkOptions {
    ChunkOptions {
        max_chunk_chars: max,
        chunk_overlap_chars: overlap,
    }
}

#[test]
fn headings_scan_skips_fences() {
    let lines = [
        "# Top",
        "",
        "```",
        "# not a heading",
        "```",
        "## Sub",
        "Setext",
        "======",
    ];
    let headings = scan_headings(&lines);
    assert_eq!(headings.len(), 3, "{headings:?}");
    assert_eq!(headings[0].text, "Top");
    assert_eq!(headings[0].level, 1);
    assert_eq!(headings[1].text, "Sub");
    assert_eq!(headings[2].text, "Setext");
    assert_eq!(headings[2].level, 1);
}

#[test]
fn sections_nest_with_breadcrumbs() {
    let lines = ["# A", "x", "## B", "y", "# C", "z"];
    let headings = scan_headings(&lines);
    let sections = build_sections(&headings, &lines);
    assert_eq!(sections.len(), 3, "{sections:?}");
    assert!(sections[0].breadcrumb.is_empty());
    assert_eq!(sections[1].breadcrumb, vec!["A".to_string()]);
    assert_eq!(sections[1].start_index, 2);
    assert_eq!(sections[1].end_index, 3);
}

#[test]
fn preamble_becomes_its_own_section() {
    let lines = ["intro", "", "# A", "x"];
    let headings = scan_headings(&lines);
    let sections = build_sections(&headings, &lines);
    assert_eq!(sections.len(), 2);
    assert!(sections[0].heading.is_none());
    assert_eq!(sections[0].end_index, 1);
}

#[test]
fn small_sections_emit_single_fragments_with_metadata() {
    let frags = extract_markdown_fragments(&source("# Guide\n\nHello world.\n"), opts(3600, 540))
        .expect("extract");
    assert_eq!(frags.len(), 1, "{frags:?}");
    match &frags[0].metadata {
        Some(FragmentMetadata::Markdown {
            heading,
            level,
            scope,
        }) => {
            assert_eq!(heading.as_deref(), Some("Guide"));
            assert_eq!(*level, Some(1));
            assert!(scope.is_none());
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(frags[0].range.start_line, 1);
}

#[test]
fn oversize_sections_emit_outline_major_plus_windows() {
    let body = (0..40)
        .map(|i| format!("paragraph {i} with enough words to fill the budget"))
        .collect::<Vec<_>>()
        .join("\n");
    let text = format!("# Big\n\n{body}\n");
    let frags = extract_markdown_fragments(&source(&text), opts(300, 30)).expect("extract");
    assert!(frags.len() >= 3, "{:?}", frags.len());
    let major = &frags[0];
    assert_eq!(major.group.as_deref(), Some(major.id.as_str()));
    assert_eq!(major.content_text, "Big");
    for window in &frags[1..] {
        assert_eq!(
            window.group.as_deref(),
            Some(major.id.as_str()),
            "{window:?}"
        );
        assert!(
            crate::extractor::utf16_len(&window.content_text) <= 300,
            "{window:?}"
        );
    }
    // Ids are unique positional hashes.
    let mut ids: Vec<&str> = frags.iter().map(|f| f.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), frags.len());
}

#[test]
fn non_markdown_source_yields_nothing() {
    let mut src = source("# x");
    src.format = "text".to_string();
    assert!(extract_markdown_fragments(&src, opts(100, 10))
        .expect("extract")
        .is_empty());
}

/// M1.1 characterization: `byte_offset_for_units` edge table.
/// Must stay byte-identical to the code/text copies (C1).
#[test]
fn byte_offset_for_units_table() {
    let cases: &[(&str, usize, usize)] = &[
        ("", 0, 0),
        ("hello", 0, 0),
        ("hello", 2, 2),
        ("hello", 5, 5),
        ("hello", 99, 5),
        ("h\u{e9}llo", 2, 3),
        ("a\u{1f4bb}b", 2, 5),
    ];
    for (line, units, want) in cases {
        assert_eq!(
            crate::extractor::byte_offset_for_units(line, *units),
            *want,
            "line={line:?} units={units}"
        );
    }
    assert_eq!(crate::extractor::byte_offset_for_units("a\u{1f4bb}b", 4), 6);
}

/// M1.1 characterization: `compute_markdown_overlap_lines` edge
/// table (C3). Must stay identical to the code twin (the /2 cap
/// masks their one-line boundary difference on every input here).
#[test]
fn compute_markdown_overlap_lines_table() {
    let lines = ["aaa", "bb", "c"];
    assert_eq!(super::compute_markdown_overlap_lines(&lines, 0, 3, 0), 0);
    assert_eq!(super::compute_markdown_overlap_lines(&lines, 0, 3, 2), 1);
    assert_eq!(super::compute_markdown_overlap_lines(&lines, 0, 3, 1), 0);
    assert_eq!(
        super::compute_markdown_overlap_lines(&lines, 0, 3, 100),
        1
    );
    assert_eq!(
        super::compute_markdown_overlap_lines(&lines, 1, 2, 100),
        0
    );
    assert_eq!(
        super::compute_markdown_overlap_lines(&lines, 0, 0, 100),
        0
    );
    let nine = ["a", "b", "c", "d", "e", "f", "g", "h", "i"];
    assert_eq!(super::compute_markdown_overlap_lines(&nine, 0, 9, 5), 2);
    let heavy = ["aaaaaaaaaa", "b", "c", "d"];
    assert_eq!(
        super::compute_markdown_overlap_lines(&heavy, 0, 4, 12),
        2
    );
}

/// M1.1 characterization: `split_long_md_line` window boundaries
/// (C4). Same slide-while-`find_line_cut` algorithm as the text twin;
/// markdown keeps whitespace-only windows (no trim-skip).
#[test]
fn split_long_md_line_table() {
    let offsets = [0usize];
    let texts = |windows: Vec<super::MarkdownWindow>| {
        windows.into_iter().map(|w| w.text).collect::<Vec<_>>()
    };
    let lines = ["hello world foo"];
    assert_eq!(
        texts(super::split_long_md_line(&lines, &offsets, 0, 5)),
        vec!["hello", " worl", "d foo"]
    );
    let lines = ["abcde"];
    assert_eq!(
        texts(super::split_long_md_line(&lines, &offsets, 0, 5)),
        vec!["abcde"]
    );
    let lines = ["abcdef"];
    assert_eq!(
        texts(super::split_long_md_line(&lines, &offsets, 0, 5)),
        vec!["abcde", "f"]
    );
    let lines = ["aaa. bbb. ccc"];
    assert_eq!(
        texts(super::split_long_md_line(&lines, &offsets, 0, 6)),
        vec!["aaa. ", "bbb. ", "ccc"]
    );
    let lines = ["ab   "];
    assert_eq!(
        texts(super::split_long_md_line(&lines, &offsets, 0, 2)),
        vec!["ab", "  ", " "]
    );
    let lines = ["h\u{e9}llo w\u{f6}rld"];
    let windows = super::split_long_md_line(&lines, &offsets, 0, 5);
    assert_eq!(
        texts(windows.clone()),
        vec!["h\u{e9}llo", " w\u{f6}rl", "d"]
    );
    assert_eq!(
        windows
            .iter()
            .map(|w| (w.range.start_offset, w.range.end_offset))
            .collect::<Vec<_>>(),
        vec![(0, 6), (6, 12), (12, 13)]
    );
    assert!(windows.iter().all(|w| w.range.start_line == 1));
    let lines = ["a\u{1f4bb}b"];
    assert_eq!(
        texts(super::split_long_md_line(&lines, &offsets, 0, 2)),
        vec!["a", "\u{1f4bb}b"]
    );
    let lines = ["hello world foo"];
    let windows = super::split_long_md_line(&lines, &[10], 0, 5);
    assert_eq!(windows[0].range.start_offset, 10);
    assert_eq!(windows[1].range.start_offset, 15);
}
