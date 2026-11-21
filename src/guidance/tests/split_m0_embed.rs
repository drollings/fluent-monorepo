//! Embed retry contract: fail-fast aborts, retryable exhausts.

use std::cell::Cell;

use super::{embed_retry_core, EmbedFailure, IndexOptions};
use fluent_llm::embeddings::{BatchEmbedding, EmbeddingError};

fn options(attempts: u32) -> IndexOptions {
    IndexOptions {
        retry_max_attempts: attempts,
        retry_base_ms: 1,
        ..IndexOptions::default()
    }
}

fn ok_batch() -> BatchEmbedding {
    BatchEmbedding {
        flat: vec![0.0; 4],
        count: 2,
        dims: 2,
    }
}

#[test]
fn embed_failfast_single_attempt() {
    let calls = Cell::new(0);
    let result = embed_retry_core(&options(3), None, || {
        calls.set(calls.get() + 1);
        Err(EmbeddingError::UnknownProvider("ghost".to_string()))
    });
    assert_eq!(calls.get(), 1, "fail-fast never retries");
    assert!(matches!(result, Err(EmbedFailure::FailFast(_))));
}

#[test]
fn embed_retryable_exhausts_budget() {
    let calls = Cell::new(0);
    let result = embed_retry_core(&options(2), None, || {
        calls.set(calls.get() + 1);
        Err(EmbeddingError::RequestFailed("boom".to_string()))
    });
    assert_eq!(calls.get(), 2, "retryable burns the budget");
    match result {
        Err(EmbedFailure::Retryable(failure)) => assert!(failure.unavailable),
        other => panic!("expected Retryable, got {other:?}"),
    }
}

#[test]
fn embed_success_takes_one_attempt() {
    let calls = Cell::new(0);
    let result = embed_retry_core(&options(3), None, || {
        calls.set(calls.get() + 1);
        Ok(ok_batch())
    });
    assert!(result.is_ok());
    assert_eq!(calls.get(), 1);
}
