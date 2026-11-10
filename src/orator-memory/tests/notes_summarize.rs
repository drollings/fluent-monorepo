//! Summarizer tests: ordered map, citation gate, rejection.

use std::sync::Arc;

use crate::notes::summarize::{
    citations_valid, extract_seg_citations, summarize, ChunkSummarize, CitedSegment,
};

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
        CitedSegment {
            id: "seg-1".to_string(),
            text: "hello".to_string(),
        },
        CitedSegment {
            id: "seg-2".to_string(),
            text: "world".to_string(),
        },
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

/// M10-S5: citation grammar edges. Malformed shapes extract nothing
/// (never a partial id); an empty note cites nothing and stays valid;
/// any citation against an empty registry dangles.
#[test]
fn citation_edges_pin_malformed_and_empty() {
    assert!(extract_seg_citations("").is_empty());
    assert!(
        extract_seg_citations("[seg-1").is_empty(),
        "unclosed extracts nothing"
    );
    assert!(
        extract_seg_citations("[seg-x]").is_empty(),
        "non-numeric extracts nothing"
    );
    assert!(
        extract_seg_citations("[seg-1x]").is_empty(),
        "trailing junk extracts nothing"
    );
    assert!(
        extract_seg_citations("[[seg-1]]").len() == 1,
        "double-bracket still finds the id"
    );
    assert!(
        citations_valid("", &known()),
        "empty note cites nothing: valid"
    );
    assert!(
        !citations_valid("[seg-1]", &[]),
        "any citation dangles against nothing known"
    );
}
