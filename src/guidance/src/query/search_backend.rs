use common_core::string::contains_ignore_case;
use fluent_types::GuidanceDoc;
use fluent_wvr::{ArcIntern, Component, Describable, WorkContext, WorkError, WorkOutput, WorkUnit};

use super::identifier;
use super::llm_filter::LlmFilter;
use super::recall::RecallStorage;
use super::strategy::QueryIntent;
use super::synthesize::{Stage, Synthesizer};
use crate::query_engine::QueryEngineError;

/// Shared context for search backends — avoids threading individual references
/// through every method.
///
/// R.1 (P1): the `WordIndex` fallback is deleted — lexical recall lives in
/// the fragment index (`storage`). `storage`/`embedder`/`nlp` are `None` on
/// the legacy doc path; index-backed backends decline (`NoResults`) without
/// them so doc backends serve.
pub struct SearchContext<'a> {
    /// Fragment index for hybrid backends.
    pub storage: Option<&'a dyn RecallStorage>,
    /// Query embedder for the vector backend.
    pub embedder: Option<&'a dyn fluent_llm::embeddings::EmbeddingProvider>,
    /// Query lemmatizer for the lemma backend.
    pub nlp: Option<&'a spacy_rs::pipeline::NlpPipeline>,
    /// Relevance filter for the concept backend (R.2: retained rung).
    pub llm_filter: &'a LlmFilter,
    /// Taxonomy store for concept groundedness (`None` = today's behavior:
    /// gates and order exactly as without a store). Guidance never owns or
    /// loads a store; the owner passes `&dyn` (router) or tests pass a
    /// hermetic in-memory store.
    pub concept_store: Option<&'a dyn fluent_concept::ConceptStore>,
}

/// Polymorphic search backend — the fluent-wvr control plane for query dispatch.
///
/// Each backend handles one `QueryIntent`. The orchestrator iterates registered
/// backends and calls `matches` + `search` without branching on implementation.
/// Backends are deliberately not `Component`s: `search` needs a live
/// `SearchContext` (storage/embedder/NLP/filter refs) that no handoff
/// channel can carry, so invocation stays native. Registration pairs the
/// native dispatch view with an inventory-only adapter handle
/// (`RegisteredBackend`): names and schema stay queryable, execution
/// stays native.
pub trait SearchBackend: Send + Sync {
    /// Stable handle name (`backend.identifier`).
    fn name(&self) -> &'static str;
    /// Inventory slug (`identifier`).
    fn kind(&self) -> &'static str;
    /// Returns true if this backend handles the given intent.
    fn matches(&self, intent: QueryIntent) -> bool;

    /// Whether this rung spends model inference (LLM scoring, embedding).
    /// Deterministic rungs return false. Exact precheck verdicts skip
    /// model-backed fallback rungs; ambiguous queries keep the full ladder.
    fn uses_model(&self) -> bool {
        false
    }

    /// Additive tier label for this rung. Pure naming; dispatch and
    /// fallback never consult it.
    fn retrieval_tier(&self) -> crate::search_types::RetrievalTier {
        crate::search_types::RetrievalTier::Lexical
    }

    /// Execute the search and return synthesized stages.
    ///
    /// Rung defense contract (enforced by the query ladder, not here):
    /// decline with [`NoResults`](crate::query_engine::QueryEngineError::NoResults)
    /// when this rung cannot serve (missing index, unresolvable query) and
    /// the next rung is tried; return a hard error only to abort the whole
    /// query. A rung never fabricates stages to avoid declining.
    fn search(
        &self,
        query: &str,
        doc: &GuidanceDoc,
        ctx: &SearchContext<'_>,
    ) -> Result<Vec<Stage>, QueryEngineError>;
}

/// Search by exact or fuzzy member name.
pub struct IdentifierBackend;

impl SearchBackend for IdentifierBackend {
    fn name(&self) -> &'static str {
        "backend.identifier"
    }
    fn kind(&self) -> &'static str {
        "identifier"
    }
    fn matches(&self, intent: QueryIntent) -> bool {
        matches!(intent, QueryIntent::SingleIdentifier)
    }

    fn retrieval_tier(&self) -> crate::search_types::RetrievalTier {
        crate::search_types::RetrievalTier::Exact
    }

    fn search(
        &self,
        query: &str,
        doc: &GuidanceDoc,
        _ctx: &SearchContext<'_>,
    ) -> Result<Vec<Stage>, QueryEngineError> {
        let matched_names: Vec<String> = identifier::find_members_by_name(doc, query)
            .into_iter()
            .map(ToString::to_string)
            .collect();

        let grounded = crate::grounding::grounded_names(doc, &matched_names);
        if !grounded.is_empty() {
            return Ok(Synthesizer::synthesize(query, doc, &grounded));
        }

        let sig_matches = identifier::find_members_by_signature(doc, query);
        if !sig_matches.is_empty() {
            let sig_names: Vec<String> = sig_matches.into_iter().map(ToString::to_string).collect();
            let grounded = crate::grounding::grounded_names(doc, &sig_names);
            if !grounded.is_empty() {
                return Ok(Synthesizer::synthesize(query, doc, &grounded));
            }
        }

        Err(QueryEngineError::NoResults)
    }
}

/// Search by keyword matching across member names and comments.
pub struct KeywordBackend;

impl SearchBackend for KeywordBackend {
    fn name(&self) -> &'static str {
        "backend.keyword"
    }
    fn kind(&self) -> &'static str {
        "keyword"
    }
    fn matches(&self, intent: QueryIntent) -> bool {
        matches!(
            intent,
            QueryIntent::CapabilityQuery | QueryIntent::MultiKeyword
        )
    }

    fn search(
        &self,
        query: &str,
        doc: &GuidanceDoc,
        _ctx: &SearchContext<'_>,
    ) -> Result<Vec<Stage>, QueryEngineError> {
        let keywords: Vec<&str> = query.split_whitespace().collect();
        let mut matched_names: Vec<String> = Vec::new();

        for member in &doc.members {
            let matches_keyword = |k: &&str| {
                contains_ignore_case(member.name.as_str(), k)
                    || member
                        .comment
                        .as_ref()
                        .is_some_and(|c| contains_ignore_case(c.as_str(), k))
            };

            if keywords.iter().any(matches_keyword) {
                matched_names.push(member.name.as_str().to_string());
            }
        }

        if matched_names.is_empty() {
            return Err(QueryEngineError::NoResults);
        }

        let grounded = crate::grounding::grounded_names(doc, &matched_names);
        if grounded.is_empty() {
            return Err(QueryEngineError::NoResults);
        }
        Ok(Synthesizer::synthesize(query, doc, &grounded))
    }
}

/// Search using LLM relevance scoring.
pub struct ConceptBackend;

impl SearchBackend for ConceptBackend {
    fn name(&self) -> &'static str {
        "backend.concept"
    }
    fn kind(&self) -> &'static str {
        "concept"
    }
    fn matches(&self, intent: QueryIntent) -> bool {
        matches!(intent, QueryIntent::Conceptual | QueryIntent::HowTo)
    }

    fn uses_model(&self) -> bool {
        true
    }

    fn retrieval_tier(&self) -> crate::search_types::RetrievalTier {
        crate::search_types::RetrievalTier::Semantic
    }

    fn search(
        &self,
        query: &str,
        doc: &GuidanceDoc,
        ctx: &SearchContext<'_>,
    ) -> Result<Vec<Stage>, QueryEngineError> {
        let scores = ctx
            .llm_filter
            .filter_candidates(query, doc, 10)
            .map_err(|e| QueryEngineError::LlmFilter(e.to_string()))?;

        let matched_names: Vec<String> = scores
            .into_iter()
            .filter(|s| s.score >= 0.5)
            .map(|s| s.member_name)
            .collect();

        if matched_names.is_empty() {
            return Err(QueryEngineError::NoResults);
        }

        // Task-value axis (taxonomy groundedness) composes as a stable
        // rerank among gate-passers only: membership is decided by the
        // calibrated confidence gate above and never revisited here.
        // `None` store ⇒ byte-identical order (early return, no verdicts).
        let matched_names = match ctx.concept_store {
            Some(store) => super::concept_ground::grounded_first(matched_names, store),
            None => matched_names,
        };

        let grounded = crate::grounding::grounded_names(doc, &matched_names);
        if grounded.is_empty() {
            return Err(QueryEngineError::NoResults);
        }
        Ok(Synthesizer::synthesize(query, doc, &grounded))
    }
}

/// Search by file path matching.
pub struct FilePathBackend;

impl SearchBackend for FilePathBackend {
    fn name(&self) -> &'static str {
        "backend.filepath"
    }
    fn kind(&self) -> &'static str {
        "filepath"
    }
    fn matches(&self, intent: QueryIntent) -> bool {
        matches!(intent, QueryIntent::FilePath)
    }

    fn retrieval_tier(&self) -> crate::search_types::RetrievalTier {
        crate::search_types::RetrievalTier::Exact
    }

    fn search(
        &self,
        query: &str,
        doc: &GuidanceDoc,
        _ctx: &SearchContext<'_>,
    ) -> Result<Vec<Stage>, QueryEngineError> {
        let matched_names: Vec<String> = doc
            .members
            .iter()
            .filter(|m| {
                contains_ignore_case(doc.meta.source.as_str(), query)
                    || contains_ignore_case(m.name.as_str(), query)
            })
            .map(|m| m.name.as_str().to_string())
            .collect();

        let grounded = crate::grounding::grounded_names(doc, &matched_names);
        if grounded.is_empty() {
            return Err(QueryEngineError::NoResults);
        }
        Ok(Synthesizer::synthesize(query, doc, &grounded))
    }
}

/// General keyword search across member names and comments.
pub struct GeneralBackend;

impl SearchBackend for GeneralBackend {
    fn name(&self) -> &'static str {
        "backend.general"
    }
    fn kind(&self) -> &'static str {
        "general"
    }
    fn matches(&self, intent: QueryIntent) -> bool {
        matches!(intent, QueryIntent::GeneralSearch)
    }

    fn search(
        &self,
        query: &str,
        doc: &GuidanceDoc,
        _ctx: &SearchContext<'_>,
    ) -> Result<Vec<Stage>, QueryEngineError> {
        let matched_names: Vec<String> = doc
            .members
            .iter()
            .filter(|m| {
                contains_ignore_case(m.name.as_str(), query)
                    || m.signature
                        .as_ref()
                        .is_some_and(|s| contains_ignore_case(s.as_str(), query))
                    || m.comment
                        .as_ref()
                        .is_some_and(|c| contains_ignore_case(c.as_str(), query))
            })
            .map(|m| m.name.as_str().to_string())
            .collect();

        let grounded = crate::grounding::grounded_names(doc, &matched_names);
        if !grounded.is_empty() {
            return Ok(Synthesizer::synthesize(query, doc, &grounded));
        }

        Err(QueryEngineError::NoResults)
    }
}

// --- Uniform handles: native dispatch, inventory-only Component ---------
//
// Backends are not `Component`s: `search` needs a live `SearchContext`
// that no handoff channel can carry, so `execute` can never run one.
// Each registration therefore pairs the native dispatch view with an
// inventory-only adapter handle carrying the backend's name and schema.
// Dispatch stays native (zero round-trip cost, byte-identical
// arbitration); the handle answers inventory, never execution.

/// Inventory-only `Component` for one registered backend.
///
/// Never executes a search: `execute` reports the native entry point.
/// The orchestrator dispatches through `SearchBackend::search` and reads
/// names and schema through this handle.
struct BackendInventory {
    name: &'static str,
    kind: &'static str,
}

impl WorkUnit for BackendInventory {
    fn name(&self) -> &str {
        self.name
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        Err(WorkError::Execution(format!(
            "{} runs through QueryEngine native dispatch: call SearchBackend::search; \
             this handle is inventory only",
            self.name
        )))
    }
}

impl Describable for BackendInventory {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "kind": "search_backend",
            "backend": self.kind,
            "name": self.name,
        })
    }
}

fluent_wvr::impl_fieldless!(BackendInventory);
fluent_wvr::impl_component!(BackendInventory);

/// One registered backend: native dispatch view + inventory handle.
///
/// Built from any owned `T: SearchBackend`: the orchestrator dispatches
/// through `backend` (ladder, byte-identical) and inventories through
/// `component` (a `ComponentAdapter` whose execute reports native
/// dispatch without pretending to run). Borrowed backends cannot
/// register (uniform handles require `'static`); custom owned backends
/// implement `name`/`kind` alongside `matches`/`search`.
pub struct RegisteredBackend {
    /// Native dispatch (`matches` + `search` through the ladder).
    pub backend: std::sync::Arc<dyn SearchBackend>,
    /// Inventory handle (name, schema, fieldless config surface).
    pub component: std::sync::Arc<dyn Component>,
}

impl RegisteredBackend {
    /// Register one owned backend behind both views.
    pub fn new<T>(backend: T) -> Self
    where
        T: SearchBackend + 'static,
    {
        let shared: std::sync::Arc<T> = std::sync::Arc::new(backend);
        let inventory = BackendInventory {
            name: shared.name(),
            kind: shared.kind(),
        };
        let component: std::sync::Arc<dyn Component> = std::sync::Arc::new(
            fluent_wvr::wrapper::ComponentAdapter::new(
                std::sync::Arc::new(inventory) as std::sync::Arc<dyn Component>
            )
            .with_execute_override(std::sync::Arc::new({
                let name = shared.name();
                move |_ctx: &WorkContext| {
                    Err(WorkError::Execution(format!(
                        "{name} runs through QueryEngine native dispatch: call \
                         SearchBackend::search; this handle is inventory only"
                    )))
                }
            })),
        );
        Self {
            backend: shared,
            component,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::llm_filter::{
        LlmFilter, LlmFilterBackend, LlmFilterError, NoopLlmFilter, RelevanceScore,
    };
    use crate::tests::common::make_test_doc;

    fn ctx_with_filter(filter: &LlmFilter) -> SearchContext<'_> {
        // `llm_filter` is the only field the legacy backends under test
        // read; index seams stay empty on the doc path.
        SearchContext {
            storage: None,
            embedder: None,
            nlp: None,
            llm_filter: filter,
            concept_store: None,
        }
    }

    fn noop_filter() -> LlmFilter {
        LlmFilter::new(Some(Box::new(NoopLlmFilter)))
    }

    #[test]
    fn identifier_backend_matches_intents() {
        assert!(IdentifierBackend.matches(QueryIntent::SingleIdentifier));
        assert!(!IdentifierBackend.matches(QueryIntent::FilePath));
    }

    #[test]
    fn identifier_backend_finds_by_member_name() {
        let doc = make_test_doc();
        let stages = IdentifierBackend
            .search("helloWorld", &doc, &ctx_with_filter(&noop_filter()))
            .expect("search");
        assert!(!stages.is_empty());
    }

    #[test]
    fn identifier_backend_falls_back_to_signature() {
        // `addNumbers` is a member name; query by a signature-only token that
        // is not the member name should still resolve via the signature path.
        let doc = make_test_doc();
        let stages = IdentifierBackend
            .search("helloWorld", &doc, &ctx_with_filter(&noop_filter()))
            .expect("search");
        assert!(!stages.is_empty());
        // An unrelated identifier yields NoResults (no WordIndex in ctx).
        assert!(matches!(
            IdentifierBackend.search("zzz", &doc, &ctx_with_filter(&noop_filter())),
            Err(QueryEngineError::NoResults)
        ));
    }

    #[test]
    fn keyword_backend_matches_and_searches() {
        assert!(KeywordBackend.matches(QueryIntent::CapabilityQuery));
        assert!(KeywordBackend.matches(QueryIntent::MultiKeyword));
        let doc = make_test_doc();
        // "hello world" matches helloWorld's comment ("Prints hello world").
        let stages = KeywordBackend
            .search("hello world", &doc, &ctx_with_filter(&noop_filter()))
            .expect("search");
        assert!(!stages.is_empty());
        assert!(matches!(
            KeywordBackend.search("zzzzz", &doc, &ctx_with_filter(&noop_filter())),
            Err(QueryEngineError::NoResults)
        ));
    }

    struct StubFilter(Vec<RelevanceScore>);
    impl LlmFilterBackend for StubFilter {
        fn score_relevance(
            &self,
            _query: &str,
            _candidates: &[&str],
        ) -> Result<Vec<RelevanceScore>, LlmFilterError> {
            Ok(self.0.clone())
        }
    }

    // M10.3: test stub joins the uniform contract (native-only execute).
    impl WorkUnit for StubFilter {
        fn name(&self) -> &str {
            "filter.stub"
        }
        fn depends(&self) -> &[ArcIntern<str>] {
            &[]
        }
        fn provides(&self) -> &[ArcIntern<str>] {
            &[]
        }
        fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
            Err(WorkError::Execution(
                "filter.stub runs natively: call score_relevance directly".into(),
            ))
        }
    }

    impl Describable for StubFilter {
        fn describe(&self) -> serde_json::Value {
            serde_json::json!({
                "kind": "llm_filter_backend",
                "backend": "stub",
                "name": "filter.stub",
            })
        }
    }

    fluent_wvr::impl_fieldless!(StubFilter);
    fluent_wvr::impl_component!(StubFilter);

    #[test]
    fn concept_backend_matches_and_filters_by_score() {
        assert!(ConceptBackend.matches(QueryIntent::Conceptual));
        assert!(ConceptBackend.matches(QueryIntent::HowTo));
        let filter = LlmFilter::new(Some(Box::new(StubFilter(vec![
            RelevanceScore {
                member_name: "helloWorld".into(),
                score: 0.9,
                reasoning: String::new(),
            },
            RelevanceScore {
                member_name: "addNumbers".into(),
                score: 0.2,
                reasoning: String::new(),
            },
        ]))));
        let stages = ConceptBackend
            .search("add things", &make_test_doc(), &ctx_with_filter(&filter))
            .expect("search");
        // Only the member scoring >= 0.5 survives.
        assert!(!stages.is_empty());
    }

    #[test]
    fn concept_backend_no_results_when_all_below_threshold() {
        let filter = LlmFilter::new(Some(Box::new(StubFilter(vec![RelevanceScore {
            member_name: "helloWorld".into(),
            score: 0.1,
            reasoning: String::new(),
        }]))));
        assert!(matches!(
            ConceptBackend.search("x", &make_test_doc(), &ctx_with_filter(&filter)),
            Err(QueryEngineError::NoResults)
        ));
    }

    #[test]
    fn concept_backend_errors_without_a_client() {
        let filter = LlmFilter::new(None);
        assert!(matches!(
            ConceptBackend.search("x", &make_test_doc(), &ctx_with_filter(&filter)),
            Err(QueryEngineError::LlmFilter(_))
        ));
    }

    #[test]
    fn concept_store_none_context_matches_legacy_output() {
        // Seam default: an explicit `None` store must behave exactly like
        // the pre-seam context (same helper, same filter, same doc).
        let filter = LlmFilter::new(Some(Box::new(StubFilter(vec![RelevanceScore {
            member_name: "helloWorld".into(),
            score: 0.9,
            reasoning: String::new(),
        }]))));
        let doc = make_test_doc();
        let legacy = ConceptBackend
            .search("add things", &doc, &ctx_with_filter(&filter))
            .expect("search");
        let explicit = ConceptBackend
            .search(
                "add things",
                &doc,
                &SearchContext {
                    storage: None,
                    embedder: None,
                    nlp: None,
                    llm_filter: &filter,
                    concept_store: None,
                },
            )
            .expect("search");
        assert_eq!(legacy.len(), explicit.len());
        let legacy_kinds: Vec<String> = legacy.iter().map(|s| format!("{:?}", s.kind)).collect();
        let explicit_kinds: Vec<String> =
            explicit.iter().map(|s| format!("{:?}", s.kind)).collect();
        assert_eq!(legacy_kinds, explicit_kinds);
    }

    fn seeded_concept_store() -> fluent_concept::InMemoryConceptStore {
        // `addNumbers` grounds via `schema:Addnumbers`; `helloWorld` stays
        // unknown — the minimal order-discriminating pair.
        use fluent_concept::ConceptStore;
        let store = fluent_concept::InMemoryConceptStore::new();
        store
            .insert(fluent_types::ConceptMetadata {
                id: fluent_types::InterlinguaId::new(
                    fluent_types::InterlinguaNamespace::YagoClass,
                    7,
                ),
                canonical_name: "schema:Addnumbers".to_string(),
                namespace: fluent_types::InterlinguaNamespace::YagoClass,
                yago_iri: None,
                yago_class_iri: None,
                label: None,
                node_id: None,
                parent_class_id: None,
            })
            .expect("insert");
        store
    }

    fn member_order(stages: &[crate::query::synthesize::Stage]) -> Vec<String> {
        stages
            .iter()
            .filter_map(|s| s.member_name.clone())
            .collect()
    }

    #[test]
    fn concept_backend_reranks_grounded_first_with_store() {
        // Both members pass the 0.5 gate (LLM order: helloWorld first);
        // the store grounds `addNumbers`, which must move first.
        // Membership is untouched — only order changes.
        let filter = LlmFilter::new(Some(Box::new(StubFilter(vec![
            RelevanceScore {
                member_name: "helloWorld".into(),
                score: 0.9,
                reasoning: String::new(),
            },
            RelevanceScore {
                member_name: "addNumbers".into(),
                score: 0.8,
                reasoning: String::new(),
            },
        ]))));
        let doc = make_test_doc();
        let store = seeded_concept_store();
        let ctx = SearchContext {
            storage: None,
            embedder: None,
            nlp: None,
            llm_filter: &filter,
            concept_store: Some(&store),
        };
        let stages = ConceptBackend
            .search("add things", &doc, &ctx)
            .expect("search");
        // Each member emits code + comment stages; order follows the rerank.
        assert_eq!(
            member_order(&stages),
            vec!["addNumbers", "addNumbers", "helloWorld", "helloWorld"]
        );
    }

    #[test]
    fn concept_backend_none_store_preserves_llm_order() {
        // No store ⇒ gate order passes through byte-identically.
        let filter = LlmFilter::new(Some(Box::new(StubFilter(vec![
            RelevanceScore {
                member_name: "helloWorld".into(),
                score: 0.9,
                reasoning: String::new(),
            },
            RelevanceScore {
                member_name: "addNumbers".into(),
                score: 0.8,
                reasoning: String::new(),
            },
        ]))));
        let doc = make_test_doc();
        let stages = ConceptBackend
            .search("add things", &doc, &ctx_with_filter(&filter))
            .expect("search");
        assert_eq!(
            member_order(&stages),
            vec!["helloWorld", "helloWorld", "addNumbers", "addNumbers"]
        );
    }

    #[test]
    fn concept_backend_loading_store_matches_ready_output() {
        // M6 control: store readiness must not leak into output through
        // either signal — grounding reads identically in both states, so a
        // `Loading` store reranks exactly like a `Ready` one.
        use fluent_concept::ConceptStoreState;
        let filter = LlmFilter::new(Some(Box::new(StubFilter(vec![
            RelevanceScore {
                member_name: "helloWorld".into(),
                score: 0.9,
                reasoning: String::new(),
            },
            RelevanceScore {
                member_name: "addNumbers".into(),
                score: 0.8,
                reasoning: String::new(),
            },
        ]))));
        let doc = make_test_doc();
        let ready = seeded_concept_store();
        let loading = seeded_concept_store();
        loading.set_state(ConceptStoreState::Loading);
        let search_with = |store: &fluent_concept::InMemoryConceptStore| {
            let ctx = SearchContext {
                storage: None,
                embedder: None,
                nlp: None,
                llm_filter: &filter,
                concept_store: Some(store),
            };
            ConceptBackend
                .search("add things", &doc, &ctx)
                .expect("search")
        };
        let ready_stages = search_with(&ready);
        let loading_stages = search_with(&loading);
        assert_eq!(member_order(&ready_stages), member_order(&loading_stages));
        assert_eq!(
            member_order(&ready_stages),
            vec!["addNumbers", "addNumbers", "helloWorld", "helloWorld"]
        );
    }

    #[test]
    fn concept_backend_store_preserves_membership() {
        // M6 no-degrade: the store may reorder gate-passers, never change
        // the member set (recall preserved — the rerank is not a filter).
        let filter = LlmFilter::new(Some(Box::new(StubFilter(vec![
            RelevanceScore {
                member_name: "helloWorld".into(),
                score: 0.9,
                reasoning: String::new(),
            },
            RelevanceScore {
                member_name: "addNumbers".into(),
                score: 0.8,
                reasoning: String::new(),
            },
        ]))));
        let doc = make_test_doc();
        let plain = ConceptBackend
            .search("add things", &doc, &ctx_with_filter(&filter))
            .expect("search");
        let store = seeded_concept_store();
        let ctx = SearchContext {
            storage: None,
            embedder: None,
            nlp: None,
            llm_filter: &filter,
            concept_store: Some(&store),
        };
        let ranked = ConceptBackend
            .search("add things", &doc, &ctx)
            .expect("search");
        let mut plain_members = member_order(&plain);
        let mut ranked_members = member_order(&ranked);
        plain_members.sort();
        ranked_members.sort();
        assert_eq!(plain_members, ranked_members);
    }

    #[test]
    fn file_path_backend_matches_and_searches() {
        assert!(FilePathBackend.matches(QueryIntent::FilePath));
        let doc = make_test_doc();
        // Matches on the doc source path.
        let stages = FilePathBackend
            .search("test.zig", &doc, &ctx_with_filter(&noop_filter()))
            .expect("search");
        assert!(!stages.is_empty());
        assert!(matches!(
            FilePathBackend.search("nonexistent", &doc, &ctx_with_filter(&noop_filter())),
            Err(QueryEngineError::NoResults)
        ));
    }

    #[test]
    fn general_backend_matches_and_searches() {
        assert!(GeneralBackend.matches(QueryIntent::GeneralSearch));
        let doc = make_test_doc();
        // Matches via the member comment.
        let stages = GeneralBackend
            .search("hello", &doc, &ctx_with_filter(&noop_filter()))
            .expect("search");
        assert!(!stages.is_empty());
        assert!(matches!(
            GeneralBackend.search("qqqq", &doc, &ctx_with_filter(&noop_filter())),
            Err(QueryEngineError::NoResults)
        ));
    }

    #[test]
    fn test_concept_backend_sweep_passes_gate_at_0_5() {
        // Control group: positives (genuine conceptual queries) should have high scores,
        // negatives (near-miss / single-token / empty) should have low scores.
        // We test the 0.5 threshold operating point via common_core::calibration.
        struct Case {
            score: f64,
            label: bool,
        }
        let cases = vec![
            // Positives — should fire at 0.5
            Case {
                score: 0.9,
                label: true,
            },
            Case {
                score: 0.85,
                label: true,
            },
            Case {
                score: 0.75,
                label: true,
            },
            Case {
                score: 0.60,
                label: true,
            },
            Case {
                score: 0.95,
                label: true,
            },
            Case {
                score: 0.80,
                label: true,
            },
            Case {
                score: 0.70,
                label: true,
            },
            Case {
                score: 0.55,
                label: true,
            },
            // Negatives — must NOT fire at 0.5 (precision guard)
            Case {
                score: 0.40,
                label: false,
            },
            Case {
                score: 0.30,
                label: false,
            },
            Case {
                score: 0.20,
                label: false,
            },
            Case {
                score: 0.10,
                label: false,
            },
            Case {
                score: 0.05,
                label: false,
            },
            Case {
                score: 0.45,
                label: false,
            },
            Case {
                score: 0.35,
                label: false,
            },
            Case {
                score: 0.15,
                label: false,
            },
            Case {
                score: 0.0,
                label: false,
            },
            Case {
                score: 0.25,
                label: false,
            },
        ];
        let thresholds: Vec<f64> = (0..=20).map(|i| f64::from(i) * 0.05).collect();
        let reports = common_core::calibration::sweep_thresholds(
            &cases,
            |c| c.score,
            |c| c.label,
            &thresholds,
        );
        // Emit artifact
        common_core::calibration::emit_markdown_artifact("guidance_concept_backend", &reports);
        // Gate at 0.5
        let report_at_05 =
            common_core::calibration::calibrate_threshold(&cases, |c| c.score, |c| c.label, 0.5);
        assert!(
            report_at_05.passes_gate(),
            "ConceptBackend 0.5 must pass gate: precision {} FPR {} \n{}",
            report_at_05.precision,
            report_at_05.fpr,
            common_core::calibration::render_calibration_table(&reports)
        );
    }

    // --- Uniform-handle contract (Option B: honest handles) ----------------
    //
    // Backends are not `Component`s: `execute` cannot run without a live
    // `SearchContext`, so each registration pairs the native dispatch view
    // with an inventory-only adapter handle. These tests pin that shape.

    use fluent_wvr::{FieldAccess, WorkContext};

    /// A `SearchBackend` with no `Component` impl must still register:
    /// the uniform contract never required executability.
    struct SearchOnlyBackend;

    impl SearchBackend for SearchOnlyBackend {
        fn name(&self) -> &'static str {
            "backend.search-only"
        }
        fn kind(&self) -> &'static str {
            "search-only"
        }
        fn matches(&self, intent: QueryIntent) -> bool {
            matches!(intent, QueryIntent::GeneralSearch)
        }
        fn search(
            &self,
            _query: &str,
            _doc: &GuidanceDoc,
            _ctx: &SearchContext<'_>,
        ) -> Result<Vec<Stage>, QueryEngineError> {
            Err(QueryEngineError::NoResults)
        }
    }

    #[test]
    fn registered_backends_report_names_and_schema() {
        let registrations = [
            RegisteredBackend::new(IdentifierBackend),
            RegisteredBackend::new(SearchOnlyBackend),
        ];
        let names: Vec<String> = registrations
            .iter()
            .map(|r| r.component.name().to_string())
            .collect();
        assert_eq!(names, vec!["backend.identifier", "backend.search-only"]);
        let kinds: Vec<serde_json::Value> = registrations
            .iter()
            .map(|r| fluent_wvr::Describable::describe(r.component.as_ref())["backend"].clone())
            .collect();
        assert_eq!(kinds, vec![serde_json::json!("identifier"), serde_json::json!("search-only")]);
    }

    #[test]
    fn registered_execute_reports_native_dispatch() {
        // No live SearchContext travels the handoff: execute says where
        // to go instead of pretending to run.
        let registration = RegisteredBackend::new(IdentifierBackend);
        let err = registration
            .component
            .execute(&WorkContext::default())
            .expect_err("inventory-only");
        match err {
            fluent_wvr::WorkError::Execution(message) => {
                assert!(message.contains("native dispatch"), "{message}");
            }
            other => panic!("expected Execution, got {other:?}"),
        }
    }

    #[test]
    fn registered_inventory_is_fieldless() {
        let registration = RegisteredBackend::new(KeywordBackend);
        let mut component = registration.component;
        assert_eq!(component.field_names(), &[] as &[&str]);
        assert!(component.get_field("intent").is_err());
        assert!(component.set_field("intent", "x").is_err());
    }

    /// Source guard: no `component_downcast_ref::<*Backend>` may exist in
    /// the guidance tree — native dispatch needs no downcast, and the
    /// router pipeline is the only sanctioned exception (other crate).
    #[test]
    fn no_backend_downcast_in_guidance_tree() {
        fn rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).expect("read_dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    rs_files(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        rs_files(&root, &mut files);
        // Split the needle so this guard's own source never matches it.
        let needle = ["component_downcast_ref", "::<"].concat();
        let mut hits = Vec::new();
        for file in files {
            let content = std::fs::read_to_string(&file).expect("read rs file");
            for (i, line) in content.lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                if code.contains(&needle) && code.contains("Backend") {
                    hits.push(format!("{}:{}: {code}", file.display(), i + 1));
                }
            }
        }
        assert!(
            hits.is_empty(),
            "backend downcasts forbidden in guidance (dispatch natively):\n{}",
            hits.join("\n")
        );
    }
}
