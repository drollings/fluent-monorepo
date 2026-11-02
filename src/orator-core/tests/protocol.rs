//! Unit tests for [`crate::protocol`]: stable discriminants, strict decode.

use crate::protocol::{validate_method, MessageType, METHOD_NAMES};

#[test]
fn discriminants_are_stable_on_the_wire() {
    assert_eq!(MessageType::Hello.to_u8(), 0);
    assert_eq!(MessageType::Audio.to_u8(), 1);
    assert_eq!(MessageType::Request.to_u8(), 2);
    assert_eq!(MessageType::Response.to_u8(), 3);
    assert_eq!(MessageType::Event.to_u8(), 4);
    assert_eq!(MessageType::Error.to_u8(), 5);
    assert_eq!(MessageType::Ping.to_u8(), 6);
    assert_eq!(MessageType::Subscribe.to_u8(), 7);
}

#[test]
fn every_family_round_trips() {
    for family in MessageType::ALL {
        assert_eq!(MessageType::from_u8(family.to_u8()), Some(family));
    }
}

#[test]
fn unknown_bytes_are_rejected_never_defaulted() {
    for byte in [8u8, 9, 42, 255] {
        assert_eq!(MessageType::from_u8(byte), None);
    }
}

#[test]
fn methods_validate_against_the_contract() {
    assert_eq!(METHOD_NAMES.len(), 11);
    for method in METHOD_NAMES {
        validate_method(method).expect("contract method must validate");
    }
    validate_method("StopDaemon").expect("lifecycle method validates");
    validate_method("Reload").expect("lifecycle method validates");
    validate_method("Nope").expect_err("unknown method must fail");
    validate_method("").expect_err("empty method must fail");
}
