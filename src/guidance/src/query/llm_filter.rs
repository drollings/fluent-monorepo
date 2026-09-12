use common_core::string::contains_ignore_case;
use fluent_types::GuidanceDoc;
use fluent_wvr::{ArcIntern, Describable, WorkContext, WorkError, WorkOutput, WorkUnit};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum LlmFilterError {
    #[error("LLM client error: {0}")]
    Client(String),
    #[error("no LLM client configured")]
    NoClient,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RelevanceScore {
    pub member_name: String,
    pub score: f32,
    pub reasoning: String,
}

/// LLM relevance backend — the fluent-wvr control plane for concept scoring.
///
/// M10.3: uniformity is compiler-enforced — every backend is a
/// `fluent_wvr::Component`. `LlmFilter` holds one backend and scores
/// natively per candidate slice (no dual-view allocation, no handoff
/// round-trip); the bound keeps the single registration uniformly
/// inventoried.
///
/// M14.3: `LlmFilter`/`NoopLlmFilter` stay synchronous and stay here, not
/// on `fluent_llm::protocol::LlmRequestQueue`. The queue is async while
/// `SearchBackend::search` (the only caller, via `ConceptBackend`) is
/// sync — migration would force a `block_on` into the query path or
/// async-ify every search backend — and no real LLM-backed scoring
/// backend exists today (only the keyword `NoopLlmFilter` fail-open
/// default plus test stubs), so there is nothing to route through the
/// queue. The enhancer (the real LLM caller) already composes the
/// canonical async path (`retry_async` + `chat_complete_async`). When a
/// genuine async scoring backend arrives it composes `LlmRequestQueue`
/// with `NoopLlmFilter` kept as the fail-open default; `BatchLlmFilter`
/// batching belongs to M11.2 (`ResultPool`), not this milestone. Stays —
/// pinned by the noop-score goldens and the ConceptBackend 0.5 gate.
pub trait LlmFilterBackend: fluent_wvr::Component {
    fn score_relevance(
        &self,
        query: &str,
        candidates: &[&str],
    ) -> Result<Vec<RelevanceScore>, LlmFilterError>;
}

pub struct LlmFilter {
    backend: Option<Box<dyn LlmFilterBackend>>,
}

impl LlmFilter {
    pub fn new(backend: Option<Box<dyn LlmFilterBackend>>) -> Self {
        Self { backend }
    }

    pub fn filter_candidates(
        &self,
        query: &str,
        doc: &GuidanceDoc,
        max_results: usize,
    ) -> Result<Vec<RelevanceScore>, LlmFilterError> {
        let backend = self.backend.as_ref().ok_or(LlmFilterError::NoClient)?;

        let candidate_names: Vec<&str> = doc
            .members
            .iter()
            .filter_map(|m| {
                m.signature
                    .as_ref()
                    .or(Some(&m.name))
                    .map(smol_str::SmolStr::as_str)
            })
            .take(20)
            .collect();

        if candidate_names.is_empty() {
            return Ok(Vec::new());
        }

        let scores = backend.score_relevance(query, &candidate_names)?;

        let mut scores = scores;
        scores.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scores.truncate(max_results);

        Ok(scores)
    }
}

pub struct NoopLlmFilter;

impl LlmFilterBackend for NoopLlmFilter {
    fn score_relevance(
        &self,
        query: &str,
        candidates: &[&str],
    ) -> Result<Vec<RelevanceScore>, LlmFilterError> {
        Ok(candidates
            .iter()
            .map(|c| {
                let score = if contains_ignore_case(c, query) {
                    0.9
                } else {
                    0.1
                };
                RelevanceScore {
                    member_name: c.to_string(),
                    score,
                    reasoning: "keyword match".into(),
                }
            })
            .collect())
    }
}

// --- M10.2: uniform handle (additive; native scoring untouched) -------------
//
// `NoopLlmFilter` is a `fluent_wvr::Component` so the orchestrator can hold
// an `Arc<dyn Component>` handle. Scoring is pure, so `execute` runs it for
// real through the structured handoff (`ctx.structured["query"]` string +
// `ctx.structured["candidates"]` JSON string array) and returns the typed
// scores.

impl WorkUnit for NoopLlmFilter {
    fn name(&self) -> &str {
        "filter.noop"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        let query = ctx
            .structured
            .get("query")
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                WorkError::Execution("filter execute needs ctx.structured[\"query\"]".into())
            })?;
        let candidates_value = ctx.structured.get("candidates").ok_or_else(|| {
            WorkError::Execution("filter execute needs ctx.structured[\"candidates\"]".into())
        })?;
        let candidates: Vec<String> =
            serde_json::from_value(candidates_value.clone()).map_err(|e| {
                WorkError::Execution(format!("filter execute: candidates decode: {e}"))
            })?;
        let refs: Vec<&str> = candidates.iter().map(String::as_str).collect();
        let scores = self
            .score_relevance(query, &refs)
            .map_err(|e| WorkError::Execution(format!("filter execute: score: {e}")))?;
        WorkOutput::typed("noop scores", &scores)
            .map_err(|e| WorkError::Execution(format!("filter execute: encode: {e}")))
    }
}

impl Describable for NoopLlmFilter {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "kind": "llm_filter_backend",
            "backend": "noop",
            "name": "filter.noop",
        })
    }
}

fluent_wvr::impl_fieldless!(NoopLlmFilter);
fluent_wvr::impl_component!(NoopLlmFilter);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::common::make_test_doc;
    use fluent_types::{GuidanceDoc, Member, MemberType, Meta};

    #[test]
    fn test_noop_filter_basic() {
        let filter = LlmFilter::new(Some(Box::new(NoopLlmFilter)));

        let doc = GuidanceDoc {
            meta: Meta {
                source: "test.zig".into(),
                ..make_test_doc().meta
            },
            members: vec![
                Member {
                    type_name: MemberType::FnDecl,
                    name: "hello".into(),
                    signature: Some("fn hello() void".into()),
                    ..Member::default()
                },
                Member {
                    type_name: MemberType::FnDecl,
                    name: "greet".into(),
                    signature: Some("fn greet(name: []const u8) void".into()),
                    ..Member::default()
                },
            ],
            ..make_test_doc()
        };

        let results = filter.filter_candidates("hello", &doc, 5).expect("filter");
        assert!(!results.is_empty());
    }

    #[test]
    fn test_filter_no_client_error() {
        let filter = LlmFilter::new(None);
        let doc = GuidanceDoc::default();
        let result = filter.filter_candidates("test", &doc, 5);
        assert!(result.is_err());
    }

    // --- M10.1 characterization: noop scores byte-exact ---------------------

    #[test]
    fn m10_noop_filter_scores_substring_hit_above_miss() {
        // 0.9 substring hit / 0.1 miss, relevance-descending, reasoning
        // pinned: M10.3's filter-handle switch must preserve the 0.5
        // operating point the ConceptBackend gates on.
        let filter = LlmFilter::new(Some(Box::new(NoopLlmFilter)));
        let doc = GuidanceDoc {
            meta: Meta {
                source: "test.zig".into(),
                ..make_test_doc().meta
            },
            members: vec![
                Member {
                    type_name: MemberType::FnDecl,
                    name: "hello".into(),
                    signature: Some("fn hello() void".into()),
                    ..Member::default()
                },
                Member {
                    type_name: MemberType::FnDecl,
                    name: "greet".into(),
                    signature: Some("fn greet(name: []const u8) void".into()),
                    ..Member::default()
                },
            ],
            ..make_test_doc()
        };
        let results = filter.filter_candidates("hello", &doc, 5).expect("filter");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].member_name, "fn hello() void");
        assert_eq!(results[0].score.to_bits(), 0.9f32.to_bits());
        assert_eq!(results[0].reasoning, "keyword match");
        assert_eq!(results[1].member_name, "fn greet(name: []const u8) void");
        assert_eq!(results[1].score.to_bits(), 0.1f32.to_bits());
        // Truncation honors max_results after sorting.
        let top = filter.filter_candidates("hello", &doc, 1).expect("filter");
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].member_name, "fn hello() void");
    }

    // --- M10.2: uniform-handle contract ------------------------------------

    use fluent_wvr::{Component, FieldAccess, WorkContext};

    #[test]
    fn m10_noop_filter_handle_executes_native_scores() {
        let filter = NoopLlmFilter;
        let handle: std::sync::Arc<dyn Component> = std::sync::Arc::new(NoopLlmFilter);
        assert_eq!(handle.name(), "filter.noop");
        let mut ctx = WorkContext::default();
        ctx.structured.insert(
            "query".to_string(),
            serde_json::Value::String("hello".to_string()),
        );
        ctx.structured.insert(
            "candidates".to_string(),
            serde_json::json!(["fn hello() void", "fn greet() void"]),
        );
        let out = handle.execute(&ctx).expect("execute");
        let scores: Vec<RelevanceScore> = out.data_as().expect("typed scores");
        assert_eq!(scores.len(), 2);
        assert_eq!(scores[0].member_name, "fn hello() void");
        assert_eq!(scores[0].score.to_bits(), 0.9f32.to_bits());
        assert_eq!(scores[1].score.to_bits(), 0.1f32.to_bits());
        // Byte-identical to the native call.
        let native = filter
            .score_relevance("hello", &["fn hello() void", "fn greet() void"])
            .expect("native");
        assert_eq!(
            serde_json::to_value(&scores).expect("encode"),
            serde_json::to_value(&native).expect("encode")
        );
    }

    #[test]
    fn m10_noop_filter_execute_needs_handoff() {
        let handle: std::sync::Arc<dyn Component> = std::sync::Arc::new(NoopLlmFilter);
        let err = handle.execute(&WorkContext::default()).expect_err("missing query");
        assert!(matches!(err, fluent_wvr::WorkError::Execution(_)));
    }

    #[test]
    fn m10_noop_filter_handle_is_fieldless_with_schema() {
        let mut handle = NoopLlmFilter;
        assert_eq!(handle.field_names(), &[] as &[&str]);
        assert!(handle.get_field("backend").is_err());
        assert!(handle.set_field("backend", "noop").is_err());
        assert_eq!(
            fluent_wvr::Describable::describe(&handle)["backend"],
            serde_json::json!("noop")
        );
    }
}
