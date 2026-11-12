//! One semantic home model shared by both execution engines.
//!
//! A *home* is where a mutable value came from: a directly addressable
//! slot, the live receiver's field, a nested field path, or a map entry a
//! variable aliases. The engines keep their own slot addresses — the
//! tree-walk names a namespace or a persistent cell, the compiled loop
//! names a column, a caller frame slot, a per-instance cell, or a live
//! receiver slot — while everything above the slot (the nested rebuild
//! order, the map-entry write, the lease decision) is this module's single
//! implementation. Two parallel enums with parallel take/put branches
//! cannot drift because there is only one.
//!
//! Every operation is a decision table over the home shape:
//!
//! | shape | take | put |
//! |---|---|---|
//! | slot | engine | engine |
//! | `this` field | engine receiver | engine receiver |
//! | nested path | walk fields, restore on miss | rebuild outward, put root |
//! | map entry | engine slot | entry, then engine slot |
//!
//! The tree-walk and the loop share the walk/rebuild order, so a nested
//! receiver mutates the stored object the same way in either engine.

use crate::native_libs::LeaseKey;
use crate::value::{PineValue, UserInstance};
use crate::KernelError;

/// Engine-owned address of one directly writable slot.
pub(crate) trait HomeSlot {
    /// Lease key when the slot backs a native-served resident ring.
    /// Receiver fields and nested paths have none: the enclosing take
    /// brackets them and bumps on write-back.
    fn lease_key(&self) -> Option<LeaseKey>;
}

/// One home a value can be taken out of and put back into.
#[derive(Debug, Clone)]
/// One home a value can be taken out of and put back into.
pub(crate) enum Home<S> {
    /// Directly addressable slot in the owning engine's storage.
    Slot(S),
    /// Field of the live receiver on the engine's receiver stack.
    ThisField(String),
    /// Nested field path under a root home. `parents` holds the taken
    /// chain outermost first with each child already removed.
    Nested {
        /// Root home the path was taken from.
        root: Box<Home<S>>,
        /// Outer-to-inner `(parent, field)` chain.
        parents: Vec<(UserInstance, String)>,
    },
    /// A slot aliasing one map entry: the write reaches the entry and the
    /// slot, so method calls through `map.get` handles mutate the stored
    /// object exactly like Pine references.
    MapEntry {
        /// The slot the instance was taken from.
        inner: Box<Home<S>>,
        /// Variable holding the map.
        map_var: String,
        /// Canonical key encoding.
        key: String,
        /// Original key value.
        key_value: PineValue,
    },
}

/// Storage operations an engine provides for [`Home`] writes. Resolution —
/// which name/column/slot a home came from — stays engine-specific; only
/// the store step is shared.
pub(crate) trait HomeStore<S> {
    /// Store one instance in a directly addressable slot.
    fn put_slot(&mut self, slot: &S, instance: UserInstance) -> Result<(), KernelError>;
    /// Store one instance in the live receiver's field.
    fn put_this_field(&mut self, field: &str, instance: UserInstance) -> Result<(), KernelError>;
    /// Persist one instance into a general-map entry.
    fn put_map_entry(
        &mut self,
        map_var: &str,
        key: &str,
        key_value: &PineValue,
        instance: UserInstance,
    ) -> Result<(), KernelError>;
}

impl<S> Home<S> {
    /// The directly addressable slot at the root of this home, when it has
    /// one. Receiver-field homes have none.
    #[must_use]
    pub(crate) fn root_slot(&self) -> Option<&S> {
        match self {
            Self::Slot(slot) => Some(slot),
            Self::ThisField(_) => None,
            Self::Nested { root, .. } | Self::MapEntry { inner: root, .. } => root.root_slot(),
        }
    }

    /// Whether a write through this home must bump a lease version.
    #[must_use]
    pub(crate) fn lease_key(&self) -> Option<LeaseKey>
    where
        S: HomeSlot,
    {
        self.root_slot().and_then(HomeSlot::lease_key)
    }

    /// Put `instance` back through this home. Nested paths rebuild the
    /// parent chain outward and store the rebuilt root; map entries persist
    /// the entry first and then the slot, matching the tree-walk's order.
    pub(crate) fn put<St: HomeStore<S>>(
        &self,
        store: &mut St,
        instance: UserInstance,
    ) -> Result<(), KernelError> {
        match self {
            Self::Slot(slot) => store.put_slot(slot, instance),
            Self::ThisField(field) => store.put_this_field(field, instance),
            Self::Nested { root, parents } => {
                let rebuilt = rebuild_chain(parents, instance);
                root.put(store, rebuilt)
            }
            Self::MapEntry {
                inner,
                map_var,
                key,
                key_value,
            } => {
                store.put_map_entry(map_var, key, key_value, instance.clone())?;
                inner.put(store, instance)
            }
        }
    }
}

/// Rebuild one taken parent chain outward: the innermost instance re-enters
/// its parent's field, that parent re-enters its own, and so on.
fn rebuild_chain(parents: &[(UserInstance, String)], instance: UserInstance) -> UserInstance {
    let mut current = instance;
    for (parent, field) in parents.iter().rev() {
        let mut parent = parent.clone();
        parent
            .fields
            .insert(field.clone(), Some(PineValue::Object(current)));
        current = parent;
    }
    current
}

/// Walk `path` off one already-taken root instance and return the innermost
/// instance plus its nested home. A miss restores everything taken so far
/// through the root and returns `None`, so a declined take never unbinds
/// the home.
pub(crate) fn take_nested<S, St>(
    store: &mut St,
    root: Home<S>,
    root_instance: UserInstance,
    path: &[String],
) -> Result<Option<(UserInstance, Home<S>)>, KernelError>
where
    St: HomeStore<S>,
{
    let mut current = root_instance;
    let mut parents: Vec<(UserInstance, String)> = Vec::with_capacity(path.len());
    for field in path {
        match current.fields.get_mut(field).map(Option::take) {
            Some(Some(PineValue::Object(next))) => {
                let parent = std::mem::replace(&mut current, next);
                parents.push((parent, field.clone()));
            }
            Some(other) => {
                current.fields.insert(field.clone(), other);
                restore_chain(store, &root, &parents, current)?;
                return Ok(None);
            }
            None => {
                restore_chain(store, &root, &parents, current)?;
                return Ok(None);
            }
        }
    }
    Ok(Some((
        current,
        Home::Nested {
            root: Box::new(root),
            parents,
        },
    )))
}

/// Restore a partially taken chain after a declined take: the innermost
/// taken instance returns first, rebuilding outward, and the root returns
/// to its own home.
fn restore_chain<S, St>(
    store: &mut St,
    root: &Home<S>,
    parents: &[(UserInstance, String)],
    current: UserInstance,
) -> Result<(), KernelError>
where
    St: HomeStore<S>,
{
    root.put(store, rebuild_chain(parents, current))
}

#[cfg(test)]
#[path = "../../tests/pine_home.rs"]
mod home_tests;