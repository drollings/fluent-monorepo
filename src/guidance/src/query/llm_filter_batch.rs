//! Chunked synchronous scoring over one `LlmFilterBackend`.
//!
//! M11.2: stays sequential and stays here, not on `ResultPool`. This is a
//! chunking helper, not an execution batch — chunks run in order through a
//! synchronous backend with first-error-wins (`?` stops at the first
//! failing chunk) and a final relevance sort. `ResultPool` is async and
//! concurrent: migration would async-ify the API, parallelize backend
//! calls (changing error precedence), and add worker machinery for zero
//! production callers (only the unit tests below construct it). If a
//! production caller ever needs concurrent scoring, that caller composes
//! `ResultPool` directly with `NoopLlmFilter` as fail-open. Stays — pinned
//! by the empty/single/multi-batch tests.

use super::llm_filter::{LlmFilterBackend, LlmFilterError, RelevanceScore};

pub struct BatchLlmFilter {
    backend: Box<dyn LlmFilterBackend>,
    batch_size: usize,
}

impl BatchLlmFilter {
    pub fn new(backend: Box<dyn LlmFilterBackend>, batch_size: usize) -> Self {
        Self {
            backend,
            batch_size,
        }
    }

    pub fn score_batch(
        &self,
        query: &str,
        candidates: &[&str],
    ) -> Result<Vec<RelevanceScore>, LlmFilterError> {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let mut all_scores = Vec::with_capacity(candidates.len());

        for chunk in candidates.chunks(self.batch_size) {
            let scores = self.backend.score_relevance(query, chunk)?;
            all_scores.extend(scores);
        }

        all_scores.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(all_scores)
    }
}

#[cfg(test)]
mod tests {
    use super::super::llm_filter::NoopLlmFilter;
    use super::*;

    #[test]
    fn test_batch_filter_empty() {
        let batch = BatchLlmFilter::new(Box::new(NoopLlmFilter), 10);
        let results = batch.score_batch("test", &[]).expect("score");
        assert!(results.is_empty());
    }

    #[test]
    fn test_batch_filter_single_batch() {
        let batch = BatchLlmFilter::new(Box::new(NoopLlmFilter), 10);
        let candidates = vec!["fn hello()", "fn world()", "struct Config"];
        let results = batch.score_batch("hello", &candidates).expect("score");
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_batch_filter_multiple_batches() {
        let batch = BatchLlmFilter::new(Box::new(NoopLlmFilter), 2);
        let candidates: Vec<String> = (0..5).map(|i| format!("fn func_{i}()")).collect();
        let candidate_refs: Vec<&str> = candidates.iter().map(String::as_str).collect();
        let results = batch.score_batch("func", &candidate_refs).expect("score");
        assert_eq!(results.len(), 5);
    }
}
