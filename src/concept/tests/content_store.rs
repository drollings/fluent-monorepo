use super::*;
use fluent_types::{ContentNode, LOD0_FULL_TEXT};

fn node(content: &str, session: &str) -> ContentNode {
    ContentNode {
        lod: vec![content.to_string()],
        session_id: Some(session.to_string()),
        ..Default::default()
    }
}

#[test]
fn insert_applies_eager_fill_and_stamps_hash() {
    let store = InMemoryContentNodeStore::new();
    let id = store.insert(node("Hello world. Second sentence.", "s"));
    let got = store.snapshot(id).expect("present");
    assert_eq!(got.id, Some(id));
    assert_eq!(
        got.lod[LOD0_FULL_TEXT as usize],
        "Hello world. Second sentence."
    );
    assert!(!got.lod[LOD5_LABEL as usize].is_empty(), "LOD5 derived");
    assert!(!got.content_hash.is_unstamped(), "non-empty LOD0 stamps");
    assert_eq!(got.active_lod, Some(LOD0_FULL_TEXT));
}

#[test]
fn empty_content_stays_unstamped_without_label() {
    let store = InMemoryContentNodeStore::new();
    let id = store.insert(node("", "s"));
    let got = store.snapshot(id).expect("present");
    assert!(got.content_hash.is_unstamped());
    assert_eq!(store.lod_text(id, LOD0_FULL_TEXT), None);
}

#[test]
fn lazy_tiers_read_cached_or_absent_never_derived() {
    let store = InMemoryContentNodeStore::new();
    let id = store.insert(node("Full text here.", "s"));
    assert_eq!(store.lod_text(id, 2), None, "uncached lazy tier is absent");
    store.set_tier(id, 2, "cached summary");
    assert_eq!(store.lod_text(id, 2).as_deref(), Some("cached summary"));
    // Markers survive: the double never rewrites tiers on read.
    assert_eq!(
        store.lod_text(id, LOD0_FULL_TEXT).as_deref(),
        Some("Full text here.")
    );
}

#[test]
fn session_ids_in_insertion_order() {
    let store = InMemoryContentNodeStore::new();
    let a = store.insert(node("a", "s1"));
    let b = store.insert(node("b", "s2"));
    let c = store.insert(node("c", "s1"));
    assert_eq!(store.session_node_ids("s1"), vec![a, c]);
    assert_eq!(store.session_node_ids("s2"), vec![b]);
    assert!(store.session_node_ids("nope").is_empty());
    assert!(store.snapshot(NodeId::from_int(999)).is_none());
}

#[test]
fn derive_label_edges() {
    assert_eq!(derive_label("assistant", ""), "assistant");
    assert_eq!(derive_label("", ""), "");
    assert_eq!(derive_label("user", "Hello world. Rest."), "Hello world.");
}

#[test]
fn store_recovers_after_lock_poison_instead_of_panicking() {
    use std::panic::AssertUnwindSafe;
    let store = InMemoryContentNodeStore::new();
    let before = store.insert(node("before poison", "s"));
    assert!(store.snapshot(before).is_some());
    // Poison the shard by panicking while holding its write guard.
    let poisoned = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let _guard = store.nodes.write().unwrap();
        panic!("intentional poison");
    }));
    assert!(poisoned.is_err(), "poison attempt must panic");
    // The store recovers (poison-tolerant locking) instead of panicking.
    let after = store.insert(node("after poison", "s"));
    assert!(store.snapshot(after).is_some(), "writes work after poison");
    assert!(
        store.snapshot(before).is_some(),
        "old reads work after poison"
    );
    assert_eq!(store.session_node_ids("s").len(), 2);
}
