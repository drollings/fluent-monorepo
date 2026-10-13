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
fn none_and_all_bits_nan_hash_distinctly() {
    let all_bits_nan = f64::from_bits(u64::MAX);
    assert!(all_bits_nan.is_nan(), "all-bits-set is a NaN payload");
    assert_ne!(
        series_hash(&[None]),
        series_hash(&[Some(all_bits_nan)]),
        "a hole hashes distinctly from every resolved NaN payload"
    );
}

#[test]
fn none_vs_resolved_nan_is_a_content_change() {
    let all_bits_nan = f64::from_bits(u64::MAX);
    let mut cache = PaneCache::new();
    let key = cache_key("sentinel", 3);
    let first = cache.publish(key, vec![None]);
    let second = cache.publish(key, vec![Some(all_bits_nan)]);
    assert!(second > first, "hole vs resolved NaN republishes instead of backdating");
}

#[test]
fn distinct_nan_payloads_hash_distinctly_and_backdate() {
    const PAYLOADS: [u64; 4] =
        [0x7FF0_0000_0000_0001, 0x7FF8_0000_0000_0000, 0x7FFF_FFFF_FFFF_FFFF, 0xFFFF_FFFF_FFFF_FFFF];
    let hashes: Vec<u64> =
        PAYLOADS.iter().map(|bits| series_hash(&[Some(f64::from_bits(*bits))])).collect();
    for (index, first) in hashes.iter().enumerate() {
        for second in hashes.iter().skip(index + 1) {
            assert_ne!(first, second, "distinct NaN payloads hash distinctly");
        }
    }
    for bits in PAYLOADS {
        let nan = f64::from_bits(bits);
        assert!(nan.is_nan(), "{bits:#018x} is a NaN payload");
        let mut cache = PaneCache::new();
        let key = cache_key("nan", bits);
        let first = cache.publish(key, vec![Some(nan), None, Some(1.0)]);
        let second = cache.publish(key, vec![Some(nan), None, Some(1.0)]);
        assert_eq!(first, second, "bit-identical NaN payload {bits:#018x} backdates");
    }
}

const _: () = assert!(DEFAULT_PANE_CACHE_CAPACITY > 0, "the default holds a frame's working set");

#[test]
fn zero_capacity_refuses_with_reason() {
    let err = PaneCache::with_capacity(0).expect_err("zero capacity refuses");
    assert!(err.to_string().contains("capacity"), "refusal names capacity: {err}");
    assert_eq!(PaneCache::new().capacity(), DEFAULT_PANE_CACHE_CAPACITY);
}

#[test]
fn pinned_working_set_never_misses_across_a_publish_burst() {
    let mut cache = PaneCache::with_capacity(3).expect("non-zero capacity");
    let working = [cache_key("price", 1), cache_key("ribbon", 1)];
    assert!(working.len() < cache.capacity(), "working set fits with headroom");
    let price_rev = cache.publish(working[0], vec![Some(1.0), None]);
    let ribbon_rev = cache.publish(working[1], vec![Some(2.0), Some(3.0)]);
    for round in 0..10_u64 {
        cache.pin(&working);
        assert_eq!(cache.publish(working[0], vec![Some(1.0), None]), price_rev, "price backdates");
        assert_eq!(
            cache.publish(working[1], vec![Some(2.0), Some(3.0)]),
            ribbon_rev,
            "ribbon backdates"
        );
        cache.publish(cache_key("scratch", round), vec![Some(round as f64)]);
    }
    let price = cache.get(working[0]).expect("pinned price stays resident");
    assert_eq!(price.revision, price_rev);
    let ribbon = cache.get(working[1]).expect("pinned ribbon stays resident");
    assert_eq!(ribbon.revision, ribbon_rev);
}

#[test]
fn evicted_key_reads_as_miss_never_stale() {
    let mut cache = PaneCache::with_capacity(2).expect("non-zero capacity");
    let (first, second, third) = (cache_key("price", 1), cache_key("ribbon", 1), cache_key("delta", 1));
    let first_rev = cache.publish(first, vec![Some(1.0)]);
    cache.publish(second, vec![Some(2.0)]);
    cache.publish(third, vec![Some(3.0)]);
    assert!(cache.get(first).is_none(), "evicted key is a miss, never a stale revision");
    assert_eq!(cache.get(second).expect("resident").values, vec![Some(2.0)]);
    assert_eq!(cache.get(third).expect("resident").values, vec![Some(3.0)]);
    let revived = cache.publish(first, vec![Some(1.0)]);
    assert!(revived > first_rev, "republished key starts a fresh revision");
}

#[test]
fn eviction_is_deterministic_for_a_fixed_access_sequence() {
    fn run() -> (Option<u64>, Option<u64>, Option<u64>, usize) {
        let mut cache = PaneCache::with_capacity(2).expect("non-zero capacity");
        let (a, b, c) = (cache_key("a", 0), cache_key("b", 0), cache_key("c", 0));
        cache.publish(a, vec![Some(1.0)]);
        cache.publish(b, vec![Some(2.0)]);
        cache.pin(&[a]);
        cache.publish(c, vec![Some(3.0)]);
        let revision = |key| cache.get(key).map(|entry| entry.revision);
        (revision(a), revision(b), revision(c), cache.len())
    }
    let (a_rev, b_rev, c_rev, len) = run();
    assert_eq!((a_rev, b_rev, c_rev, len), run(), "same sequence evicts the same key");
    assert!(a_rev.is_some() && c_rev.is_some(), "pinned and fresh keys stay resident");
    assert!(b_rev.is_none(), "the untouched key is the one evicted");
    assert_eq!(len, 2, "bounded at capacity");
}

/// Calibration control (task value: residency): under churn past capacity,
/// every read is a miss or the last-published revision — never stale.
/// Survivors keep their exact revisions; the evicted key misses; the control
/// pair is unpinned churn (below) against the pinned working set.
#[test]
fn churn_past_capacity_never_serves_stale() {
    let mut cache = PaneCache::with_capacity(2).expect("non-zero capacity");
    let keys = [cache_key("a", 0), cache_key("b", 0), cache_key("c", 0)];
    let mut last_values: [Vec<Option<f64>>; 3] = [vec![], vec![], vec![]];
    let mut last_revisions = [0_u64; 3];
    for round in 0..12_u64 {
        let slot = round as usize % keys.len();
        let values = vec![Some(round as f64)];
        last_revisions[slot] = cache.publish(keys[slot], values.clone());
        last_values[slot] = values;
        for (index, key) in keys.iter().enumerate() {
            match cache.get(*key) {
                None => {}
                Some(entry) => {
                    assert_eq!(entry.values, last_values[index], "no stale values");
                    assert_eq!(entry.revision, last_revisions[index], "no stale revision");
                }
            }
        }
    }
    assert_eq!(cache.len(), 2, "bounded at capacity");
    // The first eviction drops the least-recently-used key only: `a`
    // published first is LRU once `b` lands, so `c` evicts `a` while `b`
    // keeps the revision its publish returned.
    let mut probe = PaneCache::with_capacity(2).expect("non-zero capacity");
    probe.publish(keys[0], vec![Some(1.0)]);
    let rev_b = probe.publish(keys[1], vec![Some(2.0)]);
    probe.publish(keys[2], vec![Some(3.0)]);
    assert!(probe.get(keys[0]).is_none(), "only the untouched key evicts");
    assert_eq!(probe.get(keys[1]).expect("resident").revision, rev_b);
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
