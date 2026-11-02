//! Summarizer tests: ordered map, citation gate, rejection.

use std::sync::Arc;

use crate::summarize::{citations_valid, extract_seg_citations, summarize, CitedSegment, ChunkSummarize};

struct Echo;

impl ChunkSummarize for Echo {
    fn summarize_chunk(&self, chunk: &str) -> String {
        format!("[seg-1] {chunk}")
    }

    fn merge(&self, summaries: &[String]) -> String {
        summaries.join("\n")
    }
}

struct Dangling;

impl ChunkSummarize for Dangling {
    fn summarize_chunk(&self, _chunk: &str) -> String {
        "[seg-99] invented".to_string()
    }

    fn merge(&self, summaries: &[String]) -> String {
        summaries.join("\n")
    }
}

fn known() -> Vec<CitedSegment> {
    vec![
        CitedSegment { id: "seg-1".to_string(), text: "hello".to_string() },
        CitedSegment { id: "seg-2".to_string(), text: "world".to_string() },
    ]
}

#[test]
fn citations_extract_in_order() {
    assert_eq!(
        extract_seg_citations("see [seg-2] and [seg-10] now"),
        vec!["seg-2".to_string(), "seg-10".to_string()]
    );
    assert!(extract_seg_citations("no citations here").is_empty());
    assert!(extract_seg_citations("broken [seg-]").is_empty());
}

#[test]
fn validity_needs_total_resolution() {
    assert!(citations_valid("[seg-1] fine", &known()));
    assert!(citations_valid("no citations", &known()));
    assert!(!citations_valid("[seg-1] plus [seg-99]", &known()));
}

#[test]
fn ordered_map_merges_and_validates() {
    let note = summarize(Arc::new(Echo), "hello world transcript", &known()).expect("valid note");
    assert!(note.contains("[seg-1]"));
    assert!(citations_valid(&note, &known()));
}

#[test]
fn dangling_citations_reject_the_note() {
    let error = summarize(Arc::new(Dangling), "hello", &known()).expect_err("must reject");
    assert_eq!(error, "note cites unknown segments");
}
