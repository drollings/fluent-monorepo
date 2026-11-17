use super::*;

fn sample_request() -> RouterRequest {
    serde_json::from_value(serde_json::json!({
        "model": "fast",
        "messages": [{"role": "user", "content": "hello"}],
        "temperature": 0.7,
        "session_id": "s1",
    }))
    .expect("deserialize sample request")
}

#[test]
fn request_round_trip() {
    let req = sample_request();
    assert_eq!(req.model, "fast");
    assert_eq!(req.temperature, Some(0.7));
    let back: RouterRequest =
        serde_json::from_str(&serde_json::to_string(&req).expect("serialize")).expect("round trip");
    assert_eq!(back.model, "fast");
    assert_eq!(back.messages.len(), 1);
}

#[test]
fn missing_optionals_default_none() {
    let req: RouterRequest = serde_json::from_value(serde_json::json!({
        "model": "m",
        "messages": [],
    }))
    .expect("deserialize");
    assert!(req.temperature.is_none());
    assert!(req.tools.is_none());
    assert!(req.instance.is_none());
    assert!(req.metadata.is_empty());
}

#[test]
fn content_text_vs_parts() {
    let t = RouterMessageContent::Text("hi".into());
    assert_eq!(t.as_text(), "hi");
    assert_eq!(t.to_string_lossy(), "hi");
    let p = RouterMessageContent::Parts(vec![
        ContentPart::Text { text: "one".into() },
        ContentPart::ImageUrl {
            image_url: ImageUrl {
                url: "http://i".into(),
            },
        },
        ContentPart::Text { text: "two".into() },
    ]);
    assert_eq!(p.as_text(), "");
    assert_eq!(p.to_string_lossy(), "one two");
}

#[test]
fn tool_calls_empty_array_vs_none() {
    let empty: RouterMessage = serde_json::from_value(serde_json::json!({
        "role": "assistant", "content": "hi", "tool_calls": [],
    }))
    .expect("deserialize");
    assert!(empty.tool_calls.as_ref().is_some_and(Vec::is_empty));
    let absent: RouterMessage = serde_json::from_value(serde_json::json!({
        "role": "assistant", "content": "hi",
    }))
    .expect("deserialize");
    assert!(absent.tool_calls.is_none());
}

#[test]
fn unknown_fields_tolerated() {
    let req: RouterRequest = serde_json::from_value(serde_json::json!({
        "model": "m",
        "messages": [{"role": "user", "content": "hi", "future_key": 1}],
        "future_top_level": true,
    }))
    .expect("unknown fields tolerated");
    assert_eq!(req.messages.len(), 1);
}

#[test]
fn response_constructors() {
    let r = RouterResponse::text("m", "hi");
    assert_eq!(r.object, "chat.completion");
    assert_eq!(r.answer_text().as_deref(), Some("hi"));
    let e = RouterResponse::error("m", "boom");
    assert!(e.answer_text().is_some_and(|t| t.starts_with("ERROR: ")));
    assert_eq!(Usage::default().total_tokens, 0);
}
