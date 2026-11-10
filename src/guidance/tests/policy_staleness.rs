//! Policy staleness: version change re-derives exactly affected items.

use super::{policy_fingerprint, policy_stale, MaintenancePolicy};

#[test]
fn same_policy_same_fingerprint() {
    let first = MaintenancePolicy::default();
    let second = MaintenancePolicy::default();
    assert_eq!(policy_fingerprint(&first), policy_fingerprint(&second));
}

#[test]
fn policy_content_change_moves_fingerprint() {
    let mut changed = MaintenancePolicy::default();
    changed.mechanical_classes.push("clippy-fix".to_string());
    assert_ne!(
        policy_fingerprint(&changed),
        policy_fingerprint(&MaintenancePolicy::default())
    );
}

#[test]
fn version_bump_rederives_unchanged_version_does_not() {
    let current = MaintenancePolicy { version: 2, ..MaintenancePolicy::default() };
    assert!(policy_stale(Some(1), &current));
    assert!(!policy_stale(Some(2), &current));
}

#[test]
fn missing_stored_version_is_stale() {
    assert!(policy_stale(None, &MaintenancePolicy::default()));
}

#[test]
fn defaults_pin_mechanical_rustfmt_and_three_attempts() {
    let policy = MaintenancePolicy::default();
    assert_eq!(policy.version, 1);
    assert_eq!(policy.mechanical_classes, vec!["rustfmt".to_string()]);
    assert_eq!(policy.attempt_limit, 3);
}
