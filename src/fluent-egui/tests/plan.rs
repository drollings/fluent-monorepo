use super::*;

fn position(order: &[String], node: &str) -> usize {
    order.iter().position(|n| n == node).unwrap_or_else(|| panic!("{node} ordered"))
}

/// Small domain-blind diamond: two roots, one join, one sink.
fn test_plan() -> PanePlan {
    let mut plan = PanePlan::new();
    plan.register_node("left", &[], 0).expect("left registers");
    plan.register_node("right", &[], 0).expect("right registers");
    plan.register_node("join", &["left", "right"], 0).expect("join registers");
    plan.register_node("sink", &["join"], 0).expect("sink registers");
    plan
}

#[test]
fn topo_order_puts_dependencies_first() {
    let plan = test_plan();
    let order = plan.order().expect("acyclic test plan");
    assert_eq!(order.len(), 4, "every pane ordered exactly once: {order:?}");
    assert!(position(&order, "left") < position(&order, "join"));
    assert!(position(&order, "right") < position(&order, "join"));
    assert!(position(&order, "join") < position(&order, "sink"));
}

#[test]
fn readiness_opens_as_panes_complete() {
    let mut plan = test_plan();
    assert_eq!(plan.ready_steps(), vec!["left".to_owned(), "right".to_owned()]);
    assert!(plan.is_ready("left"));
    assert!(!plan.is_ready("join"));
    plan.complete("left");
    assert!(!plan.is_ready("join"), "join still waits on right");
    plan.complete("right");
    assert!(plan.is_ready("join"));
    assert!(!plan.is_completed("join"));
    plan.complete("join");
    assert!(plan.is_ready("sink"));
}

#[test]
fn failing_a_node_dirties_only_transitive_dependents() {
    let plan = test_plan();
    let dirty = plan.dirty_set("join");
    assert_eq!(dirty, vec!["join".to_owned(), "sink".to_owned()], "node plus dependents");
    // Dirty sets follow plan order so waves reschedule deterministically.
    let order = plan.order().expect("ordered");
    let mut sorted = dirty.clone();
    sorted.sort_by_key(|node| position(&order, node));
    assert_eq!(dirty, sorted);
    // Failing a root dirties its whole downstream cone but never siblings.
    let root_dirty = plan.dirty_set("left");
    assert!(!root_dirty.contains(&"right".to_owned()), "right is independent of left");
    assert!(root_dirty.contains(&"left".to_owned()));
    assert!(root_dirty.contains(&"sink".to_owned()));
    // Cancellation scope excludes the failed node itself.
    assert!(!plan.cancel_dependents("join").contains(&"join".to_owned()));
}

#[test]
fn identical_recompute_keeps_the_revision() {
    let mut cache = PaneCache::new();
    assert!(cache.is_empty());
    let key = cache_key("ribbon", 42);
    let first = cache.publish(key, vec![Some(1.0), None, Some(3.0)]);
    let second = cache.publish(key, vec![Some(1.0), None, Some(3.0)]);
    assert_eq!(first, second, "identical content backdates");
    assert_eq!(cache.len(), 1);
    let bumped = cache.publish(key, vec![Some(1.0), None, Some(4.0)]);
    assert!(bumped > second, "changed content bumps the revision");
    let stored = cache.get(key).expect("cached");
    assert_eq!(stored.revision, bumped);
    assert_eq!(stored.values, vec![Some(1.0), None, Some(4.0)]);
}

#[test]
fn identical_nan_payloads_stay_cached() {
    const QUIET_NAN: f64 = f64::from_bits(0x7FF8_0000_0000_0000);
    assert!(QUIET_NAN.is_nan());
    let mut cache = PaneCache::new();
    let key = cache_key("fast", 7);
    let first = cache.publish(key, vec![Some(QUIET_NAN), None]);
    let second = cache.publish(key, vec![Some(QUIET_NAN), None]);
    assert_eq!(first, second, "bit-identical NaN payloads backdate instead of re-bumping");
}

#[test]
fn cache_keys_are_deterministic_and_sensitive() {
    let first = cache_key("ribbon", 0xA5A5);
    assert_eq!(first, cache_key("ribbon", 0xA5A5), "same inputs key identically");
    assert_ne!(first, cache_key("fast", 0xA5A5), "node names key distinctly");
    assert_ne!(first, cache_key("ribbon", 0x5A5A), "fingerprints key distinctly");
    let expected = {
        let mut bytes = Vec::with_capacity(8 + "ribbon".len() + 8);
        bytes.extend_from_slice(&("ribbon".len() as u64).to_le_bytes());
        bytes.extend_from_slice("ribbon".as_bytes());
        bytes.extend_from_slice(&0xA5A5_u64.to_le_bytes());
        common_core::hash::fnv1a64(&bytes)
    };
    assert_eq!(first, expected, "hashed once, length-framed");
}

#[test]
fn duplicate_registration_refuses_without_panicking() {
    let mut plan = test_plan();
    let err = plan.register_node("left", &[], 0).expect_err("duplicate pane refuses");
    assert!(err.to_string().contains("uplicate"), "refusal says duplicate: {err}");
    assert!(plan.checkpoint("no-such-pane").is_err());
    assert!(plan.rewind_to("no-such-pane").is_err());
    assert!(plan.set_fingerprint("no-such-pane", 1).is_err());
    assert!(!plan.contains("no-such-pane"));
    assert!(plan.contains("join"));
}

#[test]
fn checkpoints_rewind_the_suffix_for_replay() {
    let mut plan = test_plan();
    plan.complete("left");
    plan.complete("right");
    plan.checkpoint("join").expect("checkpoint names a pane");
    assert!(plan.is_checkpoint("join"));
    plan.complete("join");
    plan.complete("sink");
    let rerun = plan.rewind_to("join").expect("rewind replays the suffix");
    assert_eq!(rerun, vec!["join".to_owned(), "sink".to_owned()]);
    assert!(!plan.is_completed("join"), "suffix un-completes");
    assert!(!plan.is_completed("sink"), "suffix un-completes");
    assert!(plan.is_completed("right"), "prefix stays complete");
    assert!(!plan.is_checkpoint("join"), "checkpoints are single-use");
}

#[test]
fn fingerprints_round_trip_per_pane() {
    let mut plan = test_plan();
    assert_eq!(plan.fingerprint("join"), Some(0));
    plan.set_fingerprint("join", 99).expect("registered pane records");
    assert_eq!(plan.fingerprint("join"), Some(99));
    assert_eq!(plan.fingerprint("no-such-pane"), None);
}
