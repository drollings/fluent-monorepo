//! Bar-persistent `var` versus intrabar-reset `varip`, as distinct types.
//!
//! The two lifetimes share no representation: a [`Var`] survives
//! [`VarStore::next_bar`] while a [`VarIp`] is dropped by it. Conflating them
//! (the legacy `varip == var` bug) is a type error here, not a silent alias.

use std::collections::HashMap;

use crate::value::PineValue;

/// Bar-persistent cell: survives [`VarStore::next_bar`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Var {
    value: Option<PineValue>,
}

impl Var {
    /// Read the cell; `None` is Pine `na`.
    #[must_use]
    pub fn get(&self) -> Option<PineValue> {
        self.value
    }

    /// Overwrite the cell.
    pub fn set(&mut self, value: PineValue) {
        self.value = Some(value);
    }
}

/// Intrabar cell: cleared by [`VarStore::next_bar`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct VarIp {
    value: Option<PineValue>,
}

impl VarIp {
    /// Read the cell; `None` is Pine `na`.
    #[must_use]
    pub fn get(&self) -> Option<PineValue> {
        self.value
    }

    /// Overwrite the cell for the rest of the current bar only.
    pub fn set(&mut self, value: PineValue) {
        self.value = Some(value);
    }
}

/// Per-run variable store. One store per [`RunCtx`](crate::context::RunCtx);
/// stores are never shared between runs, which is what makes parallel
/// backtests isolated.
#[derive(Debug, Clone, Default)]
pub struct VarStore {
    vars: HashMap<String, Var>,
    varips: HashMap<String, VarIp>,
}

impl VarStore {
    /// Create an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Write a bar-persistent variable.
    pub fn set_var(&mut self, name: impl Into<String>, value: PineValue) {
        self.vars
            .entry(name.into())
            .and_modify(|v| v.set(value))
            .or_insert(Var { value: Some(value) });
    }

    /// Read a bar-persistent variable; absent is `na`.
    #[must_use]
    pub fn get_var(&self, name: &str) -> Option<PineValue> {
        self.vars.get(name).and_then(Var::get)
    }

    /// Write an intrabar variable.
    pub fn set_varip(&mut self, name: impl Into<String>, value: PineValue) {
        self.varips
            .entry(name.into())
            .and_modify(|v| v.set(value))
            .or_insert(VarIp { value: Some(value) });
    }

    /// Read an intrabar variable; absent (or post-reset) is `na`.
    #[must_use]
    pub fn get_varip(&self, name: &str) -> Option<PineValue> {
        self.varips.get(name).and_then(VarIp::get)
    }

    /// Advance one bar: intrabar cells are dropped, persistent cells survive.
    pub fn next_bar(&mut self) {
        self.varips.clear();
    }
}

#[cfg(test)]
#[path = "../tests/vars.rs"]
mod tests;
