//! Endpoint probe contract: `/models` shape, `+/v1` retry, fast failure.

use crate::probe::{probe_models, ProbeError};

#[test]
fn probe_reads_the_model_list() {
    let server = httpmock::MockServer::start();
    let mock = server.mock(|when, then| {
        when.method("GET").path("/models");
        then.status(200).json_body(serde_json::json!({
            "object": "list",
            "data": [{"id": "qwen3-test", "object": "model"}],
        }));
    });
    let probe = common_core::runtime::block_on_shared(probe_models(
        &server.base_url(),
        common_core::http::test_http_client(),
    ))
    .expect("probe succeeds");
    assert_eq!(probe.models, vec!["qwen3-test".to_string()]);
    assert!(!probe.via_v1_fallback);
    mock.assert_hits(1);
}

#[test]
fn probe_retries_with_v1_prefix() {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("GET").path("/models");
        then.status(404).body("nope");
    });
    let mock_v1 = server.mock(|when, then| {
        when.method("GET").path("/v1/models");
        then.status(200).json_body(serde_json::json!({
            "data": [{"id": "qwen3-test"}],
        }));
    });
    let probe = common_core::runtime::block_on_shared(probe_models(
        &server.base_url(),
        common_core::http::test_http_client(),
    ))
    .expect("v1 retry succeeds");
    assert!(probe.via_v1_fallback);
    mock_v1.assert_hits(1);
}

#[test]
fn probe_refused_is_unreachable_not_a_hang() {
    // A bound-then-dropped port refuses fast: the probe must report, fast.
    let port = crate::supervisor::free_port().expect("port");
    let base = format!("http://127.0.0.1:{port}");
    let started = std::time::Instant::now();
    let error = common_core::runtime::block_on_shared(probe_models(
        &base,
        common_core::http::test_http_client(),
    ))
    .expect_err("refused probe must fail");
    assert!(
        matches!(error, ProbeError::Unreachable(_, _)),
        "unexpected error: {error}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "probe must fail fast"
    );
}
