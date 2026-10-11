//! Pine import graph: multi-file programs as one ordered namespace.
//!
//! [`resolve_imports`] links supplied modules into a [`ModuleGraph`]: exact
//! matches on publisher and name (versions ride along as metadata and never
//! refuse), aliases per importer, and only `export`ed items visible across
//! modules. Ordering composes the canonical [`DependencyGraph`](fluent_dag::dep_graph::DependencyGraph)
//! — the same sorter the indicator registry uses — so there is exactly one
//! topological sort in the kernel surface.
//!
//! Resolution is pure over supplied values: the caller loads sources (file
//! reads, version warnings) and parses programs beforehand, then hands the
//! triple over. Unknown imports and import cycles refuse with a reason;
//! version findings warn and still resolve.

use std::collections::{HashMap, HashSet};
use std::fmt;

use fluent_dag::dep_graph::{DependencyGraph, GraphError};

use super::source::{LibraryId, PineSource, VersionWarn};
use super::syntax::{PineProgram, PineStmt};
use crate::error::KernelError;

/// Identity of one resolvable Pine module: publisher and library name, as in
/// `import showmethegrail/libUtil/12`. The pinned version is deliberately
/// not part of the identity — version mismatch warns, never refuses — so it
/// travels on the claimed [`LibraryId`] instead.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModuleId {
    /// Publisher namespace (`showmethegrail`).
    pub publisher: String,
    /// Library name (`libUtil`).
    pub name: String,
}

impl ModuleId {
    /// Build an identity from parts.
    #[must_use]
    pub fn new(publisher: &str, name: &str) -> Self {
        Self {
            publisher: publisher.to_owned(),
            name: name.to_owned(),
        }
    }
}

impl From<&LibraryId> for ModuleId {
    fn from(id: &LibraryId) -> Self {
        Self::new(&id.publisher, &id.name)
    }
}

impl fmt::Display for ModuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.publisher, self.name)
    }
}

/// One module offered to the resolver: claimed identity plus the loaded
/// source (import lines, version findings) and its parsed program (exports).
#[derive(Debug, Clone)]
pub struct ModuleInput {
    /// Claimed publisher/name/version (version retained, never enforced).
    pub id: LibraryId,
    /// Loaded source; its scanned `imports` drive graph edges.
    pub source: PineSource,
    /// Parsed program; its top-level `export`s drive visibility.
    pub program: PineProgram,
}

impl ModuleInput {
    /// Bundle one already-loaded, already-parsed module for resolution.
    #[must_use]
    pub fn new(id: LibraryId, source: PineSource, program: PineProgram) -> Self {
        Self { id, source, program }
    }

    /// Graph identity: publisher and name only.
    #[must_use]
    pub fn module_id(&self) -> ModuleId {
        ModuleId::from(&self.id)
    }
}

/// One resolved module: everything later stages need, in dependency order.
#[derive(Debug, Clone)]
pub struct ResolvedModule {
    /// Claimed identity (pinned version retained as metadata).
    pub id: LibraryId,
    /// Loaded source (paths, import lines, version findings).
    pub source: PineSource,
    /// Parsed program for lowering.
    pub program: PineProgram,
    /// Top-level `export`ed names, in source order.
    pub exports: Vec<String>,
}

/// A linked multi-file program: modules in dependency-first order with
/// per-importer alias tables and export-filtered visibility.
#[derive(Debug, Clone)]
pub struct ModuleGraph {
    /// Module identities, dependencies before dependents.
    order: Vec<ModuleId>,
    /// Resolved modules by identity.
    entries: HashMap<ModuleId, ResolvedModule>,
    /// Version findings, deduplicated by message so a double-loaded library
    /// warns once.
    warnings: Vec<VersionWarn>,
}

impl ModuleGraph {
    /// Module identities in dependency-first order.
    #[must_use]
    pub fn order(&self) -> &[ModuleId] {
        &self.order
    }

    /// Collected version findings (warn-only; resolution still succeeded).
    #[must_use]
    pub fn warnings(&self) -> &[VersionWarn] {
        &self.warnings
    }

    /// Look up one resolved module by identity.
    #[must_use]
    pub fn get(&self, id: &ModuleId) -> Option<&ResolvedModule> {
        self.entries.get(id)
    }

    /// Names the module exports (visible to importers), in source order.
    #[must_use]
    pub fn exports_of(&self, id: &ModuleId) -> Option<&[String]> {
        self.entries.get(id).map(|entry| entry.exports.as_slice())
    }

    /// One importer's alias table: local alias to exporting module.
    /// A repeated alias keeps its last declaration.
    #[must_use]
    pub fn alias_table(&self, importer: &ModuleId) -> Option<HashMap<String, ModuleId>> {
        let entry = self.entries.get(importer)?;
        let mut table = HashMap::new();
        for import in &entry.source.imports {
            table.insert(import.alias.clone(), ModuleId::from(&import.library));
        }
        Some(table)
    }

    /// Bind `ALIAS.item` from one importer to its exporting module. Refuses
    /// unknown aliases and items the target does not export, each with a
    /// reason — a bare helper is never visible across modules.
    pub fn resolve_reference(
        &self,
        importer: &ModuleId,
        alias: &str,
        item: &str,
    ) -> Result<&ModuleId, KernelError> {
        let entry = self.entries.get(importer).ok_or_else(|| {
            KernelError::InvalidInput(format!("unknown pine module '{importer}'"))
        })?;
        let target = entry
            .source
            .imports
            .iter()
            .rev()
            .find(|import| import.alias == alias)
            .map(|import| ModuleId::from(&import.library))
            .ok_or_else(|| {
                KernelError::InvalidInput(format!(
                    "unknown import alias '{alias}' in pine module '{importer}'"
                ))
            })?;
        let (key, target_entry) = self.entries.get_key_value(&target).ok_or_else(|| {
            KernelError::InvalidInput(format!(
                "unknown pine import '{target}' required by '{importer}'"
            ))
        })?;
        if target_entry.exports.iter().any(|name| name == item) {
            Ok(key)
        } else {
            Err(KernelError::InvalidInput(format!(
                "pine item '{item}' is not exported by '{target}' (imported as '{alias}' in '{importer}')"
            )))
        }
    }
}

/// Link `modules` into dependency-first order. Duplicate loads of one
/// publisher/name keep the first copy; unknown imports and cycles refuse
/// with reasons; version findings collect as warnings and still resolve.
pub fn resolve_imports(modules: Vec<ModuleInput>) -> Result<ModuleGraph, KernelError> {
    let mut entries: HashMap<ModuleId, ResolvedModule> = HashMap::new();
    let mut order_hint: Vec<ModuleId> = Vec::new();
    for input in modules {
        let key = input.module_id();
        if entries.contains_key(&key) {
            continue;
        }
        order_hint.push(key.clone());
        entries.insert(
            key,
            ResolvedModule {
                id: input.id,
                source: input.source,
                exports: exported_names(&input.program),
                program: input.program,
            },
        );
    }

    let mut graph = DependencyGraph::new();
    for key in &order_hint {
        let entry = &entries[key];
        let mut deps: Vec<ModuleId> = Vec::new();
        let mut seen: HashSet<ModuleId> = HashSet::new();
        for import in &entry.source.imports {
            let target = ModuleId::from(&import.library);
            if !entries.contains_key(&target) {
                return Err(KernelError::InvalidInput(format!(
                    "unknown pine import '{}' as {} required by '{}'",
                    import.library, import.alias, key
                )));
            }
            if seen.insert(target.clone()) {
                deps.push(target);
            }
        }
        graph.register(key, &deps, std::slice::from_ref(key))?;
    }
    let order = graph.topo_sort().map_err(|err| match err {
        GraphError::CircularDependency(_) => {
            KernelError::InvalidInput(format!("pine import cycle: {err}"))
        }
        other => KernelError::InvalidInput(format!("pine import graph failed: {other}")),
    })?;

    let mut warnings: Vec<VersionWarn> = Vec::new();
    let mut seen_messages: HashSet<String> = HashSet::new();
    for key in &order {
        for warn in &entries[key].source.warnings {
            if seen_messages.insert(warn.message.clone()) {
                warnings.push(warn.clone());
            }
        }
    }

    Ok(ModuleGraph {
        order,
        entries,
        warnings,
    })
}

/// Top-level `export`ed names in source order: functions, methods, consts,
/// enums, and types. Anything else under `export` (or without it) stays
/// module-private.
fn exported_names(program: &PineProgram) -> Vec<String> {
    let mut names = Vec::new();
    for stmt in &program.statements {
        if let PineStmt::ExportDecl(inner) = stmt {
            let name = match inner.as_ref() {
                PineStmt::FuncDecl { name, .. }
                | PineStmt::MethodDecl { name, .. }
                | PineStmt::ConstDecl { name, .. }
                | PineStmt::EnumDecl { name, .. }
                | PineStmt::TypeDecl { name, .. } => Some(name.clone()),
                _ => None,
            };
            if let Some(name) = name {
                names.push(name);
            }
        }
    }
    names
}

impl From<GraphError> for KernelError {
    fn from(err: GraphError) -> Self {
        KernelError::InvalidInput(format!("pine import graph failed: {err}"))
    }
}

#[cfg(test)]
#[path = "../../tests/pine_modules.rs"]
mod tests;
