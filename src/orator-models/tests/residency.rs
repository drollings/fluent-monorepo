//! Residency tests: eviction order, budget plans, lease sweep.

use crate::residency::{ResidencyManager, VoiceLeases, LEASE_SECS};

#[test]
fn big_cold_models_evict_before_small_hot_ones() {
    let mut manager = ResidencyManager::new(u64::MAX);
    manager.insert("tiny", 100, 900);
    manager.insert("huge", 8_000, 100);
    manager.insert("mid", 1_000, 950);
    let order = manager.eviction_order(1_000);
    assert_eq!(order[0], "huge");
    assert_eq!(order[order.len() - 1], "tiny");
}

#[test]
fn budget_plan_frees_until_fit() {
    let mut manager = ResidencyManager::new(1_200);
    manager.insert("a", 800, 10);
    manager.insert("b", 800, 20);
    manager.insert("c", 100, 1_000);
    let plan = manager.plan_evictions(1_001);
    assert!(!plan.is_empty());
    let freed: u64 = plan
        .iter()
        .map(|name| match *name {
            ref n if n == "a" => 800,
            ref n if n == "b" => 800,
            _ => 100,
        })
        .sum();
    assert!(manager.used_bytes().saturating_sub(freed) <= 1_200);
    // A just-used model is never first.
    assert_ne!(plan[0], "c");
}

#[test]
fn touch_refreshes_and_remove_drops() {
    let mut manager = ResidencyManager::new(u64::MAX);
    manager.insert("m", 500, 10);
    manager.touch("m", 999);
    assert_eq!(manager.eviction_order(1_000).last().map(String::as_str), Some("m"));
    manager.remove("m");
    assert_eq!(manager.used_bytes(), 0);
    assert!(manager.eviction_order(1_000).is_empty());
}

#[test]
fn leases_hold_then_release_on_sweep() {
    let mut leases = VoiceLeases::new();
    leases.acquire("kokoro-82m", 1_000);
    assert!(leases.held("kokoro-82m", 1_000 + LEASE_SECS - 1));
    assert!(!leases.held("kokoro-82m", 1_000 + LEASE_SECS));
    assert!(!leases.is_empty());
    let released = leases.sweep(1_000 + LEASE_SECS);
    assert_eq!(released, vec!["kokoro-82m".to_string()]);
    assert!(leases.is_empty());
    assert_eq!(leases.len(), 0);
}
