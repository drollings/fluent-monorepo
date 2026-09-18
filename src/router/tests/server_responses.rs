use super::*;

#[test]
fn cors_headers_unchanged() {
    // M2.3: the router-owned CORS contract (moved from `common-core::http`).
    assert_eq!(CORS_ALLOW_ORIGIN, "*");
    assert_eq!(CORS_ALLOW_METHODS, "POST, GET, OPTIONS");
    assert_eq!(CORS_ALLOW_HEADERS, "Content-Type, Authorization");
    let mut headers = http::HeaderMap::new();
    add_cors_headers(&mut headers);
    assert_eq!(headers["access-control-allow-origin"], CORS_ALLOW_ORIGIN);
    assert_eq!(headers["access-control-allow-methods"], CORS_ALLOW_METHODS);
    assert_eq!(headers["access-control-allow-headers"], CORS_ALLOW_HEADERS);
}

#[test]
fn fallback_has_stop_reason() {
    let r = fallback_completion("test");
    assert_eq!(r.choices.len(), 1);
    assert_eq!(r.choices[0].finish_reason, "stop");
}

#[test]
fn answer_text_extracts_first_choice() {
    let c = make_text_completion("fast", "the answer");
    assert_eq!(answer_text(&c).as_deref(), Some("the answer"));
}

#[test]
fn answer_text_is_none_without_choices() {
    let c = RouterResponse {
        id: String::new(),
        object: "chat.completion".into(),
        created: 0,
        model: "fast".into(),
        choices: vec![],
        usage: Usage::default(),
    };
    assert_eq!(answer_text(&c), None);
}

#[test]
fn answer_text_concatenates_text_parts() {
    let mut c = make_text_completion("fast", "ignored");
    c.choices[0].message.content = RouterMessageContent::Parts(vec![
        fluent_types::openai_wire::ContentPart::Text {
            text: "hello".into(),
        },
        fluent_types::openai_wire::ContentPart::Text {
            text: "world".into(),
        },
    ]);
    assert_eq!(answer_text(&c).as_deref(), Some("hello world"));
}
