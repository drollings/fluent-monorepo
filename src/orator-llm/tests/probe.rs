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

#[test]
fn probe_keeps_server_order_duplicates_and_skips_invalid_entries() {
    let server = httpmock::MockServer::start();
    let mock = server.mock(|when, then| {
        when.method("GET").path("/models");
        then.status(200).json_body(serde_json::json!({
            "data": [
                {"id": "b"},
                {"object": "model"},
                {"id": "a"},
                {"id": 7},
                {"id": "b"},
                {"id": "c"},
                {"id": null},
            ],
        }));
    });
    let probe = common_core::runtime::block_on_shared(probe_models(
        &server.base_url(),
        common_core::http::test_http_client(),
    ))
    .expect("probe succeeds");
    assert_eq!(
        probe.models,
        vec!["b".to_string(), "a".to_string(), "b".to_string(), "c".to_string()]
    );
    assert!(!probe.via_v1_fallback);
    mock.assert_hits(1);
}

/// Minimal path-dependent stub: each accepted connection gets the next
/// scripted reply (`None` closes without responding — a transport
/// failure). `Connection: close` framing forces one connection per
/// request so the two probe paths stay distinguishable.
fn stub_server(replies: Vec<Option<&'static [u8]>>) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    std::thread::spawn(move || {
        for reply in replies {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut head = [0u8; 4096];
            let _ = stream.read(&mut head);
            if let Some(bytes) = reply {
                let _ = stream.write_all(bytes);
            }
        }
    });
    base
}

#[test]
fn probe_reports_the_second_status_when_both_paths_refuse() {
    let server = httpmock::MockServer::start();
    let first = server.mock(|when, then| {
        when.method("GET").path("/models");
        then.status(404).body("nope");
    });
    let second = server.mock(|when, then| {
        when.method("GET").path("/v1/models");
        then.status(500).body("down");
    });
    let base = server.base_url();
    let error = common_core::runtime::block_on_shared(probe_models(
        &base,
        common_core::http::test_http_client(),
    ))
    .expect_err("both paths refuse");
    assert_eq!(
        error,
        ProbeError::BadStatus(
            base.clone(),
            "GET /v1/models → 500 Internal Server Error".to_string()
        )
    );
    first.assert_hits(1);
    second.assert_hits(1);
}

#[test]
fn probe_reports_the_transport_failure_after_a_first_status_refusal() {
    // `/models` refuses with a status; `/v1/models` dies mid-handshake.
    let base = stub_server(vec![
        Some(b"HTTP/1.1 404 Not Found\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnope"),
        None,
    ]);
    let error = common_core::runtime::block_on_shared(probe_models(
        &base,
        common_core::http::test_http_client(),
    ))
    .expect_err("second path fails transport");
    assert!(
        matches!(error, ProbeError::Unreachable(ref url, _) if url == &base),
        "later-path transport errors stay unsanitized: {error}"
    );
    assert!(
        !error.to_string().contains("connection failed"),
        "no first-path sanitizing on the later path: {error}"
    );
}

#[test]
fn probe_transport_failure_never_reaches_the_v1_path() {
    // First connection dies; the second reply would succeed if asked —
    // the sanitized refusal proves `/v1/models` was never requested.
    let base = stub_server(vec![
        None,
        Some(b"HTTP/1.1 200 OK\r\nContent-Length: 24\r\nConnection: close\r\n\r\n{\"data\":[{\"id\":\"x\"}]}"),
    ]);
    let error = common_core::runtime::block_on_shared(probe_models(
        &base,
        common_core::http::test_http_client(),
    ))
    .expect_err("refused probe must fail");
    assert_eq!(
        error,
        ProbeError::Unreachable(base, "connection failed".to_string())
    );
}
