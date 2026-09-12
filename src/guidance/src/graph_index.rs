//! P3 dependency graph (L4): file-layer `DependencyGraph` over import
//! edges (harvested by the P2 adapters — never a second parser) plus
//! symbol-level call edges, with deterministic graph re-rank and
//! role-coverage tiebreak over fused candidates.

use crate::ast_parser::AstParser;
use crate::extractor::adapter::format_for_extension;
use crate::extractor::code::harvest_file_entities;
use crate::extractor::ExtractSource;
use crate::query::fusion::RecallCandidate;
use crate::search_types::{EntityMetadata, FileKind};
use fluent_dag::dep_graph::{DependencyGraph, GraphError};
use search_vector::tokens::{lower_dedup_push, split_tokens};
use spacy_rs::routing::RoutingSignal;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Graph build failure.
#[derive(Debug, Error)]
pub enum GraphIndexError {
    /// Dependency graph bookkeeping failed.
    #[error("graph error: {0}")]
    Graph(String),
    /// Harvest failed (parse errors are per-file data, never fatal here).
    #[error("harvest error: {0}")]
    Harvest(String),
}

impl From<GraphError> for GraphIndexError {
    fn from(error: GraphError) -> Self {
        Self::Graph(error.to_string())
    }
}

/// One file's harvest inputs (bytes owned by the caller).
#[derive(Debug, Clone)]
pub struct HarvestInput {
    /// Normalized absolute path.
    pub path: String,
    /// Extractor format (`rust`, `typescript`, …).
    pub format: String,
    /// Full file text.
    pub text: String,
}

/// A resolved symbol definition site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolDef {
    /// Defining file (normalized absolute path).
    pub file: String,
    /// Declared symbol name.
    pub symbol: String,
}

/// A caller → callee-name edge (callee resolved at build).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallEdge {    /// Calling file.
    pub caller_file: String,
    /// Calling symbol, if named.
    pub caller_symbol: Option<String>,
    /// Raw callee name.
    pub callee: String,
    /// Files defining the callee (empty when unresolved).
    pub callee_files: Vec<String>,
}

/// An importer → specifier edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportEdge {
    /// Importing file.
    pub importer: String,
    /// Raw specifier text.
    pub specifier: String,
    /// Resolved target file, if any.
    pub resolved_file: Option<String>,
}

/// Anchor-file cap for query-time closure expansion: the top-N fused
/// files seed the depth-1 expansion (mirrors the L4 re-rank's top-3
/// anchors — one anchor convention, two consumers). Bounds the flood
/// axis before the expansion runs.
pub const CLOSURE_ANCHOR_FILES: usize = 3;

/// Direction of a context edge toward its anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ContextDirection {
    /// The file depends on the anchor (importer/caller of the anchor).
    Dependent,
    /// The anchor depends on the file (import/call target of the anchor).
    Dependency,
}

/// Family of the edge carrying a context relation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ContextFamily {
    /// Import/specifier edge.
    Import,
    /// Call edge.
    Call,
}

/// One depth-1 context edge: an anchor's graph neighbor with provenance
/// (which anchor, which direction, which edge). Ordered for
/// deterministic rendering.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ContextEdge {
    /// Neighbor file.
    pub file: String,
    /// Anchor file.
    pub anchor: String,
    /// Direction toward the anchor.
    pub direction: ContextDirection,
    /// Edge family.
    pub family: ContextFamily,
    /// Raw edge detail (specifier text or callee name).
    pub via: String,
}

/// Query-side graph signals: routing-signal lemmas (predicate + roles),
/// lowercased. Built from `extract_routing_signals` — no new NLP.
#[derive(Debug, Clone, Default)]
pub struct GraphQuerySignals {
    /// Predicate lemmas.
    pub predicate: Vec<String>,
    /// Role lemmas (subject, objects, modifiers, qualifiers).
    pub roles: Vec<String>,
}

/// Build query signals from routing signals (predicate + role lemmas).
#[must_use]
pub fn signals_from_routing(signals: &[RoutingSignal]) -> GraphQuerySignals {
    let mut predicate = Vec::new();
    let mut roles = Vec::new();
    for signal in signals {
        lower_dedup_push(&mut predicate, &signal.predicate);
        if let Some(subject) = &signal.subject {
            lower_dedup_push(&mut roles, subject);
        }
        if let Some(object) = &signal.direct_object {
            lower_dedup_push(&mut roles, object);
        }
        if let Some(object) = &signal.indirect_object {
            lower_dedup_push(&mut roles, object);
        }
        for modifier in &signal.modifiers {
            lower_dedup_push(&mut roles, modifier);
        }
        for qualifier in &signal.qualifiers {
            lower_dedup_push(&mut roles, qualifier);
        }
    }
    GraphQuerySignals { predicate, roles }
}

/// L4 boost weights (additive over fused RRF scores; deterministic).
/// Canonical home is `search_vector::fusion` (M6) — re-exported here with
/// identical values so the L4 contract surface stays stable.
pub use search_vector::fusion::{
    CALL_BOOST as L4_CALL_BOOST, DEPENDENT_BOOST as L4_DEPENDENT_BOOST,
    ROLE_EPSILON as L4_ROLE_EPSILON, SAME_FILE_BOOST as L4_SAME_FILE_BOOST,
    SCOPE_BOOST as L4_SCOPE_BOOST,
};

/// Dependency graph over indexed files + symbol call edges (L4 re-rank).
pub struct GraphIndex {
    files: DependencyGraph<String>,
    symbol_defs: HashMap<String, Vec<SymbolDef>>,
    call_edges: Vec<CallEdge>,
    import_edges: Vec<ImportEdge>,
}

/// Harvest one file's persistable graph inputs with a caller-provided
/// parser (one parser per sync, never per file). This is the same
/// harvest `assemble` consumes — the ingest path reuses it rather than
/// a second parser. Non-code files and unknown extensions yield empty
/// inputs (persisting empty clears rows a kind-changed file left).
/// Unnamed scopes persist under "" (calls only, never a definition).
/// Parse failure yields empty inputs: fragments stay authoritative for
/// content, the graph simply has no edges for the file.
pub fn harvest_graph_inputs(
    parser: &mut AstParser,
    format_ext: &str,
    kind: FileKind,
    text: &str,
) -> (Vec<String>, Vec<(String, Vec<String>)>) {
    if kind != FileKind::Code {
        return (Vec::new(), Vec::new());
    }
    let Some(format) = format_for_extension(format_ext) else {
        return (Vec::new(), Vec::new());
    };
    let Ok(harvested) = harvest_file_entities(
        parser,
        &ExtractSource {
            file_id: String::new(),
            text: text.to_string(),
            format: format.to_string(),
            kind: FileKind::Code,
        },
    ) else {
        return (Vec::new(), Vec::new());
    };
    let symbols = harvested
        .symbols
        .into_iter()
        .map(|symbol| (symbol.name.unwrap_or_default(), symbol.calls))
        .collect();
    (harvested.imports, symbols)
}

impl GraphIndex {
    /// Build from harvested file inputs (pure; the DB-backed constructor
    /// feeds the same path).
    pub fn build(files: &[HarvestInput], roots: &[String]) -> Result<Self, GraphIndexError> {
        let mut parser = AstParser::new();
        let mut per_file: Vec<(String, crate::extractor::code::HarvestedFile)> = Vec::new();
        for file in files {
            let harvested = harvest_file_entities(
                &mut parser,
                &ExtractSource {
                    file_id: file.path.clone(),
                    text: file.text.clone(),
                    format: file.format.clone(),
                    kind: FileKind::Code,
                },
            )
            .map_err(|e| GraphIndexError::Harvest(e.to_string()))?;
            per_file.push((file.path.clone(), harvested));
        }
        Self::assemble(&per_file, roots)
    }

    /// Build from persisted rows (no file reads, no parsing): every known
    /// file contributes an entry — even with no edges or symbols, so
    /// specifier resolution probes the full workspace — then the shared
    /// `assemble` registration loop runs over the reconstituted inputs.
    /// Rows for unknown files are ignored (stale by definition).
    /// Scopes are not persisted (no consumer reads them); calls ride
    /// along so call edges reconstitute exactly.
    pub fn build_from_rows(
        known_files: &[String],
        edges: &[(String, String)],
        symbols: &[(String, String, Vec<String>)],
        roots: &[String],
    ) -> Result<Self, GraphIndexError> {
        use crate::extractor::code::{HarvestedFile, HarvestedSymbol};
        let known: HashSet<&String> = known_files.iter().collect();
        let mut per_file: HashMap<String, HarvestedFile> = HashMap::new();
        for path in known_files {
            per_file.entry(path.clone()).or_default();
        }
        for (importer, specifier) in edges {
            if known.contains(importer) {
                per_file
                    .entry(importer.clone())
                    .or_default()
                    .imports
                    .push(specifier.clone());
            }
        }
        for (name, file, calls) in symbols {
            if known.contains(file) {
                per_file.entry(file.clone()).or_default().symbols.push(HarvestedSymbol {
                    name: if name.is_empty() { None } else { Some(name.clone()) },
                    scope: Vec::new(),
                    calls: calls.clone(),
                });
            }
        }
        let ordered: Vec<(String, HarvestedFile)> = per_file.into_iter().collect();
        let ordered = common_core::sort::sorted_by_vec(ordered, |a, b| a.0.cmp(&b.0));
        Self::assemble(&ordered, roots)
    }

    fn assemble(
        per_file: &[(String, crate::extractor::code::HarvestedFile)],
        roots: &[String],
    ) -> Result<Self, GraphIndexError> {
        let mut symbol_defs: HashMap<String, Vec<SymbolDef>> = HashMap::new();
        for (path, harvested) in per_file {
            for symbol in &harvested.symbols {
                if let Some(name) = &symbol.name {
                    symbol_defs.entry(name.clone()).or_default().push(SymbolDef {
                        file: path.clone(),
                        symbol: name.clone(),
                    });
                }
            }
        }
        let mut import_edges = Vec::new();
        let mut file_deps: HashMap<String, HashSet<String>> = HashMap::new();
        for (path, harvested) in per_file {
            file_deps.entry(path.clone()).or_default();
            for specifier in &harvested.imports {
                let resolved = resolve_import(path, specifier, roots, &per_file_paths(per_file));
                if let Some(target) = &resolved {
                    file_deps
                        .entry(path.clone())
                        .or_default()
                        .insert(target.clone());
                }
                import_edges.push(ImportEdge {
                    importer: path.clone(),
                    specifier: specifier.clone(),
                    resolved_file: resolved,
                });
            }
        }
        let mut call_edges = Vec::new();
        for (path, harvested) in per_file {
            for symbol in &harvested.symbols {
                for callee in &symbol.calls {
                    let callee_files: Vec<String> = symbol_defs
                        .get(callee)
                        .map(|defs| {
                            defs.iter()
                                .map(|def| def.file.clone())
                                .filter(|file| file != path)
                                .collect()
                        })
                        .unwrap_or_default();
                    for target in &callee_files {
                        file_deps
                            .entry(path.clone())
                            .or_default()
                            .insert(target.clone());
                    }
                    call_edges.push(CallEdge {
                        caller_file: path.clone(),
                        caller_symbol: symbol.name.clone(),
                        callee: callee.clone(),
                        callee_files,
                    });
                }
            }
        }
        let mut files = DependencyGraph::new();
        let ordered: Vec<&String> =
            common_core::sort::sorted_vec(file_deps.keys().collect());
        for path in ordered {
            let deps: Vec<String> =
                common_core::sort::sorted_vec(file_deps[path].iter().cloned().collect());
            files.register(path, &deps, std::slice::from_ref(path))?;
        }
        Ok(Self {
            files,
            symbol_defs,
            call_edges,
            import_edges,
        })
    }

    /// Transitive dependents closure including the seeds (sorted). Unit of
    /// staleness: touching a file invalidates its dependents, not the
    /// workspace.
    #[must_use]
    pub fn dependents_closure(&self, paths: &[String]) -> Vec<String> {
        let mut closure: HashSet<String> = paths.iter().cloned().collect();
        for path in paths {
            closure.extend(self.files.dependents_of(path));
        }
        common_core::sort::sorted_vec(closure.into_iter().collect())
    }

    /// Direct file dependencies (imports + call targets).
    #[must_use]
    pub fn file_dependencies(&self, path: &str) -> Vec<String> {
        self.files
            .deps_of(&path.to_string())
            .map(|deps| common_core::sort::sorted_vec(deps.to_vec()))
            .unwrap_or_default()
    }

    /// Symbol definitions for a name.
    #[must_use]
    pub fn definitions_of(&self, symbol: &str) -> &[SymbolDef] {
        self.symbol_defs.get(symbol).map_or(&[], Vec::as_slice)
    }

    /// Depth-1 context expansion over the hydrated graph: for each
    /// anchor, its direct dependents (single hop — never transitive)
    /// plus its direct dependencies (resolved import targets and call
    /// targets), each with provenance. Unresolved specifiers name no
    /// file and contribute nothing. Deterministic (sorted); empty
    /// anchors expand to empty.
    #[must_use]
    pub fn expand_shallow(&self, anchors: &[String]) -> Vec<ContextEdge> {
        let mut edges: Vec<ContextEdge> = Vec::new();
        for anchor in anchors {
            for dependent in self.files.direct_dependents(anchor) {
                for edge in self.import_edges.iter().filter(|edge| {
                    edge.importer == dependent && edge.resolved_file.as_deref() == Some(anchor)
                }) {
                    edges.push(ContextEdge {
                        file: dependent.clone(),
                        anchor: anchor.clone(),
                        direction: ContextDirection::Dependent,
                        family: ContextFamily::Import,
                        via: edge.specifier.clone(),
                    });
                }
                for edge in self.call_edges.iter().filter(|edge| {
                    edge.caller_file == dependent
                        && edge.callee_files.iter().any(|file| file == anchor)
                }) {
                    edges.push(ContextEdge {
                        file: dependent.clone(),
                        anchor: anchor.clone(),
                        direction: ContextDirection::Dependent,
                        family: ContextFamily::Call,
                        via: edge.callee.clone(),
                    });
                }
            }
            for edge in self.import_edges.iter().filter(|edge| {
                edge.importer.as_str() == anchor.as_str() && edge.resolved_file.is_some()
            }) {
                edges.push(ContextEdge {
                    file: edge.resolved_file.clone().unwrap_or_default(),
                    anchor: anchor.clone(),
                    direction: ContextDirection::Dependency,
                    family: ContextFamily::Import,
                    via: edge.specifier.clone(),
                });
            }
            for edge in self.call_edges.iter().filter(|edge| edge.caller_file == *anchor) {
                for callee_file in &edge.callee_files {
                    edges.push(ContextEdge {
                        file: callee_file.clone(),
                        anchor: anchor.clone(),
                        direction: ContextDirection::Dependency,
                        family: ContextFamily::Call,
                        via: edge.callee.clone(),
                    });
                }
            }
        }
        common_core::sort::dedup_sorted(edges)
    }

    /// All call edges.
    #[must_use]
    pub fn call_edges(&self) -> &[CallEdge] {
        &self.call_edges
    }

    /// All import edges (resolved and unresolved).
    #[must_use]
    pub fn import_edges(&self) -> &[ImportEdge] {
        &self.import_edges
    }

    /// L4 re-rank: boost fused candidates by graph proximity to the fused
    /// anchors (top-3 by score), then role-coverage overlap as a
    /// tiebreak-scale epsilon. Deterministic for identical inputs.
    /// Skipped (RRF order preserved) when no graph edge touches the
    /// candidate set — layered-search skip discipline.
    pub fn rerank(&self, candidates: &mut [RecallCandidate], signals: &GraphQuerySignals) {
        if candidates.is_empty() {
            return;
        }
        let candidate_files: HashSet<&str> = candidates
            .iter()
            .map(|candidate| candidate.file.absolute_path.as_str())
            .collect();
        let touches = self.call_edges.iter().any(|edge| {
            candidate_files.contains(edge.caller_file.as_str())
                || edge.callee_files.iter().any(|file| candidate_files.contains(file.as_str()))
        }) || self.import_edges.iter().any(|edge| {
            candidate_files.contains(edge.importer.as_str())
                || edge.resolved_file.as_deref().is_some_and(|file| candidate_files.contains(file))
        });
        if !touches {
            return;
        }
        let mut anchors: Vec<usize> = common_core::sort::sorted_by_vec(
            (0..candidates.len()).collect(),
            |&a, &b| {
                candidates[b]
                    .score
                    .partial_cmp(&candidates[a].score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| candidates[a].id.cmp(&candidates[b].id))
            },
        );
        anchors.truncate(3);
        let anchor_files: HashSet<&str> = anchors
            .iter()
            .map(|&index| candidates[index].file.absolute_path.as_str())
            .collect();
        let anchor_symbols: HashSet<String> = anchors
            .iter()
            .filter_map(|&index| candidate_symbol(&candidates[index]))
            .collect();
        let mut anchor_closure = HashSet::new();
        for file in &anchor_files {
            anchor_closure.insert((*file).to_string());
            for dependent in self.files.dependents_of(&(*file).to_string()) {
                anchor_closure.insert(dependent);
            }
        }
        let mut scored: Vec<(usize, f64, usize)> = Vec::with_capacity(candidates.len());
        for (index, candidate) in candidates.iter().enumerate() {
            // Structural applicability stays here (anchor/closure/scope are
            // graph-discipline decisions); the addition itself composes the
            // shared algebra (`search_vector::fusion::apply_boosts`).
            let mut boosts = Vec::with_capacity(3);
            let file = candidate.file.absolute_path.as_str();
            if anchor_files.contains(file) {
                boosts.push(L4_SAME_FILE_BOOST);
            } else if anchor_closure.contains(file) {
                boosts.push(L4_DEPENDENT_BOOST);
            }
            if let Some(symbol) = candidate_symbol(candidate) {
                if self.calls_anchor(candidate, &symbol, &anchors, candidates, &anchor_symbols) {
                    boosts.push(L4_CALL_BOOST);
                }
            }
            if Self::shares_anchor_scope(&anchors, candidates, candidate) {
                boosts.push(L4_SCOPE_BOOST);
            }
            let overlap = role_overlap(candidate, signals);
            // Local ordering key only: the composite magnitude plus the
            // structural boosts, sorted here and never stored back into
            // the candidate (composites never combine — see `RrfScore`).
            let boosted =
                search_vector::fusion::apply_boosts(candidate.score.value(), boosts);
            scored.push((index, boosted, overlap));
        }
        let scored = common_core::sort::sorted_by_vec(scored, |a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.2.cmp(&a.2))
                .then_with(|| candidates[a.0].id.cmp(&candidates[b.0].id))
        });
        // Reorder by the sorted permutation (candidates are few; the
        // clone keeps the rotation obviously correct and deterministic).
        let reordered: Vec<RecallCandidate> =
            scored.iter().map(|(index, _, _)| candidates[*index].clone()).collect();
        candidates.clone_from_slice(&reordered);
    }

    fn calls_anchor(
        &self,
        candidate: &RecallCandidate,
        symbol: &str,
        anchors: &[usize],
        candidates: &[RecallCandidate],
        anchor_symbols: &HashSet<String>,
    ) -> bool {
        let file = &candidate.file.absolute_path;
        // Candidate calls an anchor symbol, or an anchor calls the candidate.
        for edge in &self.call_edges {
            if &edge.caller_file == file
                && edge.caller_symbol.as_deref() == Some(symbol)
                && anchor_symbols.contains(&edge.callee)
            {
                return true;
            }
        }
        for &anchor in anchors {
            let anchor_file = &candidates[anchor].file.absolute_path;
            let Some(anchor_symbol) = candidate_symbol(&candidates[anchor]) else {
                continue;
            };
            for edge in &self.call_edges {
                if &edge.caller_file == anchor_file
                    && edge.caller_symbol.as_deref() == Some(anchor_symbol.as_str())
                    && edge.callee == symbol
                {
                    return true;
                }
            }
        }
        false
    }

    fn shares_anchor_scope(anchors: &[usize], candidates: &[RecallCandidate], candidate: &RecallCandidate) -> bool {
        let Some(scope) = candidate_scope(candidate) else {
            return false;
        };
        anchors.iter().any(|&anchor| {
            candidate_scope(&candidates[anchor])
                .is_some_and(|anchor_scope| anchor_scope == scope)
        })
    }
}

fn candidate_symbol(candidate: &RecallCandidate) -> Option<String> {
    match &candidate.entity.metadata {
        Some(EntityMetadata::Code { symbol_name, .. }) => symbol_name.clone(),
        _ => None,
    }
}

fn candidate_scope(candidate: &RecallCandidate) -> Option<String> {
    match &candidate.entity.metadata {
        Some(EntityMetadata::Code { scope, .. } | EntityMetadata::Markdown { scope, .. }) => {
            scope.clone()
        }
        _ => None,
    }
}

/// Role-coverage overlap: query predicate/role lemmas ∩ candidate
/// symbol/scope/file-stem tokens (tiebreak scale).
fn role_overlap(candidate: &RecallCandidate, signals: &GraphQuerySignals) -> usize {
    let mut tokens = HashSet::new();
    if let Some(symbol) = candidate_symbol(candidate) {
        tokens.extend(split_tokens(&symbol));
    }
    if let Some(scope) = candidate_scope(candidate) {
        tokens.extend(split_tokens(&scope));
    }
    if let Some(stem) = Path::new(&candidate.file.absolute_path)
        .file_stem()
        .and_then(|stem| stem.to_str())
    {
        tokens.extend(split_tokens(stem));
    }
    signals
        .predicate
        .iter()
        .chain(signals.roles.iter())
        .filter(|lemma| tokens.contains(lemma.as_str()))
        .count()
}

fn per_file_paths(per_file: &[(String, crate::extractor::code::HarvestedFile)]) -> HashSet<String> {
    per_file.iter().map(|(path, _)| path.clone()).collect()
}

/// Resolve an import specifier to a workspace file (best effort):
/// `mod:` declarations, relative paths with extension/index probing,
/// then root-anchored and dotted-module probing. `None` records the edge
/// as unresolved (never dropped).
fn resolve_import(
    importer: &str,
    specifier: &str,
    roots: &[String],
    known_files: &HashSet<String>,
) -> Option<String> {
    if let Some(module) = specifier.strip_prefix("mod:") {
        return probe_module_file(importer, module, known_files);
    }
    let cleaned = specifier.trim();
    if cleaned.is_empty() {
        return None;
    }
    // Relative specifiers resolve against the importer directory.
    if cleaned.starts_with("./") || cleaned.starts_with("../") || cleaned == "." || cleaned == ".." {
        let base = Path::new(importer).parent().unwrap_or(Path::new("/"));
        return probe_path(&base.join(cleaned), known_files);
    }
    // Dotted modules (`pkg.sub`) probe as relative segments first.
    if cleaned.contains('.') && !cleaned.contains('/') && Path::new(cleaned).extension().is_none_or(std::ffi::OsStr::is_empty) {
        let relative = cleaned.replace('.', "/");
        let base = Path::new(importer).parent().unwrap_or(Path::new("/"));
        if let Some(found) = probe_path(&base.join(&relative), known_files) {
            return Some(found);
        }
    }
    // Root-anchored probing (importer-relative dotted, roots, bare name).
    let importer_dir = Path::new(importer).parent().unwrap_or(Path::new("/"));
    for root in std::iter::once(&importer_dir.to_string_lossy().into_owned()).chain(roots.iter()) {
        let base = PathBuf::from(root);
        for candidate in [
            base.join(cleaned),
            base.join(cleaned.replace('.', "/")),
            base.join(cleaned.replace("::", "/")),
        ] {
            if let Some(found) = probe_path(&candidate, known_files) {
                return Some(found);
            }
        }
    }
    None
}

/// Rust `mod child;` (or `crate::a::b` paths) to sibling files.
fn probe_module_file(importer: &str, module: &str, known_files: &HashSet<String>) -> Option<String> {
    let dir = Path::new(importer).parent().unwrap_or(Path::new("/"));
    for candidate in [
        dir.join(format!("{module}.rs")),
        dir.join(module).join("mod.rs"),
    ] {
        if let Some(found) = probe_path(&candidate, known_files) {
            return Some(found);
        }
    }
    // `crate::a::b` paths resolve from the file's topmost module dir is
    // undecidable without the crate root; record unresolved.
    None
}

/// Probe a path with extension and index variants against known files.
pub(crate) fn probe_path(candidate: &Path, known_files: &HashSet<String>) -> Option<String> {
    let normalized = normalize_separators(&candidate.to_string_lossy());
    if known_files.contains(&normalized) {
        return Some(normalized);
    }
    [
        format!("{normalized}.ts"),
        format!("{normalized}.tsx"),
        format!("{normalized}.js"),
        format!("{normalized}.py"),
        format!("{normalized}.rs"),
        format!("{normalized}.go"),
        format!("{normalized}.java"),
        format!("{normalized}.c"),
        format!("{normalized}/index.ts"),
        format!("{normalized}/index.js"),
        format!("{normalized}/mod.rs"),
        format!("{normalized}/__init__.py"),
    ]
    .into_iter()
    .find(|suffixed| known_files.contains(suffixed))
}

pub(crate) fn normalize_separators(path: &str) -> String {
    // Lexical clean (no I/O): collapse `.`/`..` and `/` separators.
    // Delegates to the canonical `common_core::path::normalize_lexical`,
    // then re-roots relative inputs at `/` — the historical shape of this
    // helper (probe candidates are absolute, so this only pins the
    // relative/empty edge). Byte-for-byte with the pre-extraction body.
    let normalized = common_core::path::normalize_lexical(path);
    if normalized.starts_with('/') {
        normalized
    } else if normalized.is_empty() {
        "/".to_string()
    } else {
        format!("/{normalized}")
    }
}

#[cfg(test)]
#[path = "../tests/graph_index.rs"]
mod tests;

