//! Live ask: real endpoint, real inference. Skips cleanly without the
//! endpoint contract or an explicit opt-in — never fails a hermetic gate.

use orator::LlmSession;
use orator_agent::llm::HttpSession;

/// One question through the supervised endpoint. Requires `LLM_BASE_URL`
/// (e.g. `http://127.0.0.1:8080`) and `LLM_MODEL`; otherwise skips.
#[ignore = "needs a real llama-server endpoint; run via the live-ai target"]
#[test]
fn live_ask_returns_text() {
    let base = std::env::var("LLM_BASE_URL").unwrap_or_default();
    let model = std::env::var("LLM_MODEL").unwrap_or_default();
    if base.is_empty() || model.is_empty() {
        eprintln!("live: LLM_BASE_URL/LLM_MODEL unset, skipping");
        return;
    }
    common_core::runtime::block_on_shared(async {
        let probe = orator_agent::llm::probe_models(&base, common_core::http::test_http_client())
            .await
            .expect("live endpoint must answer its probe");
        assert!(
            probe.models.iter().any(|name| name == &model),
            "endpoint serves {:?}, want {model}",
            probe.models
        );
        let mut session = HttpSession::open(&base, &model, 8192, 512);
        session.push_text("Reply with exactly: pinecone");
        session
            .prefill(&[], &fluent_concurrency::stream::StreamAbort::new())
            .expect("empty prefill confirms");
        let out = session
            .step(&mut orator::Sampler::default())
            .expect("live step completes");
        assert!(out.finished);
        assert!(!out.delta.trim().is_empty(), "live reply must not be empty");
    });
}
