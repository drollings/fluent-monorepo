//! Ported from zvec-grep `resolution.ts` semantics
//! (`resolveEmbeddingReference`): the model-reference resolution order
//! flag > existing index > `GUIDANCE_EMBEDDING` > global default > builtin
//! local fallback. An environment value naming no catalog entry is a hard
//! error, never a silent fallback.

use fluent_llm::catalog::get_embedding_model_catalog_entry;
use fluent_llm::embeddings::EmbeddingError;
use fluent_llm::resolution::{resolve_embedding_reference, ResolveEmbeddingReferenceOptions};
use std::collections::HashMap;

fn options() -> ResolveEmbeddingReferenceOptions {
    ResolveEmbeddingReferenceOptions {
        explicit: None,
        existing: None,
        global_default: None,
        environment: Some(HashMap::new()),
        fallback: None,
    }
}

#[test]
fn explicit_wins_over_everything() {
    let mut opts = options();
    opts.explicit = Some("qwen/text-embedding-v4".to_string());
    opts.existing = Some("local/bge-small-en-v1.5".to_string());
    opts.global_default = Some("local/all-minilm-l6-v2".to_string());
    opts.environment = Some(HashMap::from([(
        "GUIDANCE_EMBEDDING".to_string(),
        "local/gte-modernbert-base".to_string(),
    )]));
    opts.fallback = Some("local/potion-code-16m-v2".to_string());
    assert_eq!(
        resolve_embedding_reference(&opts).unwrap(),
        Some("qwen/text-embedding-v4".to_string())
    );
}

#[test]
fn existing_index_beats_environment_and_defaults() {
    let mut opts = options();
    opts.existing = Some("local/bge-small-en-v1.5".to_string());
    opts.environment = Some(HashMap::from([(
        "GUIDANCE_EMBEDDING".to_string(),
        "local/gte-modernbert-base".to_string(),
    )]));
    opts.global_default = Some("local/all-minilm-l6-v2".to_string());
    opts.fallback = Some("local/potion-code-16m-v2".to_string());
    assert_eq!(
        resolve_embedding_reference(&opts).unwrap(),
        Some("local/bge-small-en-v1.5".to_string())
    );
}

#[test]
fn environment_beats_global_default_and_fallback() {
    let mut opts = options();
    opts.environment = Some(HashMap::from([(
        "GUIDANCE_EMBEDDING".to_string(),
        "  local/gte-modernbert-base  ".to_string(),
    )]));
    opts.global_default = Some("local/all-minilm-l6-v2".to_string());
    opts.fallback = Some("local/potion-code-16m-v2".to_string());
    assert_eq!(
        resolve_embedding_reference(&opts).unwrap(),
        Some("local/gte-modernbert-base".to_string())
    );
}

#[test]
fn blank_environment_value_is_ignored() {
    let mut opts = options();
    opts.environment = Some(HashMap::from([(
        "GUIDANCE_EMBEDDING".to_string(),
        "   ".to_string(),
    )]));
    opts.fallback = Some("local/potion-code-16m-v2".to_string());
    assert_eq!(
        resolve_embedding_reference(&opts).unwrap(),
        Some("local/potion-code-16m-v2".to_string())
    );
}

#[test]
fn invalid_environment_reference_is_a_hard_error() {
    let mut opts = options();
    opts.environment = Some(HashMap::from([(
        "GUIDANCE_EMBEDDING".to_string(),
        "bogus/model".to_string(),
    )]));
    opts.fallback = Some("local/potion-code-16m-v2".to_string());
    let error = resolve_embedding_reference(&opts).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("bogus/model"),
        "error must name the rejected value: {message}"
    );
    assert!(matches!(error, EmbeddingError::InvalidEmbeddingReference(_)));
}

#[test]
fn global_default_beats_fallback_and_none_without_fallback_is_none() {
    let mut opts = options();
    opts.global_default = Some("local/all-minilm-l6-v2".to_string());
    opts.fallback = Some("local/potion-code-16m-v2".to_string());
    assert_eq!(
        resolve_embedding_reference(&opts).unwrap(),
        Some("local/all-minilm-l6-v2".to_string())
    );

    let mut bare = options();
    bare.environment = Some(HashMap::new());
    assert_eq!(resolve_embedding_reference(&bare).unwrap(), None);
}

#[test]
fn every_resolution_result_names_a_catalog_entry() {
    // The resolver never invents references: explicit values pass through
    // (the factory rejects unknowns), everything else must be catalogued.
    for reference in [
        "local/potion-code-16m-v2",
        "qwen/text-embedding-v4",
        "local/bge-small-en-v1.5",
    ] {
        let mut opts = options();
        opts.environment = Some(HashMap::from([(
            "GUIDANCE_EMBEDDING".to_string(),
            reference.to_string(),
        )]));
        let resolved = resolve_embedding_reference(&opts).unwrap();
        assert_eq!(resolved.as_deref(), Some(reference));
        assert!(
            get_embedding_model_catalog_entry(reference).is_some(),
            "{reference} must be catalogued"
        );
    }
}
