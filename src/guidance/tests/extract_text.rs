//! P2 text-extraction tests: window budgets, overlap, intra-line cuts,
//! char-boundary safety (ports `index-input-budget` chunk cases +
//! `code.test.mjs:327` surrogate rule).

use super::*;
use crate::extractor::ExtractSource;
use crate::search_types::FileKind;

fn source(text: &str) -> ExtractSource {
    ExtractSource {
        file_id: "f".to_string(),
        text: text.to_string(),
        format: "text".to_string(),
        kind: FileKind::Text,
    }
}

fn opts(max: usize, overlap: usize) -> ChunkOptions {
    ChunkOptions { max_chunk_chars: max, chunk_overlap_chars: overlap }
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
    let text = (0..50).map(|i| format!("line {i:02} padding padding")).collect::<Vec<_>>().join("\n");
    let chunks = chunk_text(&text, opts(140, 30));
    assert!(chunks.len() >= 2, "{}", chunks.len());
    for chunk in &chunks {
        assert!(crate::extractor::utf16_len(&chunk.text) <= 140, "{chunk:?}");
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
    assert_eq!(chunks.iter().map(|c| c.text.as_str()).collect::<String>(), line);
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
