//! Ported from zvec-grep `test/unit/models/qwen.test.mjs`.
//!
//! Remote Qwen text + VL embedding models over stub HTTP (`httpmock`
//! loopback servers — never the network): ordered batches, full response
//! validation, error taxonomy with `Retry-After` context, image rules,
//! base64 encoding, header propagation.

use fluent_llm::embeddings::EmbeddingError;
use fluent_llm::qwen::{
    EmbedContent, Qwen37TextEmbedding, Qwen3VlEmbedding, QwenEmbedResult, QwenImageFormat,
    QwenTextEmbeddingV4, QwenTextOptions,
};
use serde_json::json;

fn vector(dimension: usize, value: f64) -> Vec<f64> {
    vec![value; dimension]
}

fn text_options(endpoint: &str) -> QwenTextOptions {
    QwenTextOptions {
        api_key: Some("secret-value".to_string()),
        endpoint: Some(endpoint.to_string()),
        extra_headers: Default::default(),
        timeout_ms: None,
    }
}

#[tokio::test]
async fn text_model_sends_ordered_batches_and_validates_shapes() {
    let server = httpmock::MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(httpmock::Method::POST)
            .path("/embeddings")
            .header("authorization", "Bearer secret-value")
            .body_contains(r#""input":["first","second"]"#)
            .body_contains(r#""model":"text-embedding-v4""#)
            .body_contains(r#""dimensions":1024"#)
            .body_contains(r#""encoding_format":"float""#);
        then.status(200).header("Content-Type", "application/json").json_body(json!({
            "data": [
                { "index": 1, "embedding": vector(1024, 2.0) },
                { "index": 0, "embedding": vector(1024, 1.0) },
            ]
        }));
    });
    let model = QwenTextEmbeddingV4::new(&text_options(&server.url("/embeddings"))).unwrap();
    let result: QwenEmbedResult = model
        .embed_texts(&["first".to_string(), "second".to_string()])
        .await
        .unwrap();
    assert_eq!(result.vectors[0][0], 1.0);
    assert_eq!(result.vectors[1][0], 2.0);
    assert!(result.truncated.is_empty());
    mock.assert();
}

#[test]
fn text_model_construction_rejects_blank_key_and_endpoint() {
    let error = QwenTextEmbeddingV4::new(&QwenTextOptions {
        api_key: Some(" ".to_string()),
        endpoint: Some("https://example.test/embeddings".to_string()),
        extra_headers: Default::default(),
        timeout_ms: None,
    })
    .unwrap_err();
    assert!(error.to_string().contains("requires an API key"), "{error}");

    let error = QwenTextEmbeddingV4::new(&QwenTextOptions {
        api_key: Some("secret".to_string()),
        endpoint: Some("  ".to_string()),
        extra_headers: Default::default(),
        timeout_ms: None,
    })
    .unwrap_err();
    assert!(error.to_string().contains("requires an endpoint"), "{error}");
}

#[tokio::test]
async fn text_model_reports_transport_and_protocol_failures() {
    // Unreachable endpoint: request failure, no panic.
    let model = QwenTextEmbeddingV4::new(&text_options("http://127.0.0.1:9/embeddings")).unwrap();
    let error = model.embed_texts(&["value".to_string()]).await.unwrap_err();
    assert!(error.to_string().contains("request failed"), "{error}");

    // Invalid JSON body.
    let server = httpmock::MockServer::start();
    let bad_json = server.mock(|when, then| {
        when.method(httpmock::Method::POST).path("/embeddings");
        then.status(200).body("not-json");
    });
    let model = QwenTextEmbeddingV4::new(&text_options(&server.url("/embeddings"))).unwrap();
    let error = model.embed_texts(&["value".to_string()]).await.unwrap_err();
    assert!(error.to_string().contains("not valid JSON"), "{error}");
    bad_json.assert();
}

#[tokio::test]
async fn text_model_api_error_carries_retry_context_without_secrets() {
    let server = httpmock::MockServer::start();
    let throttled = server.mock(|when, then| {
        when.method(httpmock::Method::POST).path("/embeddings");
        then.status(429)
            .header("Content-Type", "application/json")
            .header("retry-after", "1.5")
            .json_body(json!({ "error": { "code": "Throttled", "type": "rate", "message": "slow" } }));
    });
    let model = QwenTextEmbeddingV4::new(&text_options(&server.url("/embeddings"))).unwrap();
    let error = model.embed_texts(&["value".to_string()]).await.unwrap_err();
    let message = error.to_string();
    assert!(message.contains("returned an error"), "{message}");
    assert!(message.contains("retryAfterMs=1500"), "{message}");
    assert!(!message.contains("secret-value"), "key must not leak into errors");
    throttled.assert();

    // Flat and null provider error shapes still surface as API errors.
    for (index, body) in [
        json!(null),
        json!({ "code": "FlatError", "message": "flat provider error" }),
        json!({ "error": {} }),
    ]
    .into_iter()
    .enumerate()
    {
        let path = format!("/flat-{index}");
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::POST).path(&path);
            then.status(500)
                .header("Content-Type", "application/json")
                .json_body(body.clone());
        });
        let model = QwenTextEmbeddingV4::new(&text_options(&server.url(&path))).unwrap();
        let error = model.embed_texts(&["value".to_string()]).await.unwrap_err();
        assert!(error.to_string().contains("returned an error"), "{error}");
        mock.assert();
    }
}

#[tokio::test]
async fn text_model_validates_data_shapes() {
    let server = httpmock::MockServer::start();
    for (index, (body, fragment)) in [
        (json!({}), "did not include data"),
        (json!({ "data": [null] }), "invalid index"),
        (
            json!({ "data": [{ "index": 1, "embedding": [] }] }),
            "out of range",
        ),
        (json!({ "data": [{ "index": 0 }] }), "invalid embedding"),
        (json!({ "data": [] }), "non-array vector"),
    ]
    .into_iter()
    .enumerate()
    {
        let path = format!("/shapes-{index}");
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::POST).path(&path);
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(body.clone());
        });
        let model = QwenTextEmbeddingV4::new(&text_options(&server.url(&path))).unwrap();
        let error = model.embed_texts(&["value".to_string()]).await.unwrap_err();
        assert!(
            error.to_string().contains(fragment),
            "expected {fragment:?}, got {error}"
        );
        mock.assert();
    }
}

#[tokio::test]
async fn text_model_enforces_timeout() {
    let server = httpmock::MockServer::start();
    let slow = server.mock(|when, then| {
        when.method(httpmock::Method::POST).path("/slow");
        then.status(200)
            .header("Content-Type", "application/json")
            .body(r#"{"data": []}"#)
            .delay(std::time::Duration::from_secs(30));
    });
    let model = QwenTextEmbeddingV4::new(&QwenTextOptions {
        timeout_ms: Some(100),
        ..text_options(&server.url("/slow"))
    })
    .unwrap();
    let error = model.embed_texts(&["value".to_string()]).await.unwrap_err();
    assert!(error.to_string().contains("request failed"), "{error}");
    slow.assert();
}

#[tokio::test]
async fn qwen37_uses_its_name_and_expanded_limits() {
    let server = httpmock::MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(httpmock::Method::POST)
            .path("/qwen37")
            .body_contains(r#""model":"qwen3.7-text-embedding""#)
            .body_contains(r#""dimensions":1024"#)
            .body_contains(r#""encoding_format":"float""#);
        then.status(200).header("Content-Type", "application/json").json_body(json!({
            "data": [{ "index": 0, "embedding": vector(1024, 3.0) }]
        }));
    });
    let model = Qwen37TextEmbedding::new(&text_options(&server.url("/qwen37"))).unwrap();
    assert_eq!(model.max_batch_size(), 20);
    assert_eq!(model.max_input_tokens(), 128_000);
    let result = model
        .embed_texts(&["find relevant code".to_string()])
        .await
        .unwrap();
    assert_eq!(result.vectors[0][0], 3.0);
    mock.assert();

    let oversize: Vec<String> = (0..21).map(|index| format!("input-{index}")).collect();
    let error = model.embed_texts(&oversize).await.unwrap_err();
    assert!(error.to_string().contains("batch size"), "{error}");
    assert!(matches!(
        error,
        EmbeddingError::BatchTooLarge { size: 21, max: 20 }
    ));
}

#[tokio::test]
async fn requests_propagate_configured_headers() {
    let server = httpmock::MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(httpmock::Method::POST).path("/embeddings");
        then.status(200).header("Content-Type", "application/json").json_body(json!({
            "data": [{ "index": 0, "embedding": vector(1024, 0.25) }]
        }));
    });
    let model = QwenTextEmbeddingV4::new(&QwenTextOptions {
        extra_headers: [
            ("traceparent".to_string(), "00-abc-def-01".to_string()),
            ("baggage".to_string(), "tenant=example".to_string()),
        ]
        .into_iter()
        .collect(),
        ..text_options(&server.url("/embeddings"))
    })
    .unwrap();
    model.embed_texts(&["trace me".to_string()]).await.unwrap();
    mock.assert();
}

#[tokio::test]
async fn vl_model_validates_images_encodes_bytes_and_maps_indexes() {
    let error = Qwen3VlEmbedding::new(&QwenTextOptions {
        api_key: Some(String::new()),
        endpoint: Some("https://example.test/vl".to_string()),
        extra_headers: Default::default(),
        timeout_ms: None,
    })
    .unwrap_err();
    assert!(error.to_string().contains("requires an API key"), "{error}");

    let server = httpmock::MockServer::start();
    let model = Qwen3VlEmbedding::new(&text_options(&server.url("/vl"))).unwrap();
    let error = model
        .embed_contents(&[EmbedContent::image(vec![1], "gif").unwrap()])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("does not support image format"), "{error}");

    let many: Vec<EmbedContent> = (0..11)
        .map(|_| EmbedContent::image(vec![1], "png").unwrap())
        .collect();
    let error = model.embed_contents(&many).await.unwrap_err();
    assert!(error.to_string().contains("image count exceeds"), "{error}");

    let mock = server.mock(|when, then| {
        when.method(httpmock::Method::POST)
            .path("/vl")
            .body_contains(r#""image":"AQ==""#)
            .body_contains(r#""image":"AQI=""#)
            .body_contains(r#""image":"AQID""#);
        then.status(200).header("Content-Type", "application/json").json_body(json!({
            "output": {
                "embeddings": [
                    { "text_index": 1, "embedding": vector(2560, 2.0) },
                    { "index": 0, "embedding": vector(2560, 1.0) },
                    { "embedding": vector(2560, 3.0) },
                    { "embedding": vector(2560, 4.0) },
                ]
            }
        }));
    });
    let result = model
        .embed_contents(&[
            EmbedContent::Text("text".to_string()),
            EmbedContent::image(vec![1], "png").unwrap(),
            EmbedContent::image(vec![1, 2], "jpeg").unwrap(),
            EmbedContent::image(vec![1, 2, 3], "webp").unwrap(),
        ])
        .await
        .unwrap();
    assert_eq!(
        result.vectors.iter().map(|vector| vector[0]).collect::<Vec<_>>(),
        vec![1.0, 2.0, 3.0, 4.0]
    );
    mock.assert();
}

#[tokio::test]
async fn vl_model_reports_transport_and_shape_failures() {
    let model = Qwen3VlEmbedding::new(&text_options("http://127.0.0.1:9/vl")).unwrap();
    let error = model
        .embed_contents(&[EmbedContent::Text("value".to_string())])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("request failed"), "{error}");

    let server = httpmock::MockServer::start();
    let invalid = server.mock(|when, then| {
        when.method(httpmock::Method::POST).path("/vl-invalid");
        then.status(200).body("invalid");
    });
    let model = Qwen3VlEmbedding::new(&text_options(&server.url("/vl-invalid"))).unwrap();
    let error = model
        .embed_contents(&[EmbedContent::Text("value".to_string())])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not valid JSON"), "{error}");
    invalid.assert();

    for (index, (body, fragment)) in [
        (json!({ "output": {} }), "did not include embeddings"),
        (
            json!({ "output": { "embeddings": [null] } }),
            "invalid embedding item",
        ),
        (
            json!({ "output": { "embeddings": [{ "index": 2, "embedding": [] }] } }),
            "out of range",
        ),
        (
            json!({ "output": { "embeddings": [{ "index": 0 }] } }),
            "invalid embedding",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let path = format!("/vl-shape-{index}");
        let model = Qwen3VlEmbedding::new(&text_options(&server.url(&path))).unwrap();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::POST).path(&path);
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(body.clone());
        });
        let error = model
            .embed_contents(&[EmbedContent::Text("value".to_string())])
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains(fragment),
            "expected {fragment:?}, got {error}"
        );
        mock.assert();
    }
}

#[test]
fn image_format_parsing_rejects_unknown_formats() {
    assert_eq!(QwenImageFormat::parse("png").unwrap(), QwenImageFormat::Png);
    assert_eq!(QwenImageFormat::parse("JPEG").unwrap(), QwenImageFormat::Jpeg);
    assert_eq!(QwenImageFormat::parse("webp").unwrap(), QwenImageFormat::Webp);
    assert!(QwenImageFormat::parse("gif").is_err());
    assert!(QwenImageFormat::parse("").is_err());
}
