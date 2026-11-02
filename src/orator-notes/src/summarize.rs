//! Map-reduce summarizer with validated segment citations.
//!
//! The transcript splits into budgeted chunks (map), each chunk
//! summarizes through the seam in order, and the chunk summaries
//! merge into one note (reduce). Every cited segment id must resolve
//! against the known segment list or the note is rejected — a fluent
//! note with a dangling citation is worse than no note.

use std::sync::Arc;

use fluent_concurrency::pool::ResultPool;
use fluent_llm::tokens::{chunk_document, ChunkConfig};

/// Segment-id citation marker prefix.
pub const SEG_PREFIX: &str = "seg-";

/// One transcript segment available for citation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitedSegment {
    /// Segment id (`seg-N`).
    pub id: String,
    /// Segment text.
    pub text: String,
}

/// Extract `[seg-N]` citations from `note`, in order.
#[must_use]
pub fn extract_seg_citations(note: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = note;
    while let Some(open) = rest.find("[seg-") {
        let after = &rest[open + 5..];
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty() && after[digits.len()..].starts_with(']') {
            out.push(format!("{SEG_PREFIX}{digits}"));
            rest = &after[digits.len() + 1..];
        } else {
            rest = after;
        }
    }
    out
}

/// Whether every citation in `note` resolves against `known`. Empty
/// citation sets are valid (a note may cite nothing); dangling ids
/// reject the note.
#[must_use]
pub fn citations_valid(note: &str, known: &[CitedSegment]) -> bool {
    extract_seg_citations(note)
        .iter()
        .all(|id| known.iter().any(|segment| &segment.id == id))
}

/// Chunk-summarize seam. Real models implement this; tests echo.
pub trait ChunkSummarize: Send + Sync {
    /// Summarize `chunk` (already budgeted) into note prose.
    fn summarize_chunk(&self, chunk: &str) -> String;
    /// Merge ordered chunk summaries into one note.
    fn merge(&self, summaries: &[String]) -> String;
}

/// Summarize `transcript` over `known` segments: budgeted map in
/// order, merge reduce, then citation validation. Returns the note or
/// the rejection reason.
pub fn summarize(
    seam: Arc<dyn ChunkSummarize>,
    transcript: &str,
    known: &[CitedSegment],
) -> Result<String, String> {
    let budget = ChunkConfig::new(512, 64);
    let chunks = chunk_document(transcript, &budget);
    let chunks = if chunks.is_empty() {
        vec![transcript.to_string()]
    } else {
        chunks
    };
    let summaries = common_core::runtime::block_on(async {
        let pool = ResultPool::new(
            fluent_concurrency::tokio_runtime(),
            2,
            chunks.len().max(1),
            move |chunk: String| {
                let seam = Arc::clone(&seam);
                async move { Ok::<String, String>(seam.summarize_chunk(&chunk)) }
            },
        );
        let mut out = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            match pool.submit(chunk).await {
                Ok(summary) => out.push(summary),
                Err(error) => return Err(format!("map failed: {error}")),
            }
        }
        pool.shutdown().await;
        Ok(out)
    })?;
    let merged = summaries.join("\n");
    if !citations_valid(&merged, known) {
        return Err("note cites unknown segments".to_string());
    }
    Ok(merged)
}

#[cfg(test)]
#[path = "../tests/summarize.rs"]
mod tests;
