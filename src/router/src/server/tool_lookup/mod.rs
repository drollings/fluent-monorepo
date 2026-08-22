//! Read-only tool-plan lookup resolvers.
//!
//! A tool plan's `Lookup` step names a lookup *kind* (`dag`,
//! `knowledge_graph`, `chart`, `entity_tool`, `data_store`, ...). Each kind
//! maps to a [`ToolLookup`] resolver — a read-only lookup over an existing
//! store (ledger/DAG/charts/db). A kind with no installed resolver is
//! **unsupported**: a plan needing it declines to plain group dispatch rather
//! than emitting placeholder text (VISION: "no fabricated data").
//!
//! Resolvers are additive (SOLID OCP): new kinds register into the
//! [`ToolLookupRegistry`] without touching the dispatch path. The registry
//! follows the `fluent-wvr` trait-object registry pattern (a `kind ->
//! Arc<dyn ToolLookup>` map), built once at server boot from the same stores
//! the ledger/charts already use.

pub mod resolvers;

pub use resolvers::{ChartLookup, DagLookup, DataStoreLookup, EntityToolLookup, KnowledgeGraphLookup};

use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, Mutex};

use crate::config::{ToolPlan, ToolPlanStepKind};
use crate::dag_session::DependencySession;
use crate::ledger::ContentNodeLedger;
use crate::types::RouterRequest;

/// A read-only lookup resolver for one tool-plan `Lookup` kind.
///
/// A resolver returns `Ok(Some(result))` when the backing store has relevant
/// material, `Ok(None)` when it has nothing (absent — the caller drops the
/// step, it is never synthesized), and `Err` when the lookup genuinely failed
/// (the caller degrades to plain dispatch, never a half-composed answer).
pub trait ToolLookup: Send + Sync {
    /// The lookup kind this resolver serves: `"dag"`, `"knowledge_graph"`,
    /// `"chart"`, `"entity_tool"`, `"data_store"`, ...
    fn kind(&self) -> &str;

    /// Run the read-only lookup for `query` against the stores in `ctx`.
    fn lookup(
        &self,
        query: &str,
        ctx: &LookupContext<'_>,
    ) -> Result<Option<LookupResult>, LookupError>;
}

/// The read-only stores a `Lookup` step may consult, plus the router request.
///
/// Borrows are ephemeral and scoped to one step execution — the handler
/// builds this for each lookup. Optional fields are `None` when the server did
/// not wire that store (a resolver whose backing store is absent returns
/// `Ok(None)`, never an error).
pub struct LookupContext<'a> {
    /// The incoming request (session/instance/snapshot context).
    pub request: &'a RouterRequest,
    /// The session ledger (knowledge-graph / entity-tool lookups). `None`
    /// when no ledger is wired.
    pub ledger: Option<&'a Arc<ContentNodeLedger>>,
    /// The request text (the query the lookup resolves against).
    pub user_text: &'a str,
    /// The per-request dependency session (DAG / entity-tool lookups). `None`
    /// when no session registry is wired or the request carries no session —
    /// a `dag`/`entity_tool` resolver degrades to absent (`Ok(None)`).
    pub session: Option<&'a Arc<Mutex<DependencySession>>>,
}

/// A typed, bounded lookup result — the material a `Compose` step may join.
///
/// `Display` renders a compact bullet list under a label so the `Compose`
/// step can concatenate it; `is_empty()` lets `Compose` drop an absent result
/// (nothing to say) rather than ever synthesizing placeholder text.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LookupResult {
    /// Human-readable heading for the result set (e.g. the lookup kind).
    pub label: String,
    /// The resolved items, each rendered on its own line by `Display`.
    pub items: Vec<String>,
}

impl LookupResult {
    pub fn new(label: impl Into<String>, items: Vec<String>) -> Self {
        Self {
            label: label.into(),
            items,
        }
    }

    /// Whether the result carries no material (an absent lookup).
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

impl Display for LookupResult {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if self.items.is_empty() {
            return Ok(());
        }
        writeln!(f, "{}:", self.label)?;
        for item in &self.items {
            writeln!(f, "- {item}")?;
        }
        Ok(())
    }
}

/// A lookup failure — distinct from *absence* (`Ok(None)`).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LookupError {
    /// The lookup kind has no installed resolver. `plan_is_executable`
    /// prevents this from reaching a resolver, but it is kept as a
    /// first-class variant so a registry built incrementally degrades
    /// predictably (a plan needing it is declined to plain dispatch).
    #[error("lookup kind '{0}' has no installed resolver")]
    Unsupported(String),
    /// The backing store failed (I/O, capability, decode).
    #[error("lookup kind '{0}' failed: {1}")]
    Failed(String, String),
}

/// Registry mapping lookup kinds to their resolvers — additive by design.
#[derive(Default, Clone)]
pub struct ToolLookupRegistry {
    resolvers: HashMap<String, Arc<dyn ToolLookup>>,
}

impl ToolLookupRegistry {
    pub fn new() -> Self {
        Self {
            resolvers: HashMap::new(),
        }
    }

    /// Register a resolver for its `kind()`. A later registration for the same
    /// kind replaces the earlier one (the last writer wins).
    pub fn register(&mut self, resolver: Arc<dyn ToolLookup>) {
        self.resolvers.insert(resolver.kind().to_string(), resolver);
    }

    /// The resolver for `kind`, if installed.
    pub fn get(&self, kind: &str) -> Option<Arc<dyn ToolLookup>> {
        self.resolvers.get(kind).cloned()
    }

    /// Whether `kind` has an installed resolver.
    pub fn supports(&self, kind: &str) -> bool {
        self.resolvers.contains_key(kind)
    }

    /// The installed kinds (sorted for determinism).
    pub fn kinds(&self) -> Vec<String> {
        let mut kinds: Vec<String> = self.resolvers.keys().cloned().collect();
        kinds.sort();
        kinds
    }

    /// Number of installed resolvers.
    pub fn len(&self) -> usize {
        self.resolvers.len()
    }

    /// Whether no resolvers are installed (the M0 empty registry).
    pub fn is_empty(&self) -> bool {
        self.resolvers.is_empty()
    }
}

/// Whether a plan can be executed: every `Lookup` step's `lookup_kind` must
/// have an installed resolver, or the plan has no `Lookup` steps. A plan with
/// a `Lookup` step of an unsupported (or unspecified) kind is **not**
/// executable — the handler declines it to plain group dispatch rather than
/// running a placeholder lookup.
pub fn plan_is_executable(plan: &ToolPlan, registry: &ToolLookupRegistry) -> bool {
    plan.steps.iter().all(|step| match &step.kind {
        ToolPlanStepKind::Lookup => step
            .lookup_kind
            .as_deref()
            .is_some_and(|kind| registry.supports(kind)),
        _ => true,
    })
}

/// Compose the registry from the stores the deployment configures.
///
/// The single registry-construction site (DRY): the boot path and the
/// hermetic tests build the same registry. A kind whose backing store is
/// absent is **not** installed — a plan needing it is declined to plain group
/// dispatch (never a placeholder lookup):
///
/// - `dag` — always installed; resolves against the per-request session's step
///   graph, absent when the request has no session.
/// - `knowledge_graph` / `entity_tool` — installed when a ledger is wired
///   (knowledge_graph additionally needs an embedding provider).
/// - `chart` — installed when a chart store is wired.
/// - `data_store` — installed only when `config.tool_lookups.data_store_path`
///   is set and the store opens.
/// - `search` / `api` — never installed (no real client); plans needing them
///   are declined.
pub fn build_registry(
    config: &crate::config::RouterConfig,
    ledger: Option<&Arc<ContentNodeLedger>>,
    chart_store: Option<&Arc<crate::charts::store::ChartStore>>,
    embedder: Option<Arc<dyn fluent_llm::EmbeddingProvider>>,
) -> ToolLookupRegistry {
    let mut registry = ToolLookupRegistry::new();
    registry.register(Arc::new(resolvers::DagLookup));

    if ledger.is_some() {
        registry.register(Arc::new(resolvers::EntityToolLookup));
        if let Some(embedder) = embedder {
            registry.register(Arc::new(resolvers::KnowledgeGraphLookup::new(embedder)));
        } else {
            tracing::warn!(
                target: "router.server.tool_lookup",
                "knowledge_graph lookup: no embedder derivable - kind uninstalled (plans needing it decline)",
            );
        }
    }

    if let Some(store) = chart_store {
        registry.register(Arc::new(resolvers::ChartLookup::new(Arc::clone(store))));
    }

    if let Some(path) = &config.tool_lookups.data_store_path {
        match resolvers::DataStoreLookup::open(path) {
            Ok(resolver) => {
                tracing::info!(
                    target: "router.server.tool_lookup",
                    data_store = %path,
                    "data_store lookup installed",
                );
                registry.register(Arc::new(resolver));
            }
            Err(e) => tracing::warn!(
                target: "router.server.tool_lookup",
                data_store = %path,
                error = %e,
                "data_store lookup: open failed - kind uninstalled (plans needing it decline)",
            ),
        }
    }

    tracing::info!(
        target: "router.server.tool_lookup",
        lookup_kinds = ?registry.kinds(),
        "tool-plan lookup resolvers installed",
    );
    registry
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ToolPlan, ToolPlanStep, ToolPlanStepKind};

    fn step(id: &str, kind: ToolPlanStepKind, lookup_kind: Option<&str>) -> ToolPlanStep {
        ToolPlanStep {
            id: id.into(),
            kind,
            target_group: None,
            description: None,
            lookup_kind: lookup_kind.map(ToOwned::to_owned),
            step_max_rounds: None,
        }
    }

    #[test]
    fn lookup_result_display_renders_bullet_list() {
        let r = LookupResult::new("knowledge graph", vec!["node A".into(), "node B".into()]);
        let text = r.to_string();
        assert!(text.contains("knowledge graph:"));
        assert!(text.contains("- node A"));
        assert!(text.contains("- node B"));
    }

    #[test]
    fn lookup_result_is_empty_contract() {
        let empty = LookupResult::new("dag", vec![]);
        assert!(empty.is_empty());
        assert_eq!(empty.to_string(), "", "an empty result renders nothing");
        let full = LookupResult::new("dag", vec!["x".into()]);
        assert!(!full.is_empty());
    }

    #[test]
    fn plan_without_lookup_steps_is_executable_on_empty_registry() {
        let registry = ToolLookupRegistry::new();
        assert!(registry.is_empty());
        let plan = ToolPlan {
            steps: vec![
                step("dispatch", ToolPlanStepKind::Dispatch, None),
                step("compose", ToolPlanStepKind::Compose, None),
            ],
            max_rounds: Some(3),
        };
        assert!(plan_is_executable(&plan, &registry));
    }

    #[test]
    fn lookup_only_plan_declines_on_empty_registry() {
        let registry = ToolLookupRegistry::new();
        let plan = ToolPlan {
            steps: vec![step("lookup", ToolPlanStepKind::Lookup, Some("dag"))],
            max_rounds: Some(3),
        };
        assert!(!plan_is_executable(&plan, &registry));
    }

    #[test]
    fn lookup_step_without_kind_is_not_executable() {
        let registry = ToolLookupRegistry::new();
        let plan = ToolPlan {
            steps: vec![step("lookup", ToolPlanStepKind::Lookup, None)],
            max_rounds: Some(3),
        };
        assert!(!plan_is_executable(&plan, &registry));
    }

    #[test]
    fn plan_becomes_executable_when_kind_resolver_installed() {
        let mut registry = ToolLookupRegistry::new();
        assert!(!registry.supports("dag"));
        registry.register(Arc::new(StubLookup::new("dag")));
        assert!(registry.supports("dag"));
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.kinds(), vec!["dag".to_string()]);
        let plan = ToolPlan {
            steps: vec![step("lookup", ToolPlanStepKind::Lookup, Some("dag"))],
            max_rounds: Some(3),
        };
        assert!(plan_is_executable(&plan, &registry));
    }

    struct StubLookup {
        kind: &'static str,
    }

    impl StubLookup {
        fn new(kind: &'static str) -> Self {
            Self { kind }
        }
    }

    impl ToolLookup for StubLookup {
        fn kind(&self) -> &str {
            self.kind
        }

        fn lookup(
            &self,
            _query: &str,
            _ctx: &LookupContext<'_>,
        ) -> Result<Option<LookupResult>, LookupError> {
            Ok(Some(LookupResult::new(self.kind, vec!["stub".into()])))
        }
    }
}