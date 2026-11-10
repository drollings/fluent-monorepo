//! Approval tests: rounds resolve, deadlines deny, ambient and spoofed
//! answers never approve.

use crate::tools::approve::ApprovalHub;

#[test]
fn round_trip_answer() {
    let hub = ApprovalHub::new();
    let (id, recv) = hub.request("Run ls?".to_string());
    assert_eq!(hub.summary(&id).as_deref(), Some("Run ls?"));
    assert_eq!(hub.pending_count(), 1);
    assert!(hub.answer(&id, true));
    let answer = common_core::runtime::block_on(hub.wait(&id, recv, 1000));
    assert_eq!(answer, Some(true));
    assert_eq!(hub.pending_count(), 0);
}

#[test]
fn deadline_denies() {
    let hub = ApprovalHub::new();
    let (id, recv) = hub.request("Run rm?".to_string());
    let answer = common_core::runtime::block_on(hub.wait(&id, recv, 20));
    assert_eq!(answer, None);
}

#[test]
fn spoofed_ids_refuse() {
    let hub = ApprovalHub::new();
    assert!(!hub.answer("appr-9999", true));
    assert!(hub.summary("appr-9999").is_none());
}

#[test]
fn ambient_or_unverified_voice_never_approves() {
    for (verified, ambient) in [(false, false), (true, true), (false, true)] {
        let hub = ApprovalHub::new();
        let (id, recv) = hub.request("Run shell?".to_string());
        assert!(!hub.answer_voice(&id, verified, ambient));
        let answer = common_core::runtime::block_on(hub.wait(&id, recv, 50));
        assert_eq!(answer, Some(false));
    }
    let hub = ApprovalHub::new();
    let (id, recv) = hub.request("Run shell?".to_string());
    assert!(hub.answer_voice(&id, true, false));
    let answer = common_core::runtime::block_on(hub.wait(&id, recv, 50));
    assert_eq!(answer, Some(true));
}

#[test]
fn timeout_drains_pending_count() {
    let hub = ApprovalHub::new();
    let (id, recv) = hub.request("Run rm?".to_string());
    assert_eq!(hub.pending_count(), 1);
    let answer = common_core::runtime::block_on(hub.wait(&id, recv, 20));
    assert_eq!(answer, None);
    assert_eq!(hub.pending_count(), 0, "timed-out rounds must not leak");
    assert!(hub.summary(&id).is_none());
}

#[test]
fn late_answer_after_timeout_refuses() {
    let hub = ApprovalHub::new();
    let (id, recv) = hub.request("Run rm?".to_string());
    let answer = common_core::runtime::block_on(hub.wait(&id, recv, 20));
    assert_eq!(answer, None);
    assert!(
        !hub.answer(&id, true),
        "late answer on a drained round refuses"
    );
    assert_eq!(hub.pending_count(), 0);
}

#[test]
fn cancel_drains_pending_count() {
    let hub = ApprovalHub::new();
    let (id, _recv) = hub.request("Run rm?".to_string());
    assert_eq!(hub.pending_count(), 1);
    assert!(hub.cancel(&id));
    assert_eq!(hub.pending_count(), 0);
    assert!(!hub.answer(&id, true));
}
