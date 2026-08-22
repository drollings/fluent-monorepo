//! Real `ToolLookup` resolvers for the M5 lookup kinds.
//!
//! Each resolver is read-only over an existing store (session DAG, shared
//! `ContentNodeStore` ledger, chart store, SQLite data store) — nothing is
//! rebuilt here. Every resolver returns `Ok(None)` when the backing store has
//! nothing relevant (absent — the `Compose` step drops it), `Ok(Some(...))`
//! when there is real material, and `Err` only on a genuine failure (the
//! handler degrades to plain dispatch). A resolver never fabricates a string.
//!
//! `search`/`api` are deliberately NOT installed: no real client is wired in
//! M5, so a plan needing them is declined to plain dispatch (see
//! [`crate::server::tool_lookup::plan_is_executable`]).

use std::sync::Arc;

use common_core::string::truncate_utf8;
use common_core::sync::lock;
use fluent_db::store::SqliteStore;
use fluent_llm::EmbeddingProvider;
use fluent_wvr::capability::check_capability;

use super::{LookupContext, LookupError, LookupResult, ToolLookup};

/// Top-K KNN hits considered by the `knowledge_graph` resolver.
const KG_TOP_K: usize = 3;
/// Cosine-similarity floor for a `knowledge_graph` hit (below it the node is
/// "not relevant" -> absent, never a weak guess).
const KG_MIN_SCORE: f32 = 0.6;
/// Top charts surfaced by the `chart` resolver.
const CHART_TOP_K: usize = 3;
/// Hard cap on `data_store` results (a bounded pull, never a full dump).
const DS_MAX_RESULTS: usize = 8;
/// Hard cap on tables scanned by `data_store` (bounded by construction).
const DS_MAX_TABLES: usize = 16;
/// Per-item character cap so a composed answer stays bounded.
const ITEM_MAX_CHARS: usize = 160;
/// Shared cap on items per `LookupResult` (a composed answer stays bounded).
pub(crate) const MAX_LOOKUP_ITEMS: usize = 8;

/// `dag` lookup: query the per-request session's step DAG for nodes whose id
/// or description overlaps the query, surfacing each match's status, result
/// snippet, and dependents (the subgraph). Composes
/// `DependencySession`/`DependencyGraph` — never re-implements graph logic.
///
/// A request with no wired session (or none for this request) is absent
/// (`Ok(None)`).
#[derive(Debug, Default)]
pub struct DagLookup;

impl ToolLookup for DagLookup {
    fn kind(&self) -> &str {
        "dag"
    }

    fn lookup(
        &self,
        query: &str,
        ctx: &LookupContext<'_>,
    ) -> Result<Option<LookupResult>, LookupError> {
        let Some(session_arc) = ctx.session else {
            return Ok(None);
        };
        let session = lock(session_arc);
        let tokens = query_tokens(query);
        let mut items: Vec<String> = Vec::new();
        for id in session.step_ids() {
            let Some(step) = session.get_step(id) else {
                continue;
            };
            let hay = format!("{} {}", id, step.description).to_lowercase();
            let id_lower = id.to_lowercase();
            let query_lower = query.to_lowercase();
            let matches = tokens.iter().any(|t| hay.contains(t))
                || id_lower.contains(&query_lower)
                || query_lower.contains(&id_lower);
            if !matches {
                continue;
            }
            let status = format!("{:?}", step.status).to_lowercase();
            let result = step
                .result
                .as_ref()
                .map(|r| format!(" -> {}", clip(&r.content)))
                .unwrap_or_default();
            items.push(format!("{id} [{status}]{result}"));
            for dep in session.graph().dependents_of(id) {
                if dep != *id {
                    items.push(format!("  depends-on: {dep}"));
                }
            }
            if items.len() >= MAX_LOOKUP_ITEMS {
                break;
            }
        }
        if items.is_empty() {
            Ok(None)
        } else {
            Ok(Some(LookupResult::new("DAG steps", items)))
        }
    }
}

/// `knowledge_graph` lookup: embed the query and run
/// `ContentNodeStore::knn_search` (the shared brute-force cosine path) over the
/// session ledger's nodes, surfacing only hits at or above [`KG_MIN_SCORE`].
/// The node's LOD0 text is the returned material — never a synthesized
/// summary.
///
/// No ledger wired -> absent. Embedding failure -> `Err` (degrade to dispatch).
pub struct KnowledgeGraphLookup {
    embedder: Arc<dyn EmbeddingProvider>,
}

impl KnowledgeGraphLookup {
    pub fn new(embedder: Arc<dyn EmbeddingProvider>) -> Self {
        Self { embedder }
    }
}

impl ToolLookup for KnowledgeGraphLookup {
    fn kind(&self) -> &str {
        "knowledge_graph"
    }

    fn lookup(
        &self,
        query: &str,
        ctx: &LookupContext<'_>,
    ) -> Result<Option<LookupResult>, LookupError> {
        let Some(ledger) = ctx.ledger else {
            return Ok(None);
        };
        let store = ledger.node_store();
        let embedding = self
            .embedder
            .embed(query)
            .map_err(|e| LookupError::Failed("knowledge_graph".into(), e.to_string()))?;
        if embedding.is_empty() {
            return Ok(None);
        }
        let mut items: Vec<String> = Vec::new();
        for hit in store.knn_search(&embedding, KG_TOP_K) {
            let sim = (1.0 - hit.distance).max(0.0);
            if sim < KG_MIN_SCORE {
                continue;
            }
            let text = store
                .snapshot(hit.node_id)
                .and_then(|n| n.content().map(str::to_string))
                .unwrap_or_else(|| hit.name.to_string());
            items.push(format!("{} ({:.2})", clip(&text), sim));
            if items.len() >= MAX_LOOKUP_ITEMS {
                break;
            }
        }
        if items.is_empty() {
            Ok(None)
        } else {
            Ok(Some(LookupResult::new("knowledge graph", items)))
        }
    }
}

/// `chart` lookup: retrieve charts via the shared `ChartStore`'s
/// HNSW/brute-force `search` (the same store the plan route serves). An
/// unbuilt index or an empty result set is absent; an embedding/search failure
/// is `Err`.
pub struct ChartLookup {
    store: Arc<crate::charts::store::ChartStore>,
}

impl ChartLookup {
    pub fn new(store: Arc<crate::charts::store::ChartStore>) -> Self {
        Self { store }
    }
}

impl ToolLookup for ChartLookup {
    fn kind(&self) -> &str {
        "chart"
    }

    fn lookup(
        &self,
        query: &str,
        _ctx: &LookupContext<'_>,
    ) -> Result<Option<LookupResult>, LookupError> {
        let hits = self
            .store
            .search(query, CHART_TOP_K)
            .map_err(|e| LookupError::Failed("chart".into(), e.to_string()))?;
        if hits.is_empty() {
            return Ok(None);
        }
        let items: Vec<String> = hits
            .iter()
            .map(|(name, sim)| format!("{name} (match {sim:.2})"))
            .collect();
        Ok(Some(LookupResult::new("charts", items)))
    }
}

/// `entity_tool` lookup: entity <-> tool associations recorded in the session
/// DAG and the ledger. A completed session step whose id/description is
/// tool-shaped is one association (tool -> its result's content, which names
/// the entity); a ledger node with a tool-shaped role is another. Only
/// associations whose text overlaps the query survive; nothing is synthesized.
#[derive(Debug, Default)]
pub struct EntityToolLookup;

/// Step/role shapes that mark a tool invocation or tool-result node.
const TOOL_MARKERS: &[&str] = &[
    "tool",
    "lookup",
    "search",
    "fetch",
    "read",
    "query",
    "chart",
    "dispatch",
    "agent",
    "api",
];

impl ToolLookup for EntityToolLookup {
    fn kind(&self) -> &str {
        "entity_tool"
    }

    fn lookup(
        &self,
        query: &str,
        ctx: &LookupContext<'_>,
    ) -> Result<Option<LookupResult>, LookupError> {
        let tokens = query_tokens(query);
        let mut items: Vec<String> = Vec::new();

        if let Some(session_arc) = ctx.session {
            let session = lock(session_arc);
            for id in session.step_ids() {
                let Some(step) = session.get_step(id) else {
                    continue;
                };
                if !toolish(&format!("{id} {}", step.description)) {
                    continue;
                }
                let Some(result) = step.result.as_ref() else {
                    continue;
                };
                let hay = format!("{id} {}", result.content).to_lowercase();
                if tokens.iter().any(|t| hay.contains(t)) {
                    items.push(format!("{id} -> {}", clip(&result.content)));
                }
            }
        }

        if let Some(ledger) = ctx.ledger {
            let session_id = ctx.request.session_id.clone().unwrap_or_default();
            if let Ok(nodes) = ledger.get_session_nodes(&session_id, 50) {
                for node in nodes {
                    let role = node.role.as_deref().unwrap_or("");
                    if !toolish(role) {
                        continue;
                    }
                    let Some(content) = node.content() else {
                        continue;
                    };
                    let hay = format!("{role} {content}").to_lowercase();
                    if tokens.iter().any(|t| hay.contains(t)) {
                        items.push(format!("{role} -> {}", clip(content)));
                    }
                }
            }
        }

        if items.is_empty() {
            Ok(None)
        } else {
            items.truncate(MAX_LOOKUP_ITEMS);
            Ok(Some(LookupResult::new("entity-tool associations", items)))
        }
    }
}

/// `data_store` lookup: a bounded, capability-gated read over the operator's
/// SQLite data store (`fluent-db::store::SqliteStore` — the synchronous
/// surface of the same crate that owns `SqlitePool`).
///
/// Capability-gated: the router's knowledge token must be present in the
/// current task-local (installed on the serving request path); outside that
/// path the store is not consultable -> absent. Reads run a `LIKE` scan over
/// each table's text columns (schema from `sqlite_master`/`PRAGMA
/// table_info`, identifiers quoted, parameters bound), capped at
/// [`DS_MAX_RESULTS`] rows and [`DS_MAX_TABLES`] tables. Only rows overlapping
/// the query are surfaced; never fabricated.
pub struct DataStoreLookup {
    store: Arc<SqliteStore>,
}

impl DataStoreLookup {
    pub fn new(store: Arc<SqliteStore>) -> Self {
        Self { store }
    }

    /// Open a resolver over the SQLite data store at `path` (WAL mode, the
    /// same `SqliteStore` primitive the shared `ContentNodeStore` uses).
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, fluent_db::error::DbError> {
        Ok(Self::new(Arc::new(SqliteStore::open(path.as_ref())?)))
    }

    /// User tables in the store (schema from `sqlite_master`), bounded.
    fn tables(&self) -> Result<Vec<String>, LookupError> {
        let rows = self
            .store
            .query_rows(
                "SELECT name FROM sqlite_master WHERE type = 'table' \
                 AND name NOT LIKE 'sqlite_%' ORDER BY name LIMIT ?1",
                &[&(DS_MAX_TABLES as i64)],
                |row| row.get::<_, String>(0),
            )
            .map_err(|e| LookupError::Failed("data_store".into(), e.to_string()))?;
        Ok(rows)
    }

    /// TEXT columns of a table (schema from `PRAGMA table_info`), quoted.
    fn text_columns(&self, table: &str) -> Result<Vec<String>, LookupError> {
        let pragma = format!("PRAGMA table_info({})", quote_ident(table));
        let rows = self
            .store
            .query_rows(&pragma, &[], |row| {
                let name: String = row.get(1)?;
                let kind: String = row.get(2)?;
                Ok((name, kind))
            })
            .map_err(|e| LookupError::Failed("data_store".into(), e.to_string()))?;
        Ok(rows
            .into_iter()
            .filter(|(_, kind)| {
                let k = kind.to_lowercase();
                k.contains("char") || k.contains("text") || k.contains("clob")
            })
            .map(|(name, _)| quote_ident(&name))
            .collect())
    }
}

impl ToolLookup for DataStoreLookup {
    fn kind(&self) -> &str {
        "data_store"
    }

    fn lookup(
        &self,
        query: &str,
        _ctx: &LookupContext<'_>,
    ) -> Result<Option<LookupResult>, LookupError> {
        if check_capability(&crate::knowledge::RouterKnowledgeCapability).is_err() {
            return Ok(None);
        }
        let like = format!(
            "%{}%",
            query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
        );
        let mut items: Vec<String> = Vec::new();
        let tokens = query_tokens(query);
        for table in self.tables()? {
            if items.len() >= DS_MAX_RESULTS {
                break;
            }
            for col in self.text_columns(&table)? {
                if items.len() >= DS_MAX_RESULTS {
                    break;
                }
                let sql = format!("SELECT {col} FROM {table} WHERE {col} LIKE ?1 ESCAPE '\\' LIMIT ?2");
                let rows = self
                    .store
                    .query_rows(&sql, &[&like, &(DS_MAX_RESULTS as i64)], |row| {
                        row.get::<_, String>(0)
                    })
                    .map_err(|e| LookupError::Failed("data_store".into(), e.to_string()))?;
                for value in rows {
                    if value.trim().is_empty() {
                        continue;
                    }
                    let hay = value.to_lowercase();
                    if tokens.iter().any(|t| hay.contains(t)) {
                        items.push(format!("{table}.{col}: {}", clip(&value)));
                    }
                    if items.len() >= DS_MAX_RESULTS {
                        break;
                    }
                }
            }
        }
        if items.is_empty() {
            Ok(None)
        } else {
            Ok(Some(LookupResult::new("data store", items)))
        }
    }
}

/// Quote a SQLite identifier (double quotes, embedded quotes doubled).
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Whether a step id / role / description reads as a tool invocation.
fn toolish(hay: &str) -> bool {
    let hay = hay.to_lowercase();
    TOOL_MARKERS.iter().any(|m| hay.contains(m))
}

/// Lowercase alphanumeric tokens of the query (length >= 3) used for the
/// deterministic overlap matching across the DAG / entity-tool / data-store
/// resolvers.
fn query_tokens(query: &str) -> Vec<String> {
    query
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| t.len() >= 3)
        .map(str::to_lowercase)
        .collect()
}

/// Clip a lookup item to a bounded length (never synthesized — just truncated).
fn clip(s: &str) -> String {
    let t = truncate_utf8(s, ITEM_MAX_CHARS);
    if t.len() < s.len() {
        format!("{t}…")
    } else {
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag_session::{DependencySession, SessionStep, StepResult};
    use crate::ledger::ContentNodeLedger;
    use crate::test_stubs::HashEmbedder;
    use crate::testing::test_request;
    use crate::types::RouterRequest;
    use fluent_wvr::capability::CURRENT_CAPS;
    use fluent_wvr::CapabilitySet;
    use std::sync::Mutex;

    fn empty_request() -> RouterRequest {
        test_request("lookup query")
    }

    fn ctx<'a>(
        request: &'a RouterRequest,
        user_text: &'a str,
        ledger: Option<&'a Arc<ContentNodeLedger>>,
        session: Option<&'a Arc<Mutex<DependencySession>>>,
    ) -> LookupContext<'a> {
        LookupContext {
            request,
            ledger,
            user_text,
            session,
        }
    }

    fn ok_result(content: &str) -> StepResult {
        StepResult {
            content: content.into(),
            accepted: true,
            score: Some(0.9),
            latency_ms: 10,
            error: None,
        }
    }

    // ── dag ─────────────────────────────────────────────────────────────

    #[test]
    fn dag_lookup_matches_step_by_query_overlap() {
        let req = empty_request();
        let mut session = DependencySession::new("sess-1");
        session
            .add_step(SessionStep::new("identify_subject", "Identify the subject"))
            .unwrap();
        session
            .add_step(SessionStep::new("search", "Search the web"))
            .unwrap();
        session
            .complete_step("identify_subject", ok_result("the greenhouse effect"))
            .unwrap();
        let session = Arc::new(Mutex::new(session));

        let lookup = DagLookup;
        let result = lookup
            .lookup(
                "identify the subject",
                &ctx(&req, "identify the subject", None, Some(&session)),
            )
            .unwrap()
            .expect("a matching step resolves");
        assert!(result.items[0].contains("identify_subject"));
        assert!(result.items[0].contains("completed"), "status surfaced");
        assert!(
            result.items[0].contains("the greenhouse effect"),
            "result snippet surfaced: {:?}",
            result.items
        );
    }

    #[test]
    fn dag_lookup_absent_without_session() {
        let req = empty_request();
        let lookup = DagLookup;
        assert!(lookup.lookup("anything", &ctx(&req, "anything", None, None)).unwrap().is_none());
    }

    #[test]
    fn dag_lookup_absent_when_nothing_matches() {
        let req = empty_request();
        let session = Arc::new(Mutex::new(DependencySession::new("sess-1")));
        let lookup = DagLookup;
        assert!(
            lookup.lookup("query", &ctx(&req, "query", None, Some(&session))).unwrap().is_none()
        );
    }

    // ── knowledge_graph ─────────────────────────────────────────────────

    fn temp_ledger() -> Arc<ContentNodeLedger> {
        Arc::new(ContentNodeLedger::open_in_memory().unwrap())
    }

    #[test]
    fn knowledge_graph_lookup_returns_real_node_text() {
        let ledger = temp_ledger();
        let embedder = Arc::new(HashEmbedder::new(256));
        let mut node = crate::node_store::new_node(
            fluent_types::NodeId::from_int(9001),
            "sess-kg",
            "r1",
            "assistant",
            "embedding target about coral routing",
            Some(true),
        );
        // The node's embedding must live in the same vector space as the
        // query embedding (the HashEmbedder's), so the cosine KNN is
        // dimension-consistent.
        node.embedding = embedder.embed("embedding target about coral routing").ok();
        ledger.record_content_node(&node).unwrap();
        let req = empty_request();

        let lookup = KnowledgeGraphLookup::new(embedder);
        let result = lookup
            .lookup("coral routing", &ctx(&req, "coral routing", Some(&ledger), None))
            .unwrap()
            .expect("a node at or above the similarity floor resolves");
        assert!(
            result.items[0].contains("embedding target about coral routing"),
            "real node LOD0 text must be the material: {:?}",
            result.items
        );
    }

    #[test]
    fn knowledge_graph_lookup_absent_without_ledger() {
        let req = empty_request();
        let lookup = KnowledgeGraphLookup::new(Arc::new(HashEmbedder::new(256)));
        assert!(
            lookup.lookup("coral routing", &ctx(&req, "coral routing", None, None)).unwrap().is_none()
        );
    }

    // ── chart ───────────────────────────────────────────────────────────

    #[test]
    fn chart_lookup_absent_when_index_unbuilt() {
        let store = Arc::new(crate::charts::store::ChartStore::new(None));
        let req = empty_request();
        let lookup = ChartLookup::new(store);
        // No index built -> search returns empty -> absent, never an error.
        assert!(lookup.lookup("anything", &ctx(&req, "anything", None, None)).unwrap().is_none());
    }

    #[test]
    fn chart_lookup_returns_chart_names_from_indexed_store() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = Arc::new(crate::charts::store::ChartStore::new(Some(
            crate::hnsw::HnswIndexHandle {
                name: "workflow_library".into(),
                path: tmp
                    .path()
                    .join("workflow_library.sqlite")
                    .display()
                    .to_string(),
            },
        )));
        let chart = crate::charts::store::chart_from_str(
            r#"{
                "name": "bug_triage",
                "description": "triage a bug report",
                "schema_version": 1,
                "author_model": "human",
                "targets": [{
                    "name": "target_a",
                    "provides": ["bug_analysis"],
                    "template": "analyze { request }",
                    "essential": true
                }]
            }"#,
        )
        .unwrap();
        store.upsert(chart).unwrap();
        store
            .build_index(Arc::new(HashEmbedder::new(256)))
            .expect("index builds");
        let req = empty_request();

        let lookup = ChartLookup::new(store);
        let result = lookup
            .lookup("triage this bug", &ctx(&req, "triage this bug", None, None))
            .unwrap()
            .expect("a matching chart resolves");
        assert!(result.items[0].contains("bug_triage"), "{:?}", result.items);
    }

    // ── entity_tool ─────────────────────────────────────────────────────

    #[test]
    fn entity_tool_lookup_finds_tool_association_in_session() {
        let req = empty_request();
        let mut session = DependencySession::new("sess-1");
        session
            .add_step(SessionStep::new("entity_lookup", "Look up the entity"))
            .unwrap();
        session
            .complete_step("entity_lookup", ok_result("associated company Acme Corp"))
            .unwrap();
        let session = Arc::new(Mutex::new(session));

        let lookup = EntityToolLookup;
        let result = lookup
            .lookup("acme", &ctx(&req, "acme", None, Some(&session)))
            .unwrap()
            .expect("a matching tool association resolves");
        assert!(
            result.items[0].contains("entity_lookup") && result.items[0].contains("Acme Corp"),
            "{:?}",
            result.items
        );
    }

    #[test]
    fn entity_tool_lookup_absent_with_no_association() {
        let req = empty_request();
        let session = Arc::new(Mutex::new(DependencySession::new("sess-1")));
        let lookup = EntityToolLookup;
        assert!(
            lookup.lookup("acme", &ctx(&req, "acme", None, Some(&session))).unwrap().is_none()
        );
    }

    // ── data_store ──────────────────────────────────────────────────────

    fn temp_data_store() -> Arc<SqliteStore> {
        let store = SqliteStore::open_in_memory().unwrap();
        store
            .execute(
                "CREATE TABLE widgets (id INTEGER PRIMARY KEY, name TEXT, qty INTEGER)",
                &[],
            )
            .unwrap();
        store
            .execute(
                "INSERT INTO widgets (name, qty) VALUES ('coral widget', 5), ('other thing', 1)",
                &[],
            )
            .unwrap();
        Arc::new(store)
    }

    #[tokio::test]
    async fn data_store_lookup_reads_matching_rows_within_capability() {
        let store = temp_data_store();
        let req = empty_request();
        let lookup = DataStoreLookup::new(store);

        // Inside the router's capability scope the read is allowed and returns
        // the matching row's real value.
        CURRENT_CAPS
            .scope(
                CapabilitySet::new().with(crate::knowledge::RouterKnowledgeCapability),
                async {
                    let result = lookup
                        .lookup("coral", &ctx(&req, "coral", None, None))
                        .unwrap()
                        .expect("matching row resolves");
                    assert!(
                        result.items[0].contains("coral widget"),
                        "real row value surfaced: {:?}",
                        result.items
                    );
                },
            )
            .await;
    }

    #[tokio::test]
    async fn data_store_lookup_denied_outside_capability() {
        let store = temp_data_store();
        let req = empty_request();
        let lookup = DataStoreLookup::new(store);

        // No router capability token -> the store is not consultable -> absent
        // (never an error, never a fabricated row).
        CURRENT_CAPS
            .scope(CapabilitySet::new(), async {
                assert!(
                    lookup.lookup("coral", &ctx(&req, "coral", None, None)).unwrap().is_none()
                );
            })
            .await;
    }
}