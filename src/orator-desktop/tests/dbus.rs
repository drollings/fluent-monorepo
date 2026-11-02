//! Control-API tests: method parity, locked-down signals, approval gate.

use std::collections::HashSet;

use orator_core::protocol::METHOD_NAMES;

use crate::dbus::{serves, Caller, SignalPolicy, PROPERTIES, SERVICE_NAME, SIGNALS};

#[test]
fn method_table_equals_the_wire_contract() {
    assert_eq!(SERVICE_NAME, "org.fluent.Orator1");
    let served: HashSet<&str> = METHOD_NAMES
        .into_iter()
        .filter(|method| serves(method))
        .collect();
    let wired: HashSet<&str> = METHOD_NAMES.into_iter().collect();
    assert_eq!(served, wired);
    assert!(!serves("Bogus"));
    assert!(!serves("Approve"));
}

#[test]
fn content_signals_stay_off_by_default() {
    let policy = SignalPolicy::locked_down();
    assert!(!policy.may_emit("Transcript"));
    assert!(!policy.may_emit("ReplyText"));
    assert!(policy.may_emit("StateChanged"));
    assert!(policy.may_emit("ApprovalRequested"));
    assert!(!policy.may_emit("Bogus"));
    assert_eq!(PROPERTIES.len(), 5);
    assert_eq!(SIGNALS.len(), 5);
}

#[test]
fn arbitrary_dbus_callers_cannot_approve() {
    assert!(Caller::Socket.may_approve());
    assert!(Caller::Notification.may_approve());
    assert!(!Caller::Dbus(":1.42".to_string()).may_approve());
    assert!(!Caller::Dbus("org.fluent.Orator1".to_string()).may_approve());
}
