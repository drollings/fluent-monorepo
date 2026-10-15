//! The durable symbol identity is a newtype, and that is the whole point.
//!
//! `types::symbol::SymKey` is a *run-scoped* interner key: it means "row 7
//! of the table this run happened to build", and comparing two of them from
//! different runs is meaningless. A store's `SymbolId` means "the symbol this
//! catalog row has always been", across runs and across processes. They are
//! both `u32`-shaped, which is exactly why they must not be the same type: a
//! function that silently accepts either one launders a run-scoped handle
//! into a durable record.
//!
//! Compile-fail is not testable from inside a crate, so this suite pins what
//! *is* testable — the two types are distinguishable by name, the durable id
//! behaves as an opaque comparable key, and it round-trips through the
//! serialization boundary it will cross. The missing `From` impls are
//! enforced by `cargo build`, not by a test.

use std::any::type_name;

use protopyne_types::SymKey;

use super::*;

/// A key needs exactly comparison, hashing, and copying — nothing else. The
/// absence of `Add`/`Sub`/`From<u32>` is what this bound states: the helper
/// below is the only thing a caller can ask of a `SymbolId` through the
/// store's own API.
fn assert_opaque_comparable_key<T: Copy + Eq + std::hash::Hash + std::fmt::Debug>(a: T, b: T) {
    assert_eq!(a, b);
}

#[test]
fn the_durable_id_and_the_run_scoped_key_are_different_types() {
    assert_eq!(type_name::<SymbolId>(), "protopyne_store::key::SymbolId");
    assert_eq!(type_name::<SymKey>(), "protopyne_types::symbol::SymKey");
    assert_ne!(
        type_name::<SymbolId>(),
        type_name::<SymKey>(),
        "a durable identity and a run-scoped interner handle must never be \
         the same type: a function taking either would launder one into the \
         other without a compiler complaint"
    );
}

#[test]
fn a_durable_id_behaves_as_an_opaque_comparable_key() {
    assert_opaque_comparable_key(SymbolId(7), SymbolId(7));
    assert_ne!(SymbolId(7), SymbolId(8));
    let mut set = std::collections::HashSet::new();
    assert!(set.insert(SymbolId(1)));
    assert!(!set.insert(SymbolId(1)), "equal ids hash equal");
    assert!(set.insert(SymbolId(2)));
}

#[test]
fn the_durable_id_round_trips_through_the_serialization_boundary() {
    let id = SymbolId(4_294_967_295);
    let text = serde_json::to_string(&id).expect("a durable id serializes");
    assert_eq!(text, "4294967295");
    assert_eq!(
        serde_json::from_str::<SymbolId>(&text).expect("a durable id deserializes"),
        id
    );
}

#[test]
fn the_durable_id_orders_stably_for_deterministic_output() {
    let mut ids = vec![SymbolId(3), SymbolId(1), SymbolId(2)];
    ids.sort();
    assert_eq!(ids, vec![SymbolId(1), SymbolId(2), SymbolId(3)]);
}
