//! Typed in-process handoff accumulator.
//!
//! `OutputStore` is the primary channel for passing data between `WorkUnit`s
//! and pipeline stages that live in the same process. Unlike
//! `WorkOutput.data` (a `serde_json::Value` that forces a serialize /
//! deserialize round-trip at every boundary) and `WorkContext.metadata`
//! (untyped stringly-typed annotations), the store holds values as
//! `Arc<dyn Any + Send + Sync>` and exposes typed accessors: values are
//! written with `set::<T>(key, value)` and read with `get::<T>(key) ->
//! Option<Arc<T>>`. The compiler checks the type at the call site; a
//! `get::<T>` for the wrong `T` returns `None` instead of silently
//! mis-deserializing.
//!
//! Because the map itself is `Arc`-shared, clones see the same entries:
//! a unit's write through its context clone is visible to the
//! registrant's clone. `OutputStore` (and therefore `WorkContext`)
//! remains `Clone` without requiring every value to be
//! `Clone` — clones share the same typed allocation, which is exactly the
//! zero-copy handoff semantics an orchestrator wants when it fans a context
//! out to several stages.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use common_core::sync::{lock_read, lock_write};

/// A typed accumulator for in-process handoff between work units and stages.
///
/// This is the **primary** inter-unit data channel (see the decision-rule doc
/// on `WorkContext`): write with [`OutputStore::set`], read with
/// [`OutputStore::get`]. Use `WorkOutput.data` only for payloads that
/// genuinely cross a serialization boundary (WASM units, network dispatch),
/// and `WorkContext.metadata` only for genuinely untyped annotations.
#[derive(Clone, Default)]
pub struct OutputStore {
    /// Shared map: clones see the same entries, so a unit's write through
    /// its context clone is visible to the registrant's clone. This is the
    /// typed unit→caller handoff (no serialization, no side mutex).
    inner: Arc<std::sync::RwLock<HashMap<String, Arc<dyn Any + Send + Sync>>>>,
}

impl std::fmt::Debug for OutputStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputStore")
            .field("keys", &lock_read(&self.inner).keys().collect::<Vec<_>>())
            .finish()
    }
}

impl OutputStore {
    /// Store a typed value under `key`, replacing any prior value.
    ///
    /// The value's concrete type is captured by `Arc<dyn Any>` at the call
    /// site; a later [`OutputStore::get::<T>`](Self::get) must use the same
    /// `T` to retrieve it. Takes `&self`: the map is shared, so this works
    /// from `WorkUnit::execute`'s shared context reference.
    pub fn set<T: Send + Sync + 'static>(&self, key: impl Into<String>, value: T) {
        lock_write(&self.inner).insert(key.into(), Arc::new(value));
    }

    /// Read the typed value stored under `key`, if one exists with exactly
    /// type `T`. A type mismatch (or a missing key) yields `None` — never a
    /// partial or mis-typed read. Returns a shared `Arc`: no clone, no copy.
    pub fn get<T: Send + Sync + 'static>(&self, key: &str) -> Option<Arc<T>> {
        lock_read(&self.inner)
            .get(key)
            .map(Arc::clone)
            .and_then(|v| Arc::downcast::<T>(v).ok())
    }

    /// Whether a value exists under `key` (regardless of its type).
    pub fn contains_key(&self, key: &str) -> bool {
        lock_read(&self.inner).contains_key(key)
    }

    /// Remove the value under `key`. Returns `true` if a value was present.
    pub fn remove(&self, key: &str) -> bool {
        lock_write(&self.inner).remove(key).is_some()
    }

    /// Number of stored handoff values.
    pub fn len(&self) -> usize {
        lock_read(&self.inner).len()
    }

    /// Whether the store holds no values.
    pub fn is_empty(&self) -> bool {
        lock_read(&self.inner).is_empty()
    }

    /// Remove all values.
    pub fn clear(&self) {
        lock_write(&self.inner).clear();
    }
}
