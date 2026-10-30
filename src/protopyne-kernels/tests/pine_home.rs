//! Home-model behavior: the shared nested/map-entry/lease decision table
//! that both engines call. The stub store records the exact put order, so
//! a drifted branch fails here before it can diverge at run time.
//!
//! Test path note: this file rides the `pine::home` unit module, so the
//! runnable filter is `pine::home`, not the file stem.

use super::*;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
struct StubSlot(&'static str);

impl HomeSlot for StubSlot {
    fn lease_key(&self) -> Option<LeaseKey> {
        match self.0 {
            "plain" => Some(LeaseKey::variables("w")),
            _ => None,
        }
    }
}

#[derive(Default)]
struct StubStore {
    puts: Vec<(String, UserInstance)>,
    maps: Vec<(String, String, i64)>,
    receiver: Option<UserInstance>,
}

impl HomeStore<StubSlot> for StubStore {
    fn put_slot(&mut self, slot: &StubSlot, instance: UserInstance) -> Result<(), KernelError> {
        self.puts.push((slot.0.to_owned(), instance));
        Ok(())
    }

    fn put_this_field(&mut self, field: &str, instance: UserInstance) -> Result<(), KernelError> {
        let mut receiver = self.receiver.take().unwrap_or_else(|| {
            UserInstance::new("Receiver", HashMap::<String, Option<PineValue>>::new())
        });
        receiver
            .fields
            .insert(field.to_owned(), Some(PineValue::Object(instance)));
        self.receiver = Some(receiver);
        Ok(())
    }

    fn put_map_entry(
        &mut self,
        map_var: &str,
        key_text: &str,
        key_value: &PineValue,
        _instance: UserInstance,
    ) -> Result<(), KernelError> {
        let key = key_value.as_int().expect("stub keys are ints");
        self.maps
            .push((map_var.to_owned(), key_text.to_owned(), key));
        Ok(())
    }
}

fn instance(type_name: &str, fields: &[(&str, f64)]) -> UserInstance {
    let mut bound: HashMap<String, Option<PineValue>> = HashMap::new();
    for (name, value) in fields {
        bound.insert((*name).to_owned(), Some(PineValue::Float(*value)));
    }
    UserInstance::new(type_name, bound)
}

#[test]
fn nested_put_rebuilds_the_parent_chain() {
    // The innermost instance re-enters its parent's field, then the root
    // slot receives the rebuilt outer instance.
    let root = instance("Outer", &[("n", 1.0)]);
    let inner = instance("Inner", &[("n", 10.0)]);
    let mut root = root;
    root.fields
        .insert("box".to_owned(), Some(PineValue::Object(inner)));
    let home: Home<StubSlot> = Home::Nested {
        root: Box::new(Home::Slot(StubSlot("plain"))),
        parents: vec![(instance("Outer", &[("n", 1.0)]), "box".to_owned())],
    };
    let mut store = StubStore::default();
    let mutated = instance("Inner", &[("n", 99.0)]);
    home.put(&mut store, mutated).expect("nested put succeeds");
    assert_eq!(store.puts.len(), 1, "one root store");
    let (slot, rebuilt) = &store.puts[0];
    assert_eq!(slot, "plain");
    let Some(PineValue::Object(boxed)) = rebuilt.fields.get("box").cloned().flatten() else {
        panic!("rebuilt root carries the nested object: {rebuilt:?}");
    };
    assert_eq!(
        boxed.fields.get("n").cloned().flatten(),
        Some(PineValue::Float(99.0)),
        "the mutated inner value reaches the stored chain"
    );
}

#[test]
fn take_miss_restores_the_root_before_declining() {
    // A path that holds no object restores the root it already took, so a
    // declined take never unbinds the home.
    let root = instance("Outer", &[("n", 1.0)]);
    let mut store = StubStore::default();
    let home: Home<StubSlot> = Home::Slot(StubSlot("plain"));
    let taken = take_nested(&mut store, home, root.clone(), &["missing".to_owned()])
        .expect("declined take is not an error");
    assert!(taken.is_none(), "the miss declines");
    assert_eq!(store.puts.len(), 1, "the root is restored exactly once");
    assert_eq!(
        store.puts[0].1.type_name, "Outer",
        "the restored value is the root"
    );

    // A path that takes two levels then misses restores the whole chain.
    let mut middle = instance("Middle", &[("n", 2.0)]);
    let leaf = instance("Leaf", &[("n", 3.0)]);
    middle
        .fields
        .insert("leaf".to_owned(), Some(PineValue::Object(leaf)));
    let mut outer = instance("Outer", &[("n", 1.0)]);
    outer
        .fields
        .insert("mid".to_owned(), Some(PineValue::Object(middle)));
    let mut store = StubStore::default();
    let taken = take_nested(
        &mut store,
        Home::Slot(StubSlot("plain")),
        outer,
        &["mid".to_owned(), "leaf".to_owned(), "nope".to_owned()],
    )
    .expect("declined chain take is not an error");
    assert!(taken.is_none());
    assert_eq!(store.puts.len(), 1, "one root restore after the chain");
    let restored = &store.puts[0].1;
    let Some(PineValue::Object(mid)) = restored.fields.get("mid").cloned().flatten() else {
        panic!("the chain rebuilds: {restored:?}");
    };
    let Some(PineValue::Object(leaf)) = mid.fields.get("leaf").cloned().flatten() else {
        panic!("the chain rebuilds inward: {mid:?}");
    };
    assert_eq!(
        leaf.fields.get("n").cloned().flatten(),
        Some(PineValue::Float(3.0)),
        "the taken leaf is restored in place"
    );
}

#[test]
fn map_entry_put_reaches_the_entry_and_the_slot() {
    // A map-entry home persists the entry first and then the slot, so a
    // later `map.get` observes the mutated object and the variable does too.
    let home: Home<StubSlot> = Home::MapEntry {
        inner: Box::new(Home::Slot(StubSlot("plain"))),
        map_var: "m".to_owned(),
        key: "int:1".to_owned(),
        key_value: PineValue::Int(1),
    };
    let mut store = StubStore::default();
    home.put(&mut store, instance("Box2", &[("n", 7.0)]))
        .expect("map-entry put succeeds");
    assert_eq!(store.maps, vec![("m".to_owned(), "int:1".to_owned(), 1)]);
    assert_eq!(store.puts.len(), 1, "the slot is written too");
    assert_eq!(store.puts[0].0, "plain");
}

#[test]
fn this_field_put_reaches_the_live_receiver() {
    let home: Home<StubSlot> = Home::ThisField("box".to_owned());
    let mut store = StubStore::default();
    home.put(&mut store, instance("Inner", &[("n", 5.0)]))
        .expect("this-field put succeeds");
    assert!(store.puts.is_empty(), "no slot write for a receiver field");
    let receiver = store.receiver.expect("receiver written");
    assert!(matches!(
        receiver.fields.get("box").cloned().flatten(),
        Some(PineValue::Object(_))
    ));
    assert!(
        home.root_slot().is_none(),
        "receiver fields have no lease root"
    );
}

#[test]
fn lease_keys_follow_the_root_slot() {
    // Only a plain slot leases; nested paths and map entries inherit their
    // root's key, and receiver fields never lease.
    let plain: Home<StubSlot> = Home::Slot(StubSlot("plain"));
    assert_eq!(plain.lease_key(), Some(LeaseKey::variables("w")));
    let nested: Home<StubSlot> = Home::Nested {
        root: Box::new(plain),
        parents: Vec::new(),
    };
    assert_eq!(nested.lease_key(), Some(LeaseKey::variables("w")));
    let map_entry: Home<StubSlot> = Home::MapEntry {
        inner: Box::new(Home::Slot(StubSlot("other"))),
        map_var: "m".to_owned(),
        key: "int:1".to_owned(),
        key_value: PineValue::Int(1),
    };
    assert_eq!(map_entry.lease_key(), None);
    let this_field: Home<StubSlot> = Home::ThisField("f".to_owned());
    assert_eq!(this_field.lease_key(), None, "receiver fields never lease");
}
