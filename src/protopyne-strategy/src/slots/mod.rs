//! Runtime-assignable strategy slots.
//!
//! A slot is a named factory over a small parameter bag: a trigger, a filter,
//! a sizer, or a whole composition. The registries here are the one place the
//! ids live, so a schema enum, a form picker, and the binder cannot disagree
//! about what exists.
//!
//! **Hot-path budget.** A slot is resolved once when a run binds, never per
//! bar: [`SlotRegistry::build`] runs at configuration time, and the bound
//! trait object is then called directly. Registry lookup is a `BTreeMap`
//! lookup by `&'static str`; no reflection, allocation, or serialization
//! happens on the decision path.

pub mod composition;
pub mod envelope;
pub mod exit;
pub mod filter;
pub mod sizing;
pub mod trigger;

use std::collections::BTreeMap;
use std::sync::Arc;

use thiserror::Error;

/// Why a slot id or its parameters were refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SlotError {
    /// No slot is registered under the requested id.
    #[error("unknown slot id: {0}")]
    UnknownId(String),
    /// The slot exists but its parameters do not satisfy its own contract.
    #[error("invalid slot {id}: {reason}")]
    Invalid {
        /// Registered slot id.
        id: &'static str,
        /// Why the parameters were refused.
        reason: String,
    },
}

impl SlotError {
    /// Build an invalid-parameter refusal for `id`.
    #[must_use]
    pub fn invalid(id: &'static str, reason: impl Into<String>) -> Self {
        Self::Invalid {
            id,
            reason: reason.into(),
        }
    }
}

/// One named builder for a slot.
pub trait SlotFactory<T>: Send + Sync {
    /// Stable id a schema enum, a form, and the binder all spell.
    fn id(&self) -> &'static str;
    /// Build the slot from its parameter bag.
    fn build(&self, params: &serde_json::Value) -> Result<T, SlotError>;
}

/// A factory backed by a closure, for registries whose rows differ only in
/// their parameter parsing.
pub struct FnFactory<T> {
    id: &'static str,
    #[allow(clippy::type_complexity)]
    build: Box<dyn Fn(&serde_json::Value) -> Result<T, SlotError> + Send + Sync>,
}

impl<T> FnFactory<T> {
    /// Build a factory with a stable id and a parameter parser.
    #[must_use]
    pub fn new(
        id: &'static str,
        build: impl Fn(&serde_json::Value) -> Result<T, SlotError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            id,
            build: Box::new(build),
        }
    }
}

impl<T> SlotFactory<T> for FnFactory<T> {
    fn id(&self) -> &'static str {
        self.id
    }

    fn build(&self, params: &serde_json::Value) -> Result<T, SlotError> {
        (self.build)(params)
    }
}

/// A deterministic, insertion-ordered registry of slot factories.
pub struct SlotRegistry<T> {
    rows: Vec<Arc<dyn SlotFactory<T>>>,
    index: BTreeMap<&'static str, usize>,
}

impl<T> Default for SlotRegistry<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> SlotRegistry<T> {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            index: BTreeMap::new(),
        }
    }

    /// Register one factory. A duplicate id refuses rather than shadowing:
    /// two implementations of one id is the drift this registry exists to
    /// prevent.
    pub fn register(&mut self, factory: Arc<dyn SlotFactory<T>>) -> Result<(), SlotError> {
        let id = factory.id();
        if self.index.contains_key(id) {
            return Err(SlotError::invalid(id, "duplicate slot id"));
        }
        self.index.insert(id, self.rows.len());
        self.rows.push(factory);
        Ok(())
    }

    /// Every registered id, in registration order (the schema enum order).
    #[must_use]
    pub fn ids(&self) -> Vec<&'static str> {
        self.rows.iter().map(|row| row.id()).collect()
    }

    /// Build one slot by id.
    pub fn build(&self, id: &str, params: &serde_json::Value) -> Result<T, SlotError> {
        let index = self
            .index
            .get(id)
            .ok_or_else(|| SlotError::UnknownId(id.to_owned()))?;
        self.rows[*index].build(params)
    }

    /// Whether `id` is registered.
    #[must_use]
    pub fn contains(&self, id: &str) -> bool {
        self.index.contains_key(id)
    }
}

#[cfg(test)]
#[path = "../../tests/slots.rs"]
mod tests;

#[cfg(test)]
#[path = "../../tests/slot_identity_pins.rs"]
mod slot_identity_pins_tests;
