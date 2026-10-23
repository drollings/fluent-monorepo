//! Bar-persistent `var` versus intrabar-reset `varip`, as distinct types.
//!
//! The two lifetimes share no representation: a [`Var`] survives
//! [`VarStore::next_bar`] while a [`VarIp`] is dropped by it. Conflating them
//! (the legacy `varip == var` bug) is a type error here, not a silent alias.
//!
//! Every cell also owns its scope: a `var` declaration belongs to the call
//! instance that reached it, not to its spelling. Two functions (or two call
//! expressions of one function) that both declare `var x` hold two cells, so
//! [`ScopeKey`] — not the name — is what the store is keyed by. The name is
//! kept beside the value because the inspection view and write-backs still
//! address cells by spelling.

use std::collections::HashMap;

use crate::value::PineValue;

/// Parser-assigned identity of one scoping node: a `var`/`varip` declaration
/// or a call expression, numbered in source order per parsed program. Nothing
/// is hashed, so identities are stable across runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(u32);

impl NodeId {
    /// The scope no call expression encloses: a module's top level.
    pub const ROOT: Self = Self(0);

    /// The identity of the `raw` node. Numbering starts at one, so
    /// [`NodeId::ROOT`] is never a parsed node.
    #[must_use]
    pub fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// The parser's number.
    #[must_use]
    pub fn get(self) -> u32 {
        self.0
    }
}

/// Owning module identity for a persistent binding: the linked module key
/// (`publisher/name`), the same string [`LinkedModule::key`] carries.
///
/// [`LinkedModule::key`]: crate::pine::eval::LinkedModule
pub type ModuleKey = String;

/// One call expression on the path that reached a body: the module owning the
/// expression and the expression's node.
///
/// Node ids are numbered per parsed program, so the module is part of the
/// identity — two modules' expression number 7 are different call sites.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CallSite {
    /// Module owning the call expression.
    pub module: Option<ModuleKey>,
    /// The call expression's node.
    pub node: NodeId,
}

impl CallSite {
    /// Build one call site.
    #[must_use]
    pub fn new(module: Option<ModuleKey>, node: NodeId) -> Self {
        Self { module, node }
    }
}

/// Identity of one persistent cell: the module whose body declares it, the
/// call expressions that reached that body (outermost first, empty at a
/// module's top level), and the declaration node.
///
/// A call expression is one instance, so a function called from two places
/// owns two cells, and a `var` declared at the top level belongs to the
/// module's root scope.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopeKey {
    /// Module owning the declaring body.
    pub module: Option<ModuleKey>,
    /// Enclosing call expressions, outermost first.
    pub call_sites: Vec<CallSite>,
    /// The declaration node.
    pub decl: NodeId,
}

impl ScopeKey {
    /// Build one cell identity.
    #[must_use]
    pub fn new(module: Option<ModuleKey>, call_sites: Vec<CallSite>, decl: NodeId) -> Self {
        Self {
            module,
            call_sites,
            decl,
        }
    }
}

/// Bar-persistent cell: survives [`VarStore::next_bar`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Var {
    value: Option<PineValue>,
}

impl Var {
    /// Read the cell; `None` is Pine `na`.
    #[must_use]
    pub fn get(&self) -> Option<PineValue> {
        self.value.clone()
    }

    /// Overwrite the cell.
    pub fn set(&mut self, value: PineValue) {
        self.value = Some(value);
    }
}

/// Intrabar cell: cleared by [`VarStore::next_bar`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct VarIp {
    value: Option<PineValue>,
}

impl VarIp {
    /// Read the cell; `None` is Pine `na`.
    #[must_use]
    pub fn get(&self) -> Option<PineValue> {
        self.value.clone()
    }

    /// Overwrite the cell for the rest of the current bar only.
    pub fn set(&mut self, value: PineValue) {
        self.value = Some(value);
    }
}

/// One occupied slot: the spelling its declaration used beside the value.
#[derive(Debug, Clone, PartialEq)]
struct Slot<V> {
    name: String,
    cell: V,
}

/// Per-run persistent cells, keyed by the declaration instance that owns
/// them. One store per [`RunCtx`](crate::context::RunCtx); stores are never
/// shared between runs, which is what makes parallel backtests isolated.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct VarStore {
    vars: HashMap<ScopeKey, Slot<Var>>,
    varips: HashMap<ScopeKey, Slot<VarIp>>,
}

impl VarStore {
    /// Create an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Write a bar-persistent cell.
    pub fn set_var(&mut self, key: ScopeKey, name: &str, value: PineValue) {
        match self.vars.get_mut(&key) {
            Some(slot) => slot.cell.set(value),
            None => {
                self.vars.insert(
                    key,
                    Slot {
                        name: name.to_owned(),
                        cell: Var { value: Some(value) },
                    },
                );
            }
        }
    }

    /// Read a bar-persistent cell; absent is `na`.
    #[must_use]
    pub fn get_var(&self, key: &ScopeKey) -> Option<PineValue> {
        self.vars.get(key).and_then(|slot| slot.cell.get())
    }

    /// Write an intrabar cell.
    pub fn set_varip(&mut self, key: ScopeKey, name: &str, value: PineValue) {
        match self.varips.get_mut(&key) {
            Some(slot) => slot.cell.set(value),
            None => {
                self.varips.insert(
                    key,
                    Slot {
                        name: name.to_owned(),
                        cell: VarIp { value: Some(value) },
                    },
                );
            }
        }
    }

    /// Read an intrabar cell; absent (or post-reset) is `na`.
    #[must_use]
    pub fn get_varip(&self, key: &ScopeKey) -> Option<PineValue> {
        self.varips.get(key).and_then(|slot| slot.cell.get())
    }

    /// Every occupied cell with its identity and value, for recording each
    /// instance's own history series. Order is irrelevant: each entry fills
    /// its own slot.
    #[must_use]
    pub fn occupied(&self) -> Vec<(ScopeKey, Option<PineValue>)> {
        let mut out: Vec<(ScopeKey, Option<PineValue>)> = self
            .vars
            .iter()
            .map(|(key, slot)| (key.clone(), slot.cell.get()))
            .collect();
        out.extend(
            self.varips
                .iter()
                .map(|(key, slot)| (key.clone(), slot.cell.get())),
        );
        out
    }

    /// Advance one bar: intrabar cells are dropped, persistent cells survive.
    pub fn next_bar(&mut self) {
        self.varips.clear();
    }

    /// Borrow all persistent bindings for inspection (engine snapshots).
    /// Intrabar cells overlay bar-persistent ones on name collision.
    ///
    /// Cells are keyed by declaration, so one spelling can occupy several
    /// cells. The inspection view is one value per name, and it reports the
    /// outermost cell — the lowest call path, then the earliest declaration —
    /// so a script-level binding is never masked by a helper's local. Sorting
    /// keeps the view deterministic whichever way the map iterates.
    #[must_use]
    pub fn snapshot(&self) -> HashMap<String, Option<PineValue>> {
        let mut out = HashMap::new();
        let mut vars: Vec<(&ScopeKey, &Slot<Var>)> = self.vars.iter().collect();
        vars.sort_by(|(left, _), (right, _)| right.cmp(left));
        for (_, slot) in vars {
            out.insert(slot.name.clone(), slot.cell.get());
        }
        let mut varips: Vec<(&ScopeKey, &Slot<VarIp>)> = self.varips.iter().collect();
        varips.sort_by(|(left, _), (right, _)| right.cmp(left));
        for (_, slot) in varips {
            out.insert(slot.name.clone(), slot.cell.get());
        }
        out
    }

    /// Drop one bar-persistent cell, e.g. after it is assigned `na`.
    /// The caller keeps any already-recorded initialization mark.
    pub fn clear_var(&mut self, key: &ScopeKey) {
        self.vars.remove(key);
    }

    /// Drop one intrabar cell.
    pub fn clear_varip(&mut self, key: &ScopeKey) {
        self.varips.remove(key);
    }
}

#[cfg(test)]
#[path = "../tests/vars.rs"]
mod tests;
