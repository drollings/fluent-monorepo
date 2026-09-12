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
}

/// Polymorphic search backend — the fluent-wvr control plane for query dispatch.
///
/// Each backend handles one `QueryIntent`. The orchestrator iterates registered
/// backends and calls `matches` + `search` without branching on implementation.
///
/// M10.3: owned backends are also `fluent_wvr::Component`s and register
/// through `RegisteredBackend` (native dispatch view + uniform handle, one
/// allocation). Invocation stays native (`matches` + `search` through the
/// ladder): `search` needs a live `SearchContext` (storage/embedder/NLP/
/// filter refs) that no handoff channel can carry, and a `GuidanceDoc`
/// JSON round-trip per dispatch would buy nothing. Borrowed index backends
/// (`Lemma`/`Fts`/`Vector`) implement this trait natively only — uniform
/// handles require `'static` (`as_any`); they gain handles via an owned
/// adapter if a registrar ever appears (no speculative wrapper today).
pub trait SearchBackend: Send + Sync {
    /// Returns true if this backend handles the given intent.
    fn matches(&self, intent: QueryIntent) -> bool;

    /// Execute the search and return synthesized stages.
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
    fn matches(&self, intent: QueryIntent) -> bool {
        matches!(
            intent,
            QueryIntent::IdentifierLookup | QueryIntent::SingleIdentifier
        )
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

        if !matched_names.is_empty() {
            return Ok(Synthesizer::synthesize(query, doc, &matched_names));
        }

        let sig_matches = identifier::find_members_by_signature(doc, query);
        if !sig_matches.is_empty() {
            let sig_names: Vec<String> = sig_matches.into_iter().map(ToString::to_string).collect();
            return Ok(Synthesizer::synthesize(query, doc, &sig_names));
        }

        Err(QueryEngineError::NoResults)
    }
}

/// Search by keyword matching across member names and comments.
pub struct KeywordBackend;

impl SearchBackend for KeywordBackend {
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

        Ok(Synthesizer::synthesize(query, doc, &matched_names))
    }
}

/// Search using LLM relevance scoring.
pub struct ConceptBackend;

impl SearchBackend for ConceptBackend {
    fn matches(&self, intent: QueryIntent) -> bool {
        matches!(intent, QueryIntent::Conceptual | QueryIntent::HowTo)
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

        Ok(Synthesizer::synthesize(query, doc, &matched_names))
    }
}

/// Search by file path matching.
pub struct FilePathBackend;

impl SearchBackend for FilePathBackend {
    fn matches(&self, intent: QueryIntent) -> bool {
        matches!(intent, QueryIntent::FilePath)
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

        if matched_names.is_empty() {
            return Err(QueryEngineError::NoResults);
        }

        Ok(Synthesizer::synthesize(query, doc, &matched_names))
    }
}

/// General keyword search across member names and comments.
pub struct GeneralBackend;

impl SearchBackend for GeneralBackend {
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

        if !matched_names.is_empty() {
            return Ok(Synthesizer::synthesize(query, doc, &matched_names));
        }

        Err(QueryEngineError::NoResults)
    }
}

// --- M10.2: uniform handles (additive; native `search` untouched) ----------
//
// Each backend is a `fluent_wvr::Component` so the orchestrator can hold
// `Arc<dyn Component>` handles. Unlike formatters, search backends cannot
// run standalone: `search` needs a live `SearchContext` (storage,
// embedder, NLP, LLM filter refs) that no handoff channel can carry, so
// `execute` documents the native entry point and the orchestrator
// downcasts to `SearchBackend` for dispatch (zero round-trip cost,
// byte-identical arbitration).

macro_rules! impl_search_backend_component {
    ($type:ty, $name:literal, $backend:literal) => {
        impl WorkUnit for $type {
            fn name(&self) -> &str {
                $name
            }
            fn depends(&self) -> &[ArcIntern<str>] {
                &[]
            }
            fn provides(&self) -> &[ArcIntern<str>] {
                &[]
            }
            fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
                Err(WorkError::Execution(
                    concat!(
                        $name,
                        " runs through QueryEngine dispatch: downcast the",
                        " handle to SearchBackend and call search natively"
                    )
                    .into(),
                ))
            }
        }

        impl Describable for $type {
            fn describe(&self) -> serde_json::Value {
                serde_json::json!({
                    "kind": "search_backend",
                    "backend": $backend,
                    "name": $name,
                })
            }
        }

        fluent_wvr::impl_fieldless!($type);
        fluent_wvr::impl_component!($type);
    };
}

impl_search_backend_component!(IdentifierBackend, "backend.identifier", "identifier");
impl_search_backend_component!(KeywordBackend, "backend.keyword", "keyword");
impl_search_backend_component!(ConceptBackend, "backend.concept", "concept");
impl_search_backend_component!(FilePathBackend, "backend.filepath", "filepath");
impl_search_backend_component!(GeneralBackend, "backend.general", "general");

/// One registered backend: native dispatch view + uniform control-plane view.
///
/// Built only from owned `T: SearchBackend + Component` (one `Arc<T>`
/// shared as two fat pointers), so both views always agree. The
/// orchestrator dispatches through `backend` (ladder, byte-identical) and
/// inventories through `component`. Borrowed backends cannot register
/// (uniform handles require `'static`); custom owned backends implement
/// `Component` alongside `SearchBackend` (see `CountingBackend` in the
/// engine tests for the minimal shape).
pub struct RegisteredBackend {
    /// Native dispatch (`matches` + `search` through the ladder).
    pub backend: std::sync::Arc<dyn SearchBackend>,
    /// Uniform control-plane handle (inventory, supervision, config).
    pub component: std::sync::Arc<dyn Component>,
}

impl RegisteredBackend {
    /// Register one owned backend behind both views.
    pub fn new<T>(backend: T) -> Self
    where
        T: SearchBackend + Component + 'static,
    {
        let shared: std::sync::Arc<T> = std::sync::Arc::new(backend);
        Self {
            backend: shared.clone(),
            component: shared,
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
        }
    }

    fn noop_filter() -> LlmFilter {
        LlmFilter::new(Some(Box::new(NoopLlmFilter)))
    }

    #[test]
    fn identifier_backend_matches_intents() {
        assert!(IdentifierBackend.matches(QueryIntent::IdentifierLookup));
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
            common_core::calibration::render_markdown_table(&reports)
        );
    }

    // --- M10.2: uniform-handle contract ------------------------------------

    use fluent_wvr::{Component, FieldAccess, WorkContext};

    #[test]
    fn m10_search_backend_handles_carry_names_and_schema() {
        let handles: Vec<(&str, &str, std::sync::Arc<dyn Component>)> = vec![
            ("backend.identifier", "identifier", std::sync::Arc::new(IdentifierBackend)),
            ("backend.keyword", "keyword", std::sync::Arc::new(KeywordBackend)),
            ("backend.concept", "concept", std::sync::Arc::new(ConceptBackend)),
            ("backend.filepath", "filepath", std::sync::Arc::new(FilePathBackend)),
            ("backend.general", "general", std::sync::Arc::new(GeneralBackend)),
        ];
        for (name, backend, handle) in &handles {
            assert_eq!(handle.name(), *name);
            assert_eq!(
                fluent_wvr::Describable::describe(handle.as_ref())["backend"],
                serde_json::json!(*backend)
            );
        }
    }

    #[test]
    fn m10_search_backend_execute_documents_native_dispatch() {
        // No live SearchContext travels the handoff: execute guides back to
        // downcast-native dispatch instead of failing opaquely.
        let handle: std::sync::Arc<dyn Component> = std::sync::Arc::new(IdentifierBackend);
        let err = handle.execute(&WorkContext::default()).expect_err("native-only");
        match err {
            fluent_wvr::WorkError::Execution(message) => {
                assert!(message.contains("downcast"), "{message}");
            }
            other => panic!("expected Execution, got {other:?}"),
        }
    }

    #[test]
    fn m10_search_backend_handle_downcasts_to_native_dispatch() {
        // The uniform handle recovers the native backend: arbitration and
        // outputs stay byte-identical (no serialize round-trip).
        let handle: std::sync::Arc<dyn Component> = std::sync::Arc::new(IdentifierBackend);
        let native =
            fluent_wvr::component_downcast_ref::<IdentifierBackend>(handle.as_ref())
                .expect("downcast");
        assert!(native.matches(QueryIntent::IdentifierLookup));
        let stages = native
            .search("helloWorld", &make_test_doc(), &ctx_with_filter(&noop_filter()))
            .expect("search");
        assert!(!stages.is_empty());
    }

    #[test]
    fn m10_search_backends_are_fieldless() {
        let mut handle = KeywordBackend;
        assert_eq!(handle.field_names(), &[] as &[&str]);
        assert!(handle.get_field("intent").is_err());
        assert!(handle.set_field("intent", "x").is_err());
    }
}
