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
        "ToggleDictation",
        "ListSessions",
        "GetState",
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
    assert_eq!(DISPATCH_METHODS.len(), 11);
    assert_eq!(METHOD_NAMES.len(), 11);
}
