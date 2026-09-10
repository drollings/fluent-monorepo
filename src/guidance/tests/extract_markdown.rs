//! P2 markdown-extraction tests: heading scan (ATX + setext, fences),
//! section nesting, outline majors, window budgets.

use super::*;
use crate::extractor::{ExtractSource, FragmentMetadata};
use crate::zg_types::FileKind;

fn source(text: &str) -> ExtractSource {
    ExtractSource {
        file_id: "f".to_string(),
        text: text.to_string(),
        format: "markdown".to_string(),
        kind: FileKind::Text,
    }
}

fn opts(max: usize, overlap: usize) -> ChunkOptions {
    ChunkOptions { max_chunk_chars: max, chunk_overlap_chars: overlap }
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
        Some(FragmentMetadata::Markdown { heading, level, scope }) => {
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
    let body = (0..40).map(|i| format!("paragraph {i} with enough words to fill the budget")).collect::<Vec<_>>().join("\n");
    let text = format!("# Big\n\n{body}\n");
    let frags = extract_markdown_fragments(&source(&text), opts(300, 30)).expect("extract");
    assert!(frags.len() >= 3, "{:?}", frags.len());
    let major = &frags[0];
    assert_eq!(major.group.as_deref(), Some(major.id.as_str()));
    assert_eq!(major.content_text, "Big");
    for window in &frags[1..] {
        assert_eq!(window.group.as_deref(), Some(major.id.as_str()), "{window:?}");
        assert!(crate::extractor::utf16_len(&window.content_text) <= 300, "{window:?}");
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
    assert!(extract_markdown_fragments(&src, opts(100, 10)).expect("extract").is_empty());
}
