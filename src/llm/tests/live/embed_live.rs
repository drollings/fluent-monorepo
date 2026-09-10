//! Opt-in live-AI test for the `llama:` embedding scheme.
//!
//! This test performs a REAL embedding call against a live endpoint. It is
//! compiled only when the `live-ai` feature is enabled and is `#[ignore]`d
//! so it can never run under `make test` / CI. Run it via `make test-live`
//! (or `make llm-test-live`).
//!
//! Env contract (extends `tests/live/README.md`):
//! - `LLM_EMBED_BASE_URL` — OpenAI-compatible embeddings base URL
//!   (e.g. `http://127.0.0.1:8080`).
//! - `LLM_EMBED_MODEL` — model name to request (e.g. `embed`).
//!
//! When either variable is absent the test skips cleanly (early `return`,
//! never panic). Assertions are structural only (well-formed vector with
//! the configured dims) — never embedding quality.

use fluent_llm::embeddings::create_embedding_provider;

/// `LLM_EMBED_BASE_URL` and `LLM_EMBED_MODEL` must both be set; otherwise `None`.
fn live_embed_env() -> Option<(String, String)> {
    let base = std::env::var("LLM_EMBED_BASE_URL").ok()?;
    let model = std::env::var("LLM_EMBED_MODEL").ok()?;
    Some((base, model))
}

#[test]
#[ignore = "live-AI: requires LLM_EMBED_BASE_URL + LLM_EMBED_MODEL; run via `make test-live`"]
fn llama_embed_round_trip_structural() {
    let Some((base, model)) = live_embed_env() else {
        eprintln!("LLM_EMBED_BASE_URL/LLM_EMBED_MODEL not set; skipping live embed test");
        return;
    };

    let provider = create_embedding_provider(
        &format!("llama:{model}"),
        None,
        Some(&base),
        None,
        768,
        None,
        None,
    )
    .expect("llama: scheme must build from a live base URL");

    let vector = provider
        .embed("the quick brown fox")
        .expect("live embed call should return Ok");

    // Structural invariants only: correct dimensionality, finite values.
    assert_eq!(vector.len(), 768, "embed model must return 768 dims");
    assert!(
        vector.iter().all(|v| v.is_finite()),
        "embedding values must be finite"
    );
    assert!(
        vector.iter().any(|v| *v != 0.0),
        "embedding must not be all zeros"
    );
}
