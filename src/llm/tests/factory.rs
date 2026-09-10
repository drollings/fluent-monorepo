//! Ported from zvec-grep `test/unit/models/factory.test.mjs` (remote
//! dispatch) plus the resolution-order contract (flag > env > global default
//! > builtin local) and the model-or-endpoint rebuild rule: key/device
//! changes never force a rebuild.

use fluent_llm::embeddings::EmbeddingError;
use fluent_llm::factory::{
    CreateModelOptions, RebuildField, create_embedding_model, require_compatible_embedding,
    EmbeddingIdentity,
};
use fluent_llm::resolution::{ResolveEmbeddingReferenceOptions, resolve_embedding_reference};
use std::collections::HashMap;

fn qwen_options() -> CreateModelOptions {
    CreateModelOptions {
        api_key: Some("secret".to_string()),
        endpoint: Some("https://example.test/embeddings".to_string()),
        model_cache_dir: None,
        device: None,
        timeout_ms: None,
        extra_headers: HashMap::new(),
    }
}

#[test]
fn factory_resolves_remote_dispatch_and_rejects_unknown_models() {
    let provider = create_embedding_model("qwen/text-embedding-v4", &qwen_options()).unwrap();
    assert_eq!(provider.name(), "qwen");
    assert_eq!(provider.dimensions(), 1024);

    let provider =
        create_embedding_model("qwen/qwen3.7-text-embedding", &qwen_options()).unwrap();
    assert_eq!(provider.dimensions(), 1024);

    // Default endpoint applies without an explicit override.
    let provider = create_embedding_model(
        "qwen/text-embedding-v4",
        &CreateModelOptions {
            api_key: Some("secret".to_string()),
            endpoint: None,
            model_cache_dir: None,
            device: None,
            timeout_ms: None,
            extra_headers: HashMap::new(),
        },
    )
    .unwrap();
    assert_eq!(provider.dimensions(), 1024);

    for unknown in ["missing", "invalid", "unknown/missing", "local/missing"] {
        let error = match create_embedding_model(unknown, &qwen_options()) {
            Err(error) => error,
            Ok(_) => panic!("{unknown} must miss the catalog"),
        };
        assert!(
            matches!(error, EmbeddingError::CatalogNotFound(_)),
            "{unknown} must miss the catalog, got {error}"
        );
        assert!(error.to_string().contains(unknown), "{error}");
    }
}

#[test]
fn factory_builds_local_gguf_entries_with_unbound_runtime() {
    // Construction succeeds (reference valid, config valid); first use fails
    // fast with the missing-runtime error, mirroring the missing-dependency
    // path — the real runtime binds at the daemon layer.
    let provider = create_embedding_model(
        "local/embeddinggemma-300m",
        &CreateModelOptions {
            api_key: None,
            endpoint: None,
            model_cache_dir: Some(std::env::temp_dir()),
            device: Some("cpu".to_string()),
            timeout_ms: None,
            extra_headers: HashMap::new(),
        },
    )
    .unwrap();
    assert_eq!(provider.name(), "llama-cpp");
    assert_eq!(provider.dimensions(), 768);
}

#[test]
fn factory_reports_unwired_backends_without_confusing_them_with_unknowns() {
    // ONNX/model2vec entries are catalogued but constructed by their session
    // owners (daemon), not this factory.
    let error = match create_embedding_model("local/bge-small-en-v1.5", &qwen_options()) {
        Err(error) => error,
        Ok(_) => panic!("onnx must report unwired"),
    };
    assert!(
        matches!(error, EmbeddingError::BackendNotWired { .. }),
        "onnx must report unwired, got {error}"
    );
    let error = match create_embedding_model("local/potion-code-16m-v2", &qwen_options()) {
        Err(error) => error,
        Ok(_) => panic!("model2vec must report unwired"),
    };
    assert!(
        matches!(error, EmbeddingError::BackendNotWired { .. }),
        "model2vec must report unwired, got {error}"
    );
}

#[test]
fn resolution_order_flag_env_default_builtin() {
    let resolve = |explicit: Option<&str>, env: Option<&str>, default: Option<&str>| {
        resolve_embedding_reference(&ResolveEmbeddingReferenceOptions {
            explicit: explicit.map(str::to_string),
            existing: None,
            global_default: default.map(str::to_string),
            environment: Some(HashMap::from_iter(env.map(|value| {
                (
                    fluent_llm::resolution::EMBEDDING_ENVIRONMENT_VARIABLE.to_string(),
                    value.to_string(),
                )
            }))),
            fallback: Some(fluent_llm::catalog::DEFAULT_LOCAL_EMBEDDING.to_string()),
        })
        .unwrap()
    };
    assert_eq!(
        resolve(
            Some("qwen/text-embedding-v4"),
            Some("local/bge-small-en-v1.5"),
            Some("local/all-minilm-l6-v2")
        ),
        Some("qwen/text-embedding-v4".to_string())
    );
    assert_eq!(
        resolve(None, Some("local/bge-small-en-v1.5"), Some("local/all-minilm-l6-v2")),
        Some("local/bge-small-en-v1.5".to_string())
    );
    assert_eq!(
        resolve(None, None, Some("local/all-minilm-l6-v2")),
        Some("local/all-minilm-l6-v2".to_string())
    );
    assert_eq!(
        resolve(None, None, None),
        Some("local/potion-code-16m-v2".to_string())
    );
}

#[test]
fn rebuild_triggers_on_model_or_endpoint_change_only() {
    let existing = EmbeddingIdentity {
        provider: "qwen".to_string(),
        model: "text-embedding-v4".to_string(),
        dimension: 1024,
        metric: "cosine".to_string(),
        endpoint: Some("https://example.test/embeddings".to_string()),
    };
    // Identical: no rebuild.
    require_compatible_embedding(Some(&existing), &existing, "rebuild").unwrap();
    // No existing index: no rebuild.
    require_compatible_embedding(
        None,
        &existing,
        "rebuild",
    )
    .unwrap();

    // Model change (any of provider/model/dimension/metric) requires rebuild.
    let mut changed = existing.clone();
    changed.model = "qwen3.7-text-embedding".to_string();
    let error = require_compatible_embedding(Some(&existing), &changed, "rebuild").unwrap_err();
    assert_eq!(error.field, RebuildField::Model);
    assert!(error.to_string().contains("rebuild"), "{error}");

    let mut changed = existing.clone();
    changed.dimension = 768;
    let error = require_compatible_embedding(Some(&existing), &changed, "rebuild").unwrap_err();
    assert_eq!(error.field, RebuildField::Model);

    // Endpoint change requires rebuild.
    let mut changed = existing.clone();
    changed.endpoint = Some("https://other.test/embeddings".to_string());
    let error = require_compatible_embedding(Some(&existing), &changed, "rebuild").unwrap_err();
    assert_eq!(error.field, RebuildField::Endpoint);
    assert!(error.to_string().contains("rebuild"), "{error}");

    // Endpoint added where none existed requires rebuild too.
    let mut no_endpoint = existing.clone();
    no_endpoint.endpoint = None;
    let error =
        require_compatible_embedding(Some(&no_endpoint), &existing, "rebuild").unwrap_err();
    assert_eq!(error.field, RebuildField::Endpoint);
}
