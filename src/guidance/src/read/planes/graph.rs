//! Graph plane: callers, callees, dependents, and owners over the
//! hydrated call/import graph. Edges keep their resolution tiers;
//! unknown symbols yield honest empty, never lexical junk. Ownership
//! reuses `component_map::component_gate_for_path` — no second owner
//! table.

use std::collections::{HashMap, HashSet};

use search_vector::db::GuidanceDb;

use crate::graph_index::{CallEdge, GraphIndex, ResolutionMethod};

/// One call site on the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One call site on the graph.
pub struct CallSite {
    /// Calling file (absolute path as stored).
    pub file: String,
    /// Calling symbol, when the call sits in a named scope.
    pub symbol: Option<String>,
    /// How the callee name resolved to definition sites.
    pub resolution: ResolutionMethod,
}

/// One callee of a definition site.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One callee of a definition site.
pub struct Callee {
    /// Defining file of the caller.
    pub file: String,
    /// Raw callee name as recorded.
    pub name: String,
}

/// File ownership through the component gate.
#[derive(Debug, Clone, PartialEq, Eq)]
/// File ownership through the component gate.
pub struct OwnerInfo {
    /// Owning Cargo package (`unknown` fallback, never a guess).
    pub package: String,
    /// Make-matrix component, when the path is in the matrix.
    pub component: Option<String>,
}

/// Caller selection policy: product callers by default; tests and
/// fixtures are diagnostics, never product dependents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Caller selection policy: product callers by default; tests and
/// fixtures are diagnostics, never product dependents.
#[derive(Default)]
/// Caller selection policy: product callers by default; tests and
/// fixtures are diagnostics, never product dependents.
/// Caller selection policy: product callers by default; tests and
/// fixtures are diagnostics, never product dependents.
pub struct CallerFilter {
    /// Include test-scope callers.
    pub include_tests: bool,
    /// Include fixture-scope callers.
    pub include_fixtures: bool,
}

impl CallerFilter {
    fn excludes(self, file: &str) -> bool {
        match crate::source_scope::classify(file, None) {
            crate::source_scope::SourceScope::Test => !self.include_tests,
            crate::source_scope::SourceScope::Fixture => !self.include_fixtures,
            _ => false,
        }
    }
}

/// The graph plane: hydrated call/import edges plus per-symbol call
/// lists, over one committed index.
pub struct GraphPlane {
    index: GraphIndex,
    /// `(symbol name, file)` call lists from the persisted rows.
    calls_by_symbol: HashMap<(String, String), Vec<String>>,
}

impl GraphPlane {
    /// Hydrate from a committed index (no file reads, no parsing).
    pub fn hydrate(db: &GuidanceDb, roots: &[String]) -> Result<Self, String> {
        let index = GraphIndex::hydrate_from_db(db, roots).map_err(|error| error.to_string())?;
        let mut calls_by_symbol: HashMap<(String, String), Vec<String>> = HashMap::new();
        for row in db
            .graph_symbol_rows()
            .map_err(|error| format!("graph symbols: {error}"))?
        {
            calls_by_symbol
                .entry((row.name, row.file))
                .or_default()
                .extend(row.calls);
        }
        Ok(Self {
            index,
            calls_by_symbol,
        })
    }

    /// Query forms for one reference (full dotted plus terminal).
    fn forms(name: &str) -> Vec<String> {
        crate::read::planes::symbol::normalize_symbol_query(name)
    }

    /// Call sites naming the symbol in any query form, sorted by
    /// (file, symbol). Homonyms expand; unknown or empty names yield
    /// nothing. Product callers only; use [`Self::callers_with`] for
    /// test/fixture diagnostics.
    #[must_use]
    pub fn callers(&self, symbol: &str) -> Vec<CallSite> {
        self.callers_with(symbol, CallerFilter::default())
    }

    /// Call sites under an explicit scope filter. An edge must resolve to
    /// the queried symbol's definition site; unresolved name collisions
    /// are diagnostics, never asserted callers.
    #[must_use]
    pub fn callers_with(&self, symbol: &str, filter: CallerFilter) -> Vec<CallSite> {
        if Self::forms(symbol).is_empty() {
            return Vec::new();
        }
        let defs: HashSet<String> = Self::forms(symbol)
            .into_iter()
            .flat_map(|form| {
                self.index
                    .definitions_of(&form)
                    .iter()
                    .map(|def| def.file.clone())
            })
            .collect();
        let terminal = crate::read::planes::symbol::normalize_symbol_query(symbol)
            .into_iter()
            .collect::<HashSet<_>>();
        let mut out: Vec<CallSite> = self
            .index
            .call_edges()
            .iter()
            .filter(|edge| {
                terminal.contains(&edge.callee)
                    && !edge.callee_files.is_empty()
                    && (defs.is_empty() || edge.callee_files.iter().any(|file| defs.contains(file)))
                    && !filter.excludes(&edge.caller_file)
            })
            .map(CallSite::from_edge)
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Callees recorded at the symbol's definition sites.
    #[must_use]
    pub fn callees(&self, symbol: &str) -> Vec<Callee> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for form in Self::forms(symbol) {
            for def in self.index.definitions_of(&form) {
                if let Some(calls) = self.calls_by_symbol.get(&(form.clone(), def.file.clone())) {
                    for callee in calls {
                        if seen.insert((def.file.clone(), callee.clone())) {
                            out.push(Callee {
                                file: def.file.clone(),
                                name: callee.clone(),
                            });
                        }
                    }
                }
            }
        }
        out.sort_by(|a, b| (&a.file, &a.name).cmp(&(&b.file, &b.name)));
        out
    }

    /// Dependent files: importers of defining files (closure), files
    /// calling the symbol, and the defining files themselves.
    /// Sorted, deduped, absolute paths as stored.
    #[must_use]
    pub fn dependents(&self, symbol: &str) -> Vec<String> {
        let mut files: HashSet<String> = HashSet::new();
        for form in Self::forms(symbol) {
            for def in self.index.definitions_of(&form) {
                files.insert(def.file.clone());
            }
        }
        if files.is_empty() {
            // No definitions: callers alone still name dependents.
            for site in self.callers(symbol) {
                files.insert(site.file);
            }
            let mut out: Vec<String> = files.into_iter().collect();
            out.sort();
            return out;
        }
        let known: Vec<String> = files.iter().cloned().collect();
        for dependent in self.index.dependents_closure(&known) {
            files.insert(dependent);
        }
        for site in self.callers(symbol) {
            files.insert(site.file);
        }
        let mut out: Vec<String> = files.into_iter().collect();
        out.sort();
        out
    }

    /// Owner of one path through the component gate.
    #[must_use]
    pub fn owner(&self, path: &str) -> OwnerInfo {
        let gate = crate::component_map::component_gate_for_path(path);
        OwnerInfo {
            package: if gate.package.is_empty() {
                "unknown".to_string()
            } else {
                gate.package
            },
            component: gate.component,
        }
    }

    /// Borrow the hydrated index (callers that need tiers directly).
    #[must_use]
    pub fn index(&self) -> &GraphIndex {
        &self.index
    }
}

impl CallSite {
    fn from_edge(edge: &CallEdge) -> Self {
        Self {
            file: edge.caller_file.clone(),
            symbol: edge.caller_symbol.clone(),
            resolution: edge.resolution,
        }
    }
}

impl Ord for CallSite {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (&self.file, &self.symbol).cmp(&(&other.file, &other.symbol))
    }
}

impl PartialOrd for CallSite {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
