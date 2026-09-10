//! Ported from zvec-grep `test/unit/models/catalog.test.mjs`.
//!
//! The pinned catalog is the single source of truth for embedding model
//! metadata: HF + ModelScope mirrors, per-artifact sha256/size, dims, metric,
//! batch/context/concurrency limits. Every entry below mirrors the upstream
//! table field-for-field so mirror drift is caught here, not at download.

use fluent_llm::catalog::{
    DEFAULT_QWEN_TEXT_EMBEDDING_ENDPOINT, EmbeddingBackend, get_embedding_model_catalog_entry,
    list_embedding_models,
};

#[test]
fn catalog_lists_all_fourteen_pinned_models() {
    let references: Vec<&str> = list_embedding_models()
        .iter()
        .map(|entry| entry.reference)
        .collect();
    assert_eq!(references.len(), 14);
    for expected in [
        "local/embeddinggemma-300m",
        "local/qwen3-embedding-0.6b",
        "qwen/text-embedding-v4",
        "qwen/qwen3.7-text-embedding",
        "qwen/qwen3-vl-embedding",
        "local/bge-small-en-v1.5",
        "local/all-minilm-l6-v2",
        "local/potion-retrieval-32m",
        "local/potion-multilingual-128m",
        "local/potion-code-16m-v2",
        "local/multilingual-e5-small",
        "local/jina-embeddings-v2-base-code",
        "local/gte-modernbert-base",
        "local/nomic-embed-text-v1.5",
    ] {
        assert!(
            references.contains(&expected),
            "catalog is missing {expected}"
        );
    }
}

#[test]
fn gguf_entries_pin_revisions_without_changing_cache_names() {
    let entry = get_embedding_model_catalog_entry("local/embeddinggemma-300m")
        .expect("embeddinggemma must be catalogued");
    assert_eq!(entry.backend, EmbeddingBackend::LlamaCpp);
    assert_eq!(entry.provider, "local");
    assert_eq!(
        entry.uri.as_deref(),
        Some("hf:ggml-org/embeddinggemma-300M-GGUF/embeddinggemma-300M-Q8_0.gguf#0f741b5a6585bd53aeb15cd1372c56f2a0f65e12")
    );
    assert_eq!(
        entry.cache_file.as_deref(),
        Some("hf_ggml-org_embeddinggemma-300M-Q8_0.gguf")
    );
    let sources = entry.sources.as_ref().expect("gguf entry must declare mirrors");
    assert_eq!(sources.hugging_face.0, "ggml-org/embeddinggemma-300M-GGUF");
    assert_eq!(
        sources.hugging_face.1,
        "0f741b5a6585bd53aeb15cd1372c56f2a0f65e12"
    );
    assert_eq!(sources.model_scope.0, "ggml-org/embeddinggemma-300M-GGUF");
    assert_eq!(
        sources.model_scope.1,
        "e2fab2963ac0943b1dc320cf0e267bd0e06e2e97"
    );
    assert_eq!(entry.artifacts.len(), 1);
    assert_eq!(entry.artifacts[0].path, "embeddinggemma-300M-Q8_0.gguf");
    assert_eq!(entry.artifacts[0].size, 333_590_944);
    assert_eq!(entry.dimension, 768);
    assert_eq!(entry.metric, "cosine");
    assert_eq!(entry.max_batch_size, 16);
}

#[test]
fn qwen_entries_carry_endpoints_and_token_limits() {
    let entry = get_embedding_model_catalog_entry("qwen/text-embedding-v4")
        .expect("qwen v4 must be catalogued");
    assert_eq!(entry.backend, EmbeddingBackend::Qwen);
    assert_eq!(
        entry.default_endpoint.as_deref(),
        Some(DEFAULT_QWEN_TEXT_EMBEDDING_ENDPOINT)
    );
    assert_eq!(entry.max_input_tokens, Some(8192));
    assert_eq!(entry.max_batch_size, 10);

    let vl = get_embedding_model_catalog_entry("qwen/qwen3-vl-embedding")
        .expect("qwen vl must be catalogued");
    assert_eq!(vl.dimension, 2560);
    assert_eq!(vl.max_image_bytes, Some(10 * 1024 * 1024));
}

#[test]
fn gte_uses_the_modelscope_iic_namespace() {
    let entry = get_embedding_model_catalog_entry("local/gte-modernbert-base")
        .expect("gte must be catalogued");
    let sources = entry.sources.as_ref().expect("onnx entry must declare mirrors");
    assert_eq!(sources.hugging_face.0, "Alibaba-NLP/gte-modernbert-base");
    assert_eq!(sources.model_scope.0, "iic/gte-modernbert-base");
}

#[test]
fn onnx_entries_carry_pooling_and_prefixes() {
    let bge = get_embedding_model_catalog_entry("local/bge-small-en-v1.5")
        .expect("bge must be catalogued");
    assert_eq!(bge.backend, EmbeddingBackend::TransformersJs);
    assert_eq!(bge.dimension, 384);
    assert_eq!(bge.artifacts.len(), 5);
    let total: u64 = bge.artifacts.iter().map(|artifact| artifact.size).sum();
    assert_eq!(total, 61_853_327);

    let e5 = get_embedding_model_catalog_entry("local/multilingual-e5-small")
        .expect("e5 must be catalogued");
    assert_eq!(
        e5.query_prefix.as_deref(),
        Some("query: "),
        "e5 query prefix must survive the port"
    );
    assert_eq!(e5.document_prefix.as_deref(), Some("passage: "));
}

#[test]
fn model2vec_entries_carry_tensor_and_tokenizer_files() {
    let entry = get_embedding_model_catalog_entry("local/potion-code-16m-v2")
        .expect("potion code must be catalogued");
    assert_eq!(entry.backend, EmbeddingBackend::Model2Vec);
    assert_eq!(entry.model_file.as_deref(), Some("model.safetensors"));
    assert_eq!(
        entry.embedding_tensor.as_deref(),
        Some("embeddings")
    );
    assert_eq!(entry.tokenizer_file.as_deref(), Some("tokenizer.json"));
    assert_eq!(entry.dimension, 256);
    assert_eq!(entry.default_concurrency, Some(2));
}

#[test]
fn unknown_reference_resolves_to_none() {
    assert!(get_embedding_model_catalog_entry("missing").is_none());
    assert!(get_embedding_model_catalog_entry("local/missing").is_none());
    assert!(get_embedding_model_catalog_entry("").is_none());
}

#[test]
fn every_artifact_sha_is_a_64_char_lowercase_hex_digest() {
    for entry in list_embedding_models() {
        for artifact in entry.artifacts {
            assert_eq!(
                artifact.sha256.len(),
                64,
                "{}/{} has a malformed digest",
                entry.reference,
                artifact.path
            );
            assert!(
                artifact.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "{}/{} digest is not hex",
                entry.reference,
                artifact.path
            );
            assert!(
                artifact.size > 0,
                "{}/{} must declare a positive size",
                entry.reference,
                artifact.path
            );
        }
    }
}
