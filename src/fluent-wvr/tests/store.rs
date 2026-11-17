#![allow(unused_imports)]
use fluent_wvr::prelude::*;
#[allow(unused_imports)]
use fluent_wvr::store::*;
use fluent_wvr::{Component, Describable, FieldAccess, FieldError, WorkUnit};

#[test]
fn store_roundtrips_typed_values_without_serde() {
    let store = OutputStore::default();
    store.set("count", 42_i64);
    store.set("label", "hello".to_string());
    store.set("flag", true);

    assert_eq!(store.get::<i64>("count").as_deref(), Some(&42));
    assert_eq!(
        store.get::<String>("label").as_deref(),
        Some(&"hello".to_string())
    );
    assert_eq!(store.get::<bool>("flag").as_deref(), Some(&true));
    assert_eq!(store.len(), 3);
}

#[test]
fn store_type_mismatch_and_missing_key_yield_none() {
    let store = OutputStore::default();
    store.set("count", 42_i64);

    assert_eq!(store.get::<i64>("count").as_deref(), Some(&42));
    assert_eq!(store.get::<u32>("count"), None, "wrong T must be None");
    assert_eq!(store.get::<i64>("missing"), None);
    assert!(store.contains_key("count"));
    assert!(!store.contains_key("missing"));
}

#[test]
fn store_overwrite_and_remove() {
    let store = OutputStore::default();
    store.set("x", 1_i32);
    store.set("x", 2_i32);
    assert_eq!(store.get::<i32>("x").as_deref(), Some(&2));

    assert!(store.remove("x"));
    assert!(!store.contains_key("x"));
    assert!(!store.remove("x"));
    assert!(store.is_empty());
}

#[test]
fn store_clone_shares_the_map() {
    // Shared-handoff semantics: a unit's write through its context clone
    // is visible to the registrant's clone (no snapshot isolation).
    let store = OutputStore::default();
    store.set("count", 42_i64);
    let cloned = store.clone();
    assert_eq!(cloned.get::<i64>("count").as_deref(), Some(&42));
    cloned.set("count", 43_i64);
    assert_eq!(
        store.get::<i64>("count").as_deref(),
        Some(&43),
        "writes through a clone are visible to the original"
    );
    cloned.remove("count");
    assert_eq!(store.get::<i64>("count"), None);
}

#[test]
fn store_debug_lists_keys() {
    let store = OutputStore::default();
    store.set("k", 1_u8);
    let debug = format!("{store:?}");
    assert!(debug.contains("k"), "debug should list keys, got: {debug}");
}

#[test]
fn store_clear_empties_through_shared_refs() {
    let store = OutputStore::default();
    store.set("k", 1_u8);
    let cloned = store.clone();
    store.clear();
    assert!(store.is_empty());
    assert!(cloned.is_empty(), "clear is visible through clones");
}

#[test]
fn stored_arc_handle_reads_back_double_wrapped() {
    use std::sync::{Arc, Mutex};
    // Discipline pin: the store wraps every value in an `Arc`, so a stored
    // shared handle (`Arc<Mutex<_>>`) reads back double-wrapped. Consumers
    // of shared handles must `get` the `Arc` type and unwrap one layer —
    // asking for the inner type silently yields `None` (a dependency miss,
    // never a partial read).
    let store = OutputStore::default();
    store.set("state", Arc::new(Mutex::new(7_i32)));
    assert!(store.get::<Mutex<i32>>("state").is_none());
    let both = store
        .get::<Arc<Mutex<i32>>>("state")
        .expect("double-wrapped");
    let inner: Arc<Mutex<i32>> = (*both).clone();
    assert_eq!(*inner.lock().unwrap(), 7);
}
