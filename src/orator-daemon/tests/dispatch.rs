//! Unit tests for dispatch: the served set equals the contract, and only
//! implemented methods succeed.

use std::collections::HashSet;

use orator_core::protocol::METHOD_NAMES;

use crate::{contract_matches, dispatch, DISPATCH_METHODS, DispatchError};

#[test]
fn only_implemented_methods_dispatch() {
    for method in [
        "VoiceEndpoint",
        "StopDaemon",
        "StartSession",
        "StopSession",
        "Interrupt",
        "Say",
        "Ask",
        "Transcribe",
        "ToggleDictation",
        "ListSessions",
        "GetState",
        "Approve",
    ] {
        dispatch(method).unwrap_or_else(|_| panic!("{method} must dispatch"));
    }
    match dispatch("Reload") {
        Err(DispatchError::NotImplemented("Reload")) => {}
        other => panic!("Reload stays unimplemented, got {other:?}"),
    }
}

#[test]
fn unknown_methods_are_rejected() {
    dispatch("Nope").expect_err("unknown method must fail");
    dispatch("").expect_err("empty method must fail");
    match dispatch("Nope") {
        Err(DispatchError::Unknown(_)) => {}
        other => panic!("unknown must be Unknown, got {other:?}"),
    }
}

#[test]
fn short_ws_tokens_refuse_to_serve() {
    // Min 32 chars: short tokens fail closed before binding.
    for short in ["", "short", "test-token", &"x".repeat(31)] {
        let error = common_core::runtime::block_on(crate::ws::serve_ws(
            "127.0.0.1:0",
            short.to_string(),
            std::sync::Arc::new(crate::voice::VoiceOwner::disabled()),
            crate::ws::WsTuning::default(),
        ))
        .expect_err("short token must refuse");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
    // 32 chars serves (binds loopback, then stops on shutdown flag).
    crate::clear_shutdown_for_tests();
}

#[test]
fn bearer_compare_is_constant_time_correct() {
    assert!(crate::ws::constant_time_eq("Bearer abc", "Bearer abc"));
    assert!(!crate::ws::constant_time_eq("Bearer abc", "Bearer abd"));
    assert!(!crate::ws::constant_time_eq("short", "longer-token-here"));
    assert!(!crate::ws::constant_time_eq("", "Bearer x"));
}

#[test]
fn ignored_params_are_rejected_not_absorbed() {
    let voice = crate::voice::VoiceOwner::disabled();
    let (ok, body) = crate::socket::route_for_tests(
        &voice,
        "ListSessions",
        &serde_json::json!({ "sys": true }),
    );
    assert!(!ok, "ignored sys param must fail: {body}");
    let (ok, body) = crate::socket::route_for_tests(
        &voice,
        "GetState",
        &serde_json::json!({ "extra": 1 }),
    );
    assert!(!ok, "ignored extra param must fail: {body}");
    let (ok, body) = crate::socket::route_for_tests(
        &voice,
        "Say",
        &serde_json::json!({ "text": "hi", "bogus": 1 }),
    );
    assert!(!ok, "ignored bogus param must fail: {body}");
    voice.shutdown();
}

#[test]
fn served_set_equals_wire_contract() {
    assert!(contract_matches());
    let served: HashSet<&str> = DISPATCH_METHODS.into_iter().collect();
    let wired: HashSet<&str> = METHOD_NAMES.into_iter().collect();
    assert_eq!(served, wired);
    assert_eq!(DISPATCH_METHODS.len(), 13);
    assert_eq!(METHOD_NAMES.len(), 13);
}

#[test]
fn ask_failures_carry_a_structured_kind() {
    let voice = crate::voice::VoiceOwner::disabled();
    let (ok, body) = crate::socket::route_for_tests(
        &voice,
        "Ask",
        &serde_json::json!({ "question": "" }),
    );
    assert!(!ok, "disabled owner must refuse Ask: {body}");
    assert!(
        body.get("kind").and_then(|kind| kind.as_str()).is_some(),
        "Ask failures carry kind like Say/VoiceEndpoint: {body}"
    );
    voice.shutdown();
}

#[test]
fn say_failures_carry_a_structured_kind() {
    let voice = crate::voice::VoiceOwner::disabled();
    let (ok, body) = crate::socket::route_for_tests(
        &voice,
        "Say",
        &serde_json::json!({ "text": "" }),
    );
    assert!(!ok, "disabled owner must refuse Say: {body}");
    assert!(
        body.get("kind").and_then(|kind| kind.as_str()).is_some(),
        "Say failures carry kind: {body}"
    );
    voice.shutdown();
}

#[test]
fn approval_spoof_refuses_while_socket_and_notification_answer() {
    let voice = crate::voice::VoiceOwner::disabled();
    // Unknown approval id refuses regardless of caller.
    let (ok, body) = crate::socket::route_for_tests(
        &voice,
        "Approve",
        &serde_json::json!({ "id": "appr-9999", "approve": true }),
    );
    assert!(!ok, "unknown approval must refuse: {body}");
    // Live rounds answer through the socket route (trusted caller);
    // denials are ordinary answers, not usage errors.
    let (id, _recv) = voice.request_approval_for_tests("Run ls?".to_string());
    let (ok, body) = crate::socket::route_for_tests(
        &voice,
        "Approve",
        &serde_json::json!({ "id": id, "approve": true }),
    );
    assert!(ok, "socket approval answers: {body}");
    assert_eq!(body.get("approved"), Some(&serde_json::json!(true)));
    let (live, _recv) = voice.request_approval_for_tests("Run rm?".to_string());
    let (ok, body) = crate::socket::route_for_tests(
        &voice,
        "Approve",
        &serde_json::json!({ "id": live, "approve": false }),
    );
    assert!(ok, "socket denial is an ordinary answer: {body}");
    assert_eq!(body.get("approved"), Some(&serde_json::json!(false)));
    voice.shutdown();
}

#[test]
fn transcribe_route_validates_input_before_inference() {
    let voice = crate::voice::VoiceOwner::disabled();
    // Missing audio refuses without dialing.
    let (ok, body) = crate::socket::route_for_tests(
        &voice,
        "Transcribe",
        &serde_json::json!({}),
    );
    assert!(!ok, "missing audio must refuse");
    assert!(body.get("error").is_some(), "{body}");
    // Wrong rate refuses with the remedy.
    let (ok, body) = crate::socket::route_for_tests(
        &voice,
        "Transcribe",
        &serde_json::json!({"audio": "AA==", "rate": 8000}),
    );
    assert!(!ok, "wrong rate must refuse");
    assert!(
        body.get("error").is_some_and(|error| error.as_str().is_some_and(|text| text.contains("16 kHz"))),
        "rate refusal names the remedy: {body}"
    );
    // Foreign params refuse closed.
    let (ok, _) = crate::socket::route_for_tests(
        &voice,
        "Transcribe",
        &serde_json::json!({"audio": "AA==", "bogus": 1}),
    );
    assert!(!ok, "unknown params must refuse");
}
