//! P2 text-extraction tests: window budgets, overlap, intra-line cuts,
//! char-boundary safety (ports `index-input-budget` chunk cases +
//! `code.test.mjs:327` surrogate rule).

use super::*;
use crate::extractor::ExtractSource;
use crate::search_types::FileKind;
use common_core::string::utf16_len;

fn source(text: &str) -> ExtractSource {
    ExtractSource {
        file_id: "f".to_string(),
        text: text.to_string(),
        format: "text".to_string(),
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
fn short_text_is_one_window() {
    let chunks = chunk_text("hello\nworld", opts(3600, 540));
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].text, "hello\nworld");
    assert_eq!(chunks[0].range.start_line, 1);
    assert_eq!(chunks[0].range.end_line, 2);
}

#[test]
fn blank_text_yields_no_chunks() {
    assert!(chunk_text("   \n  ", opts(100, 10)).is_empty());
}

#[test]
fn windows_respect_budget_and_overlap() {
    let text = (0..50)
        .map(|i| format!("line {i:02} padding padding"))
        .collect::<Vec<_>>()
        .join("\n");
    let chunks = chunk_text(&text, opts(140, 30));
    assert!(chunks.len() >= 2, "{}", chunks.len());
    for chunk in &chunks {
        assert!(utf16_len(&chunk.text) <= 140, "{chunk:?}");
        assert!(!chunk.text.trim().is_empty());
    }
    // Adjacent windows overlap (shared line).
    let first_lines: Vec<&str> = chunks[0].text.lines().collect();
    let second_lines: Vec<&str> = chunks[1].text.lines().collect();
    assert!(
        first_lines.iter().any(|l| second_lines.contains(l)),
        "overlap expected:\n{}\n---\n{}",
        chunks[0].text,
        chunks[1].text
    );
}

#[test]
fn long_lines_cut_at_scored_breaks() {
    // Sentence boundary inside the [0.7*max, max) scan window wins.
    let line = "01234567890123. rest here now ok!";
    let chunks = chunk_text(line, opts(20, 0));
    assert!(chunks.len() >= 2, "{chunks:?}");
    assert!(chunks[0].text.ends_with('.'), "{chunks:?}");
    // Joining the windows reproduces the line (overlap 0).
    assert_eq!(
        chunks.iter().map(|c| c.text.as_str()).collect::<String>(),
        line
    );
}

#[test]
fn chunk_boundaries_never_split_characters() {
    let emoji = "😀".repeat(80);
    let text = format!("export function emoji() {{ return \"{emoji}\"; }}");
    let chunks = chunk_text(&text, opts(100, 10));
    assert!(chunks.len() >= 2, "{chunks:?}");
    for chunk in &chunks {
        assert!(chunk.text.is_char_boundary(0));
        assert!(chunk.text.is_char_boundary(chunk.text.len()));
    }
    // Offsets stay byte-consistent for slicing.
    for chunk in &chunks {
        assert!(chunk.range.end_offset >= chunk.range.start_offset);
    }
}

#[test]
fn fragment_ids_come_from_positional_index() {
    let frags = extract_plain_text_fragments(&source("a\nb"), opts(3600, 540)).expect("extract");
    assert_eq!(frags.len(), 1);
    assert_eq!(frags[0].embedding_source(), "a\nb");
    assert!(frags[0].group.is_none());
}

#[test]
fn chunk_options_reject_bad_budgets() {
    assert!(ChunkOptions::resolve(Some(0), None).is_err());
    assert!(ChunkOptions::resolve(Some(100), Some(100)).is_err());
    assert!(ChunkOptions::resolve(Some(100), Some(10)).is_ok());
}

/// M1.1 characterization: `byte_offset_for_units` edge table.
/// Must stay byte-identical to the code/markdown copies (C1).
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

/// M1.1 characterization: `compute_next_start_line` edge table
/// (C3). Same backward accumulation as the code/markdown twins but
/// an absolute-index contract with a different clamp (no /2 cap:
/// full overlap returns `end`).
#[test]
fn compute_next_start_line_table() {
    let lines = ["aaa", "bb", "c"];
    assert_eq!(super::compute_next_start_line(&lines, 0, 3, 0), 3);
    assert_eq!(super::compute_next_start_line(&lines, 0, 3, 2), 2);
    assert_eq!(super::compute_next_start_line(&lines, 0, 3, 1), 2);
    assert_eq!(super::compute_next_start_line(&lines, 0, 3, 100), 3);
    assert_eq!(super::compute_next_start_line(&lines, 1, 2, 100), 2);
    assert_eq!(super::compute_next_start_line(&lines, 0, 0, 100), 0);
    let nine = ["a", "b", "c", "d", "e", "f", "g", "h", "i"];
    assert_eq!(super::compute_next_start_line(&nine, 0, 9, 5), 6);
    let heavy = ["aaaaaaaaaa", "b", "c", "d"];
    assert_eq!(super::compute_next_start_line(&heavy, 0, 4, 12), 4);
}

/// M1.1 characterization: `split_long_line` window boundaries (C4).
/// Same slide-while-`find_line_cut` algorithm as the markdown twin;
/// text skips whitespace-only windows (offset still advances).
#[test]
fn split_long_line_table() {
    let offsets = [0usize];
    let texts =
        |chunks: Vec<super::TextChunk>| chunks.into_iter().map(|c| c.text).collect::<Vec<_>>();
    let lines = ["hello world foo"];
    assert_eq!(
        texts(super::split_long_line(&lines, &offsets, 0, 5)),
        vec!["hello", " worl", "d foo"]
    );
    let lines = ["abcde"];
    assert_eq!(
        texts(super::split_long_line(&lines, &offsets, 0, 5)),
        vec!["abcde"]
    );
    let lines = ["abcdef"];
    assert_eq!(
        texts(super::split_long_line(&lines, &offsets, 0, 5)),
        vec!["abcde", "f"]
    );
    let lines = ["aaa. bbb. ccc"];
    assert_eq!(
        texts(super::split_long_line(&lines, &offsets, 0, 6)),
        vec!["aaa. ", "bbb. ", "ccc"]
    );
    let lines = ["ab   "];
    assert_eq!(
        texts(super::split_long_line(&lines, &offsets, 0, 2)),
        vec!["ab"]
    );
    let lines = ["h\u{e9}llo w\u{f6}rld"];
    let chunks = super::split_long_line(&lines, &offsets, 0, 5);
    assert_eq!(texts(chunks.clone()), vec!["h\u{e9}llo", " w\u{f6}rl", "d"]);
    assert_eq!(
        chunks
            .iter()
            .map(|c| (c.range.start_offset, c.range.end_offset))
            .collect::<Vec<_>>(),
        vec![(0, 6), (6, 12), (12, 13)]
    );
    assert!(chunks.iter().all(|c| c.range.start_line == 1));
    let lines = ["a\u{1f4bb}b"];
    assert_eq!(
        texts(super::split_long_line(&lines, &offsets, 0, 2)),
        vec!["a", "\u{1f4bb}b"]
    );
}
