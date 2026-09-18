//! LLM reranker response parsing: ordered candidate names, tolerant of fences.
//!
//! Seam re-export over the shared [`fluent_llm::parse::parse_rerank_output`]
//! codec composition: string-in/list-out with zero recall coupling. The
//! caller validates names against its own candidate set (a hallucinated name
//! is the caller's to drop, exactly like the ranking shortlist rule).
//! Query-specific rerank logic (candidate-set validation, score mapping)
//! belongs in this module when it lands — the raw parse stays shared.

pub use fluent_llm::parse::parse_rerank_output;
