//! Chart selection — deterministic capability match → HNSW retrieval →
//! adjudication (Needle tool-pick or LLM).
//!
//! Given a raw request, the selector picks the best chart, cheapest step
//! first (VISION: deterministic before probabilistic):
//!
//! 1. **Deterministic capability match** — if the request already names a
//!    chart or one of its provides assets, select it with `score = 1.0`.
//!    No model call — a request naming a chart/asset never reaches Needle.
//! 2. **HNSW retrieval** — embed the *raw user request* (never a
//!    classifier-authored summary, review R1) and query the `workflow_library`
//!    index built at boot in `ChartStore`; candidates below `cfg.min_score`
//!    are dropped.
//! 3. **Adjudication** — pick the single best chart from the candidate list.
//!    The cheapest adjudicator wins: a `NeedleBackend` tool-pick
//!    (non-generative, grammar-constrained — one candidate per tool) when one
//!    is configured, else one `ChatBackend` call over the candidate list. The
//!    adjudicator decides *which* chart; the deterministic entity binding
//!    decides *whether it is executable* (`Exact` vs `Partial` with interview
//!    gaps).
//!
//! Everything here is pure data-in / data-out — no orchestration state.

use std::fmt::Write as _;
use std::sync::Arc;

use common_core::contains_ident_word;
use fluent_llm::client::ChatBackend;
use fluent_llm::ChatMessage;

use crate::charts::binding::{bind_chart, AmbiguousDep, Bindings, Entity};
use crate::charts::store::ChartStore;
use crate::charts::{ChartDef, ChartError};
use crate::config::ChartsConfig;
use crate::needle::backend::NeedleBackend;
use crate::needle::envelope::NeedleEnvelope;

/// How well a request fits a chart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChartFit {
    /// The chart is fully bound by the provided entities — executable now.
    Exact,
    /// The chart is the best fit but some required inputs are missing. The
    /// gap names drive the interview loop.
    Partial { gaps: Vec<String> },
    /// No chart fits the request — fall through to fresh planning.
    Mismatch,
}

/// The result of selecting a chart for a request.
#[derive(Debug, Clone)]
pub struct ChartMatch {
    /// Selected chart name. Empty for `ChartFit::Mismatch`.
    pub chart: String,
    /// Selection confidence in `[0, 1]`: 1.0 for a deterministic match, else
    /// the top HNSW cosine similarity.
    pub score: f64,
    /// Executability of the selected chart given the bound entities.
    pub fit: ChartFit,
    /// Deterministic binding of the selected chart's deps against the
    /// entities (`None` only for a mismatch).
    pub bindings: Option<Bindings>,
}

impl ChartMatch {
    /// The "no chart fits" sentinel.
    fn mismatch() -> Self {
        Self {
            chart: String::new(),
            score: 0.0,
            fit: ChartFit::Mismatch,
            bindings: None,
        }
    }
}

/// Three-step chart selector. Cheap steps always run first and short-circuit.
pub struct ChartSelector {
    store: Arc<ChartStore>,
    /// Adjudicator backend (mock-injectable). `None` skips the LLM step and
    /// falls back to the top HNSW candidate.
    client: Option<Arc<dyn ChatBackend>>,
    /// Needle adjudicator backend (mock-injectable). When set, Step 3 uses
    /// Needle (a non-generative tool-pick over the HNSW shortlist) instead of
    /// the LLM `client`. `None` keeps the LLM adjudicator (or no adjudicator).
    needle_adjudicator: Option<Arc<dyn NeedleBackend>>,
    /// Reranker backend (mock-injectable). `None` skips the rerank step.
    reranker: Option<Arc<dyn ChatBackend>>,
    cfg: ChartsConfig,
}

impl ChartSelector {
    /// Build a selector over the boot-loaded chart store.
    pub fn new(
        store: Arc<ChartStore>,
        client: Option<Arc<dyn ChatBackend>>,
        cfg: ChartsConfig,
    ) -> Self {
        Self {
            store,
            client,
            needle_adjudicator: None,
            reranker: None,
            cfg,
        }
    }

    /// Attach a Needle adjudicator backend for Step 3. When set, the HNSW
    /// shortlist is adjudicated by a Needle tool-pick (cheapest, deterministic
    /// grammar) instead of the LLM. A decline (`refuse`/`text`/multi-call /
    /// unknown candidate) is honored as a chart `Mismatch`; a backend error
    /// degrades to the top HNSW candidate.
    #[must_use]
    pub fn with_needle_adjudicator(mut self, backend: Arc<dyn NeedleBackend>) -> Self {
        self.needle_adjudicator = Some(backend);
        self
    }

    /// Attach a reranker backend for Step 2.5 (candidate re-ranking).
    #[must_use]
    pub fn with_reranker(mut self, reranker: Arc<dyn ChatBackend>) -> Self {
        self.reranker = Some(reranker);
        self
    }

    /// Select the best chart for `request`, or `ChartFit::Mismatch`.
    ///
    /// `entities` are the bound context entities used to decide whether a
    /// selected chart is `Exact` (executable) or `Partial` (needs interview).
    pub fn select(&self, request: &str, entities: &[Entity]) -> Result<ChartMatch, ChartError> {
        // Step 1: deterministic capability match — cheapest, no LLM.
        if let Some(chart) = self.deterministic_hit(request) {
            tracing::debug!(
                target: "router.charts.select",
                chart = %chart.name,
                "deterministic capability match"
            );
            return Ok(self.build_match(&chart, 1.0, entities, &[]));
        }

        // Step 2: HNSW retrieval over the raw request.
        let candidates = self.retrieve(request)?;
        if candidates.is_empty() {
            return Ok(ChartMatch::mismatch());
        }

        // Step 2.5: LLM re-ranking of the HNSW candidates (cheap before
        // expensive — a cross-encoder reranker narrows what the adjudicator
        // must judge). Failure degrades to the HNSW order.
        let candidates = self.rerank(request, candidates);

        // Step 3: adjudicate the HNSW shortlist — Needle (cheapest,
        // non-generative tool-pick) when configured, else the LLM
        // adjudicator. Both honor an explicit "no candidate fits" (Mismatch →
        // fresh planning); an error or a missing backend degrades to the top
        // HNSW candidate. The deterministic capability pre-match (step 1)
        // already returned for any request naming a chart/asset, so this only
        // ever sees retrieval-sourced candidates.
        match self.adjudication(request, &candidates) {
            Adjudication::Pick { name, gaps } => {
                let score = candidates
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map_or(0.0, |(_, s)| *s);
                let Some(chart) = self.store.get(&name) else {
                    return Ok(ChartMatch::mismatch());
                };
                return Ok(self.build_match(&chart, score, entities, &gaps));
            }
            Adjudication::Mismatch => {
                // The adjudicator judged no candidate fits — honor that.
                return Ok(ChartMatch::mismatch());
            }
            Adjudication::Unavailable => {
                tracing::warn!(
                    target: "router.charts.select",
                    "adjudicator unavailable or failed — using top HNSW candidate"
                );
            }
        }

        // No adjudicator (or it failed): pick the top candidate deterministically.
        match self.store.get(&candidates[0].0) {
            Some(chart) => Ok(self.build_match(&chart, candidates[0].1, entities, &[])),
            None => Ok(ChartMatch::mismatch()),
        }
    }

    /// Step 1: a request that names a chart or one of its provides assets.
    ///
    /// Iterates in a stable (name-sorted) order so selection is reproducible.
    /// Name matches take priority over provides-asset matches; both are
    /// whole-identifier matches (`contains_ident_word` treats `_` as a
    /// boundary, so `bug_triage` is nameable as `bug_triage`).
    fn deterministic_hit(&self, request: &str) -> Option<Arc<ChartDef>> {
        let sorted = self.store.charts_sorted();
        for chart in &sorted {
            if contains_ident_word(request, &chart.name) {
                return Some(chart.clone());
            }
        }
        sorted
            .iter()
            .find(|chart| {
                chart
                    .targets
                    .iter()
                    .any(|t| t.provides.iter().any(|p| contains_ident_word(request, p)))
            })
            .cloned()
    }

    /// Step 2: top-k charts by similarity to the embedded raw request,
    /// filtered by `cfg.min_score`.
    fn retrieve(&self, request: &str) -> Result<Vec<(String, f64)>, ChartError> {
        if !self.store.is_index_built() {
            return Ok(Vec::new());
        }
        let k = self.cfg.max_candidates;
        let hits = self.store.search(request, k)?;
        Ok(hits
            .into_iter()
            .filter(|(_, s)| f64::from(*s) >= self.cfg.min_score)
            .map(|(n, s)| (n, f64::from(s)))
            .collect())
    }

    /// Step 2.5: one LLM call that re-orders the HNSW candidates by relevance
    /// to the raw request. Cross-encoder rerankers are cheap relative to
    /// full adjudication, so this runs before Step 3 (VISION: cheap before
    /// expensive).
    ///
    /// Returns the re-ordered candidate list carrying *original* HNSW scores
    /// (the reranker only re-ranks; it cannot fabricate a chart outside the
    /// candidate set). On a missing / failed / unparseable / invalid rerank
    /// call the HNSW order is preserved (Step 3 still adjudicates correctly).
    fn rerank(&self, request: &str, candidates: Vec<(String, f64)>) -> Vec<(String, f64)> {
        let Some(client) = self.reranker.as_ref() else {
            return candidates;
        };
        let prompt = build_rerank_prompt(request, &candidates, &self.store);
        let messages = vec![
            ChatMessage {
                role: "system".into(),
                content: prompt.clone(),
            },
            ChatMessage {
                role: "user".into(),
                content: request.to_string(),
            },
        ];
        tracing::debug!(
            target: "router.charts.select",
            candidates = candidates.len(),
            prompt_len = prompt.len(),
            "reranker LLM request"
        );
        let Ok(response) = client.chat_complete(&messages) else {
            tracing::warn!(
                target: "router.charts.select",
                "reranker call failed — using HNSW candidate order"
            );
            return candidates;
        };
        let Some(names) = parse_rerank_output(&response) else {
            tracing::warn!(
                target: "router.charts.select",
                "reranker response was not parseable JSON — using HNSW candidate order"
            );
            return candidates;
        };
        let score_of = |name: &str| candidates.iter().find(|(n, _)| n == name).map(|(_, s)| *s);
        let mut reordered: Vec<(String, f64)> = Vec::with_capacity(names.len());
        for name in names {
            if let Some(score) = score_of(&name) {
                reordered.push((name, score));
            } else {
                tracing::warn!(
                    target: "router.charts.select",
                    chart = name,
                    "reranker named a chart outside the candidate list"
                );
            }
        }
        // Preserve any candidate the reranker omitted by appending the
        // un-named HNSW candidates back in original order, so the adjudicator
        // still sees the full candidate set.
        for (name, score) in &candidates {
            if !reordered.iter().any(|(n, _)| n == name) {
                reordered.push((name.clone(), *score));
            }
        }
        if reordered.is_empty() {
            candidates
        } else {
            reordered
        }
    }

    /// Step 3: one LLM call over the candidate list → the single best chart
    /// name (plus the LLM's flagged gaps) or `None` (mismatch). The returned
    /// name is validated against the candidate list so a hallucinated chart is
    /// treated as a mismatch.
    fn adjudicate(
        &self,
        client: &Arc<dyn ChatBackend>,
        request: &str,
        candidates: &[(String, f64)],
    ) -> Result<Option<(String, Vec<String>)>, ChartError> {
        let prompt = build_adjudicator_prompt(request, candidates, &self.store);
        let messages = vec![
            ChatMessage {
                role: "system".into(),
                content: prompt.clone(),
            },
            ChatMessage {
                role: "user".into(),
                content: request.to_string(),
            },
        ];
        tracing::debug!(
            target: "router.charts.select",
            candidates = candidates.len(),
            prompt_len = prompt.len(),
            "adjudicator LLM request"
        );
        let response = client
            .chat_complete(&messages)
            .map_err(|e| ChartError::Selection {
                reason: format!("adjudicator call failed: {e}"),
            })?;
        let Some(out) = parse_adjudicator_output(&response) else {
            tracing::warn!(
                target: "router.charts.select",
                "adjudicator response was not parseable JSON"
            );
            return Ok(None);
        };
        if matches!(out.fit, AdjudicatorFit::Mismatch) {
            return Ok(None);
        }
        let Some(name) = out.chart.as_deref() else {
            return Ok(None);
        };
        if !candidates.iter().any(|(n, _)| n == name) {
            tracing::warn!(
                target: "router.charts.select",
                chart = name,
                "adjudicator named a chart outside the candidate list"
            );
            return Ok(None);
        }
        Ok(Some((name.to_string(), out.gaps)))
    }

    /// Step 3 dispatch: Needle adjudicator when configured, else the LLM
    /// adjudicator, else "unavailable". `Adjudication::Mismatch` means the
    /// adjudicator explicitly judged no candidate fits (honored → fresh
    /// planning); `Adjudication::Unavailable` means no backend or a backend
    /// error (degrade to the top HNSW candidate).
    fn adjudication(
        &self,
        request: &str,
        candidates: &[(String, f64)],
    ) -> Adjudication {
        if let Some(needle) = &self.needle_adjudicator {
            return match self.adjudicate_with_needle(needle, request, candidates) {
                Ok(Some((name, gaps))) => Adjudication::Pick { name, gaps },
                Ok(None) => {
                    tracing::warn!(
                        target: "router.charts.select",
                        "needle adjudicator declined — treating as chart mismatch"
                    );
                    Adjudication::Mismatch
                }
                Err(e) => {
                    tracing::warn!(
                        target: "router.charts.select",
                        error = %e,
                        "needle adjudicator call failed — using top HNSW candidate"
                    );
                    Adjudication::Unavailable
                }
            };
        }
        if let Some(client) = &self.client {
            return match self.adjudicate(client, request, candidates) {
                Ok(Some((name, gaps))) => Adjudication::Pick { name, gaps },
                Ok(None) => Adjudication::Mismatch,
                Err(e) => {
                    tracing::warn!(
                        target: "router.charts.select",
                        error = %e,
                        "adjudicator call failed — using top HNSW candidate"
                    );
                    Adjudication::Unavailable
                }
            };
        }
        Adjudication::Unavailable
    }

    /// Needle adjudication (Step 3, Needle path): one grammar-constrained
    /// tool-pick over the HNSW shortlist. Each candidate chart becomes a tool
    /// (name + description + provides); the single emitted tool call is the
    /// selected chart. A decline (`refuse`/`text`/multi-call/unknown) returns
    /// `Ok(None)` — the caller honors it as a chart `Mismatch` (fresh
    /// planning), mirroring the LLM adjudicator's explicit-no-fit contract.
    fn adjudicate_with_needle(
        &self,
        backend: &Arc<dyn NeedleBackend>,
        request: &str,
        candidates: &[(String, f64)],
    ) -> Result<Option<(String, Vec<String>)>, ChartError> {
        let tools_json = render_candidate_tools(candidates, &self.store);
        tracing::debug!(
            target: "router.charts.select",
            candidates = candidates.len(),
            tools_len = tools_json.len(),
            "needle chart adjudicator request",
        );
        let envelope = backend
            .complete(request, &tools_json, crate::stages::needle::MAX_NEW_TOKENS)
            .map_err(|e| ChartError::Selection {
                reason: format!("needle adjudicator call failed: {e}"),
            })?;
        let name = needle_pick_from_envelope(&envelope, candidates);
        Ok(name.map(|n| (n, Vec::new())))
    }

    /// Build a `ChartMatch` for a chosen chart, resolving any ambiguous
    /// deps first (LLM pick with a deterministic tie-break fallback).
    /// The fit is the union of the deterministic binding's gaps (unmatched
    /// required deps) and the adjudicator's flagged gaps. The binding is the
    /// authority on executability; the LLM's gaps capture semantic
    /// incompleteness the binding cannot see (interview material).
    fn build_match(
        &self,
        chart: &ChartDef,
        score: f64,
        entities: &[Entity],
        llm_gaps: &[String],
    ) -> ChartMatch {
        let bindings = self.resolve_ambiguity(&bind_chart(chart, entities));
        let mut gaps = bindings.unmatched.clone();
        gaps.extend(llm_gaps.iter().cloned());
        gaps.sort_unstable();
        gaps.dedup();
        let fit = if gaps.is_empty() {
            ChartFit::Exact
        } else {
            ChartFit::Partial { gaps }
        };
        ChartMatch {
            chart: chart.name.clone(),
            score,
            fit,
            bindings: Some(bindings),
        }
    }

    /// Resolve every ambiguous dep to a single bound entity.
    ///
    /// For each `AmbiguousDep`, one LLM call picks a candidate (validated
    /// against the candidate list). A failed/unparseable call — or no LLM
    /// backend at all — falls back to a deterministic tie-break (first, then
    /// lexicographic by id). Other deps' bindings are left untouched, so
    /// resolving one dep never disturbs another's satisfied assets.
    fn resolve_ambiguity(&self, bindings: &Bindings) -> Bindings {
        if bindings.ambiguous.is_empty() {
            return bindings.clone();
        }
        let mut resolved = bindings.clone();
        for amb in &bindings.ambiguous {
            let pick = self
                .pick_ambiguous_candidate(amb)
                .unwrap_or_else(|| deterministic_pick(&amb.candidates));
            resolved
                .satisfied
                .insert(crate::charts::binding::asset_key(&pick));
            resolved
                .entity_map
                .entry(amb.dep.clone())
                .or_default()
                .push(pick);
        }
        resolved.ambiguous.clear();
        resolved
    }

    /// One LLM call over an ambiguous dep's candidate list → the chosen
    /// entity, or `None` when the pick is unparseable / invalid (caller
    /// falls back to the deterministic tie-break).
    fn pick_ambiguous_candidate(&self, amb: &AmbiguousDep) -> Option<Entity> {
        let client = self.client.as_ref()?;
        let prompt = build_ambiguity_prompt(amb);
        let messages = vec![
            ChatMessage {
                role: "system".into(),
                content: prompt.clone(),
            },
            ChatMessage {
                role: "user".into(),
                content: format!("dep: {}\n", amb.dep),
            },
        ];
        tracing::debug!(
            target: "router.charts.select",
            dep = %amb.dep,
            candidates = amb.candidates.len(),
            prompt_len = prompt.len(),
            "ambiguity adjudicator LLM request"
        );
        let response = client.chat_complete(&messages).ok()?;
        let entity_id = parse_ambiguity_output(&response)?;
        let picked = amb.candidates.iter().find(|e| e.id == entity_id).cloned();
        if picked.is_none() {
            tracing::warn!(
                target: "router.charts.select",
                dep = %amb.dep,
                entity_id = %entity_id,
                "ambiguity adjudicator named a non-candidate — deterministic fallback"
            );
        }
        picked
    }
}

/// The Step-3 adjudication outcome, shared by the Needle and LLM backends so
/// `select` handles both uniformly.
enum Adjudication {
    /// A candidate chart was picked (name + adjudicator-flagged gaps).
    Pick { name: String, gaps: Vec<String> },
    /// The adjudicator explicitly judged no candidate fits (honored).
    Mismatch,
    /// No adjudicator, or the adjudicator call failed (degrade to top HNSW).
    Unavailable,
}

/// One tool schema per HNSW candidate chart — the Needle grammar's pick set.
/// Mirrors the adjudicator prompt's candidate list so the LLM and Needle
/// adjudicators agree on exactly what is pickable.
fn render_candidate_tools(candidates: &[(String, f64)], store: &ChartStore) -> String {
    let tools: Vec<serde_json::Value> = candidates
        .iter()
        .filter_map(|(name, _)| {
            let chart = store.get(name)?;
            Some(serde_json::json!({
                "name": name,
                "description": chart.description,
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "required": [],
                },
            }))
        })
        .collect();
    serde_json::to_string(&tools).unwrap_or_else(|_| "[]".into())
}

/// Adapt a Needle envelope to a selected candidate chart name.
///
/// A `refuse`/`text` envelope, a multi-tool call, or a tool naming no
/// candidate chart is a decline — `None` honors it as a chart `Mismatch`
/// (never a silent default pick). This mirrors the LLM adjudicator's
/// validation of hallucinated charts.
fn needle_pick_from_envelope(
    envelope: &NeedleEnvelope,
    candidates: &[(String, f64)],
) -> Option<String> {
    if !envelope.is_call() {
        tracing::warn!(
            target: "router.charts.select",
            envelope_type = ?envelope.r#type,
            "needle declined chart adjudication ({:?})",
            envelope.reasoning.as_deref().unwrap_or("no reason"),
        );
        return None;
    }
    let Some(tool) = envelope.single_tool() else {
        tracing::warn!(
            target: "router.charts.select",
            calls = envelope.function_calls.len(),
            "needle emitted multiple tool calls — treating as chart mismatch",
        );
        return None;
    };
    if !candidates.iter().any(|(n, _)| n == tool) {
        tracing::warn!(
            target: "router.charts.select",
            chart = tool,
            "needle picked a chart outside the candidate list — treating as mismatch",
        );
        return None;
    }
    Some(tool.to_string())
}

/// Deterministic tie-break for an ambiguous dep: first, then lexicographic
/// by id. Candidates are never empty (ambiguity implies >= 2 matches).
fn deterministic_pick(candidates: &[Entity]) -> Entity {
    candidates
        .iter()
        .min_by(|a, b| a.id.cmp(&b.id))
        .expect("ambiguous dep has at least one candidate")
        .clone()
}

// ── Adjudicator prompt + strict-JSON parsing ─────────────────────────────

/// The candidate's `fit` as judged by the LLM. A hint: final `ChartFit` is
/// derived from entity binding so a hallucinated "exact" never overrides a
/// demonstrably unbound dep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdjudicatorFit {
    Exact,
    Partial,
    Mismatch,
}

/// Parsed adjudicator output.
#[derive(Debug, Clone)]
struct AdjudicatorOutput {
    chart: Option<String>,
    fit: AdjudicatorFit,
    gaps: Vec<String>,
}

/// Build the adjudicator system prompt from the candidate list. Mirrors
/// `ClassifierStage::build_system_prompt`: the candidate list is
/// auto-generated from the store (no hand-maintained copy) and the output
/// schema is spelled out so small models can comply.
fn build_adjudicator_prompt(
    request: &str,
    candidates: &[(String, f64)],
    store: &ChartStore,
) -> String {
    let mut prompt = String::new();
    let _ = writeln!(
        prompt,
        "You are a chart selector. Given a user request and candidate charts, \
         choose the single best chart, or none."
    );
    let _ = writeln!(prompt);
    let _ = writeln!(prompt, "User request: {request}");
    let _ = writeln!(prompt);
    let _ = writeln!(prompt, "Candidate charts:");
    for (i, (name, score)) in candidates.iter().enumerate() {
        let Some(chart) = store.get(name) else {
            continue;
        };
        let provides: Vec<&str> = chart
            .targets
            .iter()
            .flat_map(|t| t.provides.iter().map(String::as_str))
            .collect();
        let _ = writeln!(prompt, "{}. name: \"{name}\"", i + 1);
        let _ = writeln!(prompt, "   description: \"{}\"", chart.description);
        let _ = writeln!(prompt, "   provides: [{}]", provides.join(", "));
        let _ = writeln!(prompt, "   similarity: {score:.2}");
    }
    let _ = writeln!(prompt);
    let _ = writeln!(
        prompt,
        "Output ONLY this JSON object:\n\
         {{\"chart\": \"<candidate name> or null\", \
         \"fit\": \"exact\" | \"partial\" | \"mismatch\", \
         \"gaps\": [\"<missing input>\"]}}"
    );
    let _ = writeln!(prompt);
    let _ = writeln!(prompt, "Rules:");
    let _ = writeln!(prompt, "- exact: the chart fully covers the request.");
    let _ = writeln!(
        prompt,
        "- partial: the chart is the best fit but some inputs are missing; list them in gaps."
    );
    let _ = writeln!(prompt, "- mismatch: no chart fits the request.");
    let _ = writeln!(prompt, "- Only output the JSON object, no other text.");
    prompt
}

/// Parse an adjudicator response into a `ChartMatch`-shaped verdict.
///
/// Tolerant by design (mirrors `parse_classifier_response`): the shared
/// `fluent_llm::parse_typed` codec strips markdown code fences, fast-paths
/// a direct parse, then extracts the first `{...}` object; missing fields are
/// sanitized to defaults.
fn parse_adjudicator_output(raw: &str) -> Option<AdjudicatorOutput> {
    let value =
        fluent_llm::parse_typed::<serde_json::Value>(raw, &serde_json::Value::Null, |_| {}).ok()?;
    sanitize_adjudicator_output(&value)
}

/// Fill missing adjudicator fields with defaults (sanitize philosophy).
fn sanitize_adjudicator_output(value: &serde_json::Value) -> Option<AdjudicatorOutput> {
    let obj = value.as_object()?;
    let chart: Option<String> = obj
        .get("chart")
        .and_then(|c| c.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let fit = match obj.get("fit").and_then(|f| f.as_str()) {
        Some("exact") => AdjudicatorFit::Exact,
        Some("partial") => AdjudicatorFit::Partial,
        Some("mismatch" | "none") => AdjudicatorFit::Mismatch,
        _ => {
            if chart.is_some() {
                AdjudicatorFit::Exact
            } else {
                AdjudicatorFit::Mismatch
            }
        }
    };
    let gaps: Vec<String> = obj
        .get("gaps")
        .and_then(|g| g.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Some(AdjudicatorOutput { chart, fit, gaps })
}

// ── Reranker (Step 2.5): LLM candidate re-ordering ──────────────────────

/// Build the reranker system prompt from the candidate list. The reranker
/// re-orders candidates by relevance (it does not pick a winner — that is the
/// adjudicator's job), and its output is an array of candidate names.
fn build_rerank_prompt(request: &str, candidates: &[(String, f64)], store: &ChartStore) -> String {
    let mut prompt = String::new();
    let _ = writeln!(
        prompt,
        "You are a chart reranker. Given a user request and candidate charts, \
         rank the candidates by relevance to the request, most relevant first."
    );
    let _ = writeln!(prompt);
    let _ = writeln!(prompt, "User request: {request}");
    let _ = writeln!(prompt);
    let _ = writeln!(prompt, "Candidate charts:");
    for (i, (name, score)) in candidates.iter().enumerate() {
        let Some(chart) = store.get(name) else {
            continue;
        };
        let provides: Vec<&str> = chart
            .targets
            .iter()
            .flat_map(|t| t.provides.iter().map(String::as_str))
            .collect();
        let _ = writeln!(prompt, "{}. name: \"{name}\"", i + 1);
        let _ = writeln!(prompt, "   description: \"{}\"", chart.description);
        let _ = writeln!(prompt, "   provides: [{}]", provides.join(", "));
        let _ = writeln!(prompt, "   similarity: {score:.2}");
    }
    let _ = writeln!(prompt);
    let _ = writeln!(
        prompt,
        "Output ONLY this JSON array of candidate names, most relevant first:\n\
         [\"<candidate name>\", ...]"
    );
    let _ = writeln!(prompt);
    let _ = writeln!(prompt, "Rules:");
    let _ = writeln!(prompt, "- Rank every candidate exactly once.");
    let _ = writeln!(prompt, "- Only output the JSON array, no other text.");
    prompt
}

/// Parse a reranker response into an ordered list of candidate names.
///
/// Tolerant by design (mirrors `parse_adjudicator_output`): the shared
/// `fluent_llm::parse_typed` codec strips fences, accepts a bare array or
/// a `{"ranking": [...]}` object, and extracts the first balanced JSON value.
/// Non-string entries are dropped. `None` means "not parseable" — the caller
/// keeps the HNSW order.
fn parse_rerank_output(raw: &str) -> Option<Vec<String>> {
    let value =
        fluent_llm::parse_typed::<serde_json::Value>(raw, &serde_json::Value::Null, |_| {}).ok()?;
    let names: Option<Vec<&str>> = match &value {
        serde_json::Value::Array(arr) => Some(arr.iter().filter_map(|v| v.as_str()).collect()),
        serde_json::Value::Object(obj) => obj
            .get("ranking")
            .and_then(|r| r.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect()),
        _ => None,
    };
    let names = names?;
    if names.is_empty() {
        return None;
    }
    Some(names.into_iter().map(str::to_string).collect())
}

// ── Ambiguity adjudicator: LLM pick + deterministic tie-break ───────

/// Build the ambiguity-adjudicator system prompt for a single dep.
///
/// The candidate entities are summarized (id, kind, and the value as JSON)
/// and the output schema is spelled out so small models can comply.
fn build_ambiguity_prompt(amb: &AmbiguousDep) -> String {
    let mut prompt = String::new();
    let _ = writeln!(
        prompt,
        "You are a resolver. A workflow dependency matched multiple context \
         entities; choose the single best one."
    );
    let _ = writeln!(prompt);
    let _ = writeln!(prompt, "Dependency: {}", amb.dep);
    if amb.description != amb.dep {
        let _ = writeln!(prompt, "Description: {}", amb.description);
    }
    let _ = writeln!(prompt);
    let _ = writeln!(prompt, "Candidate entities:");
    for (i, e) in amb.candidates.iter().enumerate() {
        let _ = writeln!(prompt, "{}. id: \"{}\"", i + 1, e.id);
        let _ = writeln!(prompt, "   kind: \"{}\"", e.kind);
        let _ = writeln!(
            prompt,
            "   value: {}",
            serde_json::to_string(&e.value).unwrap_or_default()
        );
    }
    let _ = writeln!(prompt);
    let _ = writeln!(
        prompt,
        "Output ONLY this JSON object:\n{{\"entity_id\": \"<candidate id>\"}}"
    );
    let _ = writeln!(prompt, "Rules:");
    let _ = writeln!(
        prompt,
        "- entity_id must be one of the candidate ids above."
    );
    let _ = writeln!(prompt, "- Only output the JSON object, no other text.");
    prompt
}

/// Parse an ambiguity-adjudicator response into a candidate entity id.
///
/// Tolerant by design: the shared `fluent_llm::parse_typed` codec strips
/// code fences and extracts the first `{...}` object. Returns `None` when the
/// id is missing or empty — the caller falls back to the deterministic
/// tie-break.
fn parse_ambiguity_output(raw: &str) -> Option<String> {
    let value =
        fluent_llm::parse_typed::<serde_json::Value>(raw, &serde_json::Value::Null, |_| {}).ok()?;
    let id = value
        .as_object()?
        .get("entity_id")
        .and_then(|v| v.as_str())?
        .trim();
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

#[cfg(test)]
mod tests {
    // Tests compare reordered HNSW/reranker scores against literal
    // thresholds — deliberate strict comparisons for exact-delta checks.
    #![allow(clippy::float_cmp)]
    use super::*;
    use crate::charts::store::{chart_from_str, ChartStore};
    use crate::hnsw::HnswIndexHandle;
    use crate::needle::backend::MockNeedleBackend;
    use crate::needle::envelope::{NeedleEnvelope, NeedleEnvelopeType, NeedleFunctionCall};
    use crate::test_stubs::{HashEmbedder, StubChatBackend};
    use std::path::Path;
    use tempfile::TempDir;

    /// A chart with two targets: `reproduce` (no deps) and `root_cause`
    /// (requires the `report` entity). Copy of the Appendix A seed shape.
    fn triage_chart_json() -> String {
        r#"{
            "name": "bug_triage",
            "description": "Triage a bug report into reproduction, root cause, and fix plan",
            "schema_version": 1,
            "author_model": "human",
            "targets": [
                {
                    "name": "reproduce",
                    "provides": ["repro_plan"],
                    "depends": [],
                    "template": "reproduce {{ request }}",
                    "essential": true
                },
                {
                    "name": "root_cause",
                    "provides": ["root_cause"],
                    "depends": [
                        { "kind": "capability", "name": "repro_plan" },
                        { "kind": "entity_match", "name": "report",
                          "description": "the bug report",
                          "predicate": {
                            "fields": [
                                { "path": "title", "ty": "string", "required": true }
                            ]
                          },
                          "required": true }
                    ],
                    "template": "cause {{ request }}",
                    "essential": true
                }
            ]
        }"#
        .to_string()
    }

    fn draft_chart_json() -> String {
        r#"{
            "name": "draft_doc",
            "description": "Draft a technical design document from notes",
            "schema_version": 1,
            "author_model": "human",
            "targets": [
                {
                    "name": "outline",
                    "provides": ["doc_outline"],
                    "depends": [],
                    "template": "outline {{ request }}",
                    "essential": true
                }
            ]
        }"#
        .to_string()
    }

    fn report_entity() -> Entity {
        Entity {
            id: "issue-42".into(),
            kind: "report".into(),
            value: serde_json::json!({"title": "Segfault on startup"}),
        }
    }

    /// Build a store (optionally indexed) from chart JSON strings.
    fn store_with(charts: &[String], index_path: Option<&Path>) -> Arc<ChartStore> {
        let handle = index_path.map(|p| HnswIndexHandle {
            name: "workflow_library".into(),
            path: p.display().to_string(),
        });
        let store = ChartStore::new(handle);
        for json in charts {
            let chart = chart_from_str(json).unwrap();
            store.upsert(chart).unwrap();
        }
        if index_path.is_some() {
            store
                .build_index(Arc::new(HashEmbedder::new(256)))
                .expect("index builds");
        }
        Arc::new(store)
    }

    fn selector(
        store: Arc<ChartStore>,
        client: Option<Arc<dyn ChatBackend>>,
        min_score: f64,
    ) -> ChartSelector {
        ChartSelector::new(
            store,
            client,
            ChartsConfig {
                dir: None,
                index_path: None,
                selector_model: None,
                max_candidates: 5,
                min_score,
                entity_context: true,
            },
        )
    }

    #[test]
    fn deterministic_capability_hit_makes_no_llm_call() {
        let store = store_with(&[triage_chart_json()], None);
        // Empty backend: any LLM call would fail with NoResponse.
        let selector = selector(
            store.clone(),
            Some(Arc::new(StubChatBackend::new(Vec::new()))),
            0.6,
        );
        let m = selector
            .select("please bug_triage this issue", &[report_entity()])
            .expect("deterministic hit must not call the LLM");
        assert_eq!(m.chart, "bug_triage");
        assert!((m.score - 1.0).abs() < f64::EPSILON);
        assert_eq!(m.fit, ChartFit::Exact);
    }

    #[test]
    fn deterministic_hit_names_provides_asset() {
        let store = store_with(&[triage_chart_json(), draft_chart_json()], None);
        let selector = selector(store, Some(Arc::new(StubChatBackend::new(Vec::new()))), 0.6);
        // `repro_plan` is a provides asset of bug_triage.
        let m = selector
            .select("produce the repro_plan for this crash", &[report_entity()])
            .expect("provides-asset hit");
        assert_eq!(m.chart, "bug_triage");
    }

    #[test]
    fn hnsw_top_k_returns_seeded_chart_for_near_duplicate() {
        let tmp = TempDir::new().unwrap();
        let index_path = tmp.path().join("workflow_library.sqlite");
        let store = store_with(
            &[triage_chart_json(), draft_chart_json()],
            Some(&index_path),
        );
        assert!(store.is_index_built());
        let selector = selector(store, None, 0.0);
        let m = selector
            .select(
                "Triage a bug report into reproduction, root cause, and fix plan",
                &[report_entity()],
            )
            .expect("hnsw retrieval");
        assert_eq!(
            m.chart, "bug_triage",
            "near-duplicate query retrieves the chart"
        );
        assert!(
            m.score >= 0.9,
            "near-duplicate query should score highly, got {}",
            m.score
        );
    }

    #[test]
    fn min_score_filters_below_threshold_candidates() {
        let tmp = TempDir::new().unwrap();
        let index_path = tmp.path().join("workflow_library.sqlite");
        let store = store_with(
            &[triage_chart_json(), draft_chart_json()],
            Some(&index_path),
        );
        // Unrelated request → no candidate clears min_score 0.6.
        let strict = selector(store.clone(), None, 0.6);
        let m = strict
            .select("how do I cook pasta for dinner", &[])
            .expect("selection");
        assert_eq!(m.fit, ChartFit::Mismatch);

        // Same request with a permissive threshold → HNSW still has a top hit.
        let lax = selector(store, None, 0.0);
        let m = lax
            .select("how do I cook pasta for dinner", &[])
            .expect("selection");
        assert_ne!(m.fit, ChartFit::Mismatch);
    }

    // ── Step 3: Needle adjudication ───────────────────────────────────

    fn needle_selector(
        store: Arc<ChartStore>,
        needle: Option<Arc<dyn NeedleBackend>>,
        min_score: f64,
    ) -> ChartSelector {
        selector(store, None, min_score)
            .with_needle_adjudicator(needle.expect("needle backend provided"))
    }

    fn needle_call(tool: &str) -> NeedleEnvelope {
        NeedleEnvelope {
            r#type: NeedleEnvelopeType::Call,
            success: None,
            error: None,
            error_code: None,
            function_calls: vec![NeedleFunctionCall {
                name: tool.to_string(),
                arguments: serde_json::json!({}),
            }],
            reasoning: Some(format!("pick {tool}")),
            confidence: Some(0.9),
            results: None,
        }
    }

    #[test]
    fn needle_adjudicator_picks_chart_from_shortlist() {
        let tmp = TempDir::new().unwrap();
        let index_path = tmp.path().join("workflow_library.sqlite");
        let store = store_with(&[triage_chart_json(), draft_chart_json()], Some(&index_path));
        let needle = Arc::new(MockNeedleBackend::always(needle_call("bug_triage")));
        let selector = needle_selector(store, Some(needle), 0.0);
        let m = selector
            .select(
                "Triage a bug report into reproduction, root cause, and fix plan",
                &[report_entity()],
            )
            .expect("needle-adjudicated selection");
        assert_eq!(m.chart, "bug_triage");
        assert_eq!(m.fit, ChartFit::Exact, "binding still derives the fit");
    }

    #[test]
    fn needle_adjudicator_decline_is_mismatch() {
        let tmp = TempDir::new().unwrap();
        let index_path = tmp.path().join("workflow_library.sqlite");
        let store = store_with(&[triage_chart_json()], Some(&index_path));
        // A refuse envelope is a decline → honored as a chart Mismatch (fresh
        // planning), never a silent default pick.
        let needle = Arc::new(MockNeedleBackend::always(NeedleEnvelope {
            r#type: NeedleEnvelopeType::Refuse,
            success: None,
            error: None,
            error_code: None,
            function_calls: vec![],
            reasoning: Some("no chart fits".to_string()),
            confidence: None,
            results: None,
        }));
        let selector = needle_selector(store, Some(needle), 0.0);
        let m = selector
            .select("Triage a bug report", &[])
            .expect("selection");
        assert_eq!(m.fit, ChartFit::Mismatch);
    }

    #[test]
    fn needle_adjudicator_unknown_candidate_is_mismatch() {
        let tmp = TempDir::new().unwrap();
        let index_path = tmp.path().join("workflow_library.sqlite");
        let store = store_with(&[triage_chart_json()], Some(&index_path));
        // A tool call naming a chart outside the candidate list is a decline,
        // mirroring the LLM adjudicator's hallucinated-chart rejection.
        let needle = Arc::new(MockNeedleBackend::always(needle_call("not_a_real_chart")));
        let selector = needle_selector(store, Some(needle), 0.0);
        let m = selector
            .select("Triage a bug report", &[])
            .expect("selection");
        assert_eq!(m.fit, ChartFit::Mismatch);
    }

    #[test]
    fn needle_adjudicator_error_degrades_to_top_hnsw_candidate() {
        let tmp = TempDir::new().unwrap();
        let index_path = tmp.path().join("workflow_library.sqlite");
        let store = store_with(
            &[triage_chart_json(), draft_chart_json()],
            Some(&index_path),
        );
        // A failing backend behaves like an LLM outage: degrade to the top
        // HNSW candidate rather than error the request.
        let needle = Arc::new(MockNeedleBackend::failing());
        let selector = needle_selector(store, Some(needle), 0.0);
        let m = selector
            .select(
                "Triage a bug report into reproduction, root cause, and fix plan",
                &[report_entity()],
            )
            .expect("selection");
        assert_eq!(m.chart, "bug_triage", "top HNSW candidate wins on needle error");
    }

    #[test]
    fn deterministic_pre_match_bypasses_needle() {
        let store = store_with(&[triage_chart_json()], None);
        // A request naming the chart resolves deterministically (step 1) —
        // the failing Needle backend must never be called.
        let needle = Arc::new(MockNeedleBackend::failing());
        let selector = needle_selector(store, Some(needle), 0.6);
        let m = selector
            .select("please bug_triage this issue", &[report_entity()])
            .expect("deterministic hit must not reach needle");
        assert_eq!(m.chart, "bug_triage");
        assert!((m.score - 1.0).abs() < f64::EPSILON);
        assert_eq!(m.fit, ChartFit::Exact);
    }

    #[test]
    fn parse_candidate_tools_lists_every_candidate() {
        let tmp = TempDir::new().unwrap();
        let index_path = tmp.path().join("workflow_library.sqlite");
        let store = store_with(
            &[triage_chart_json(), draft_chart_json()],
            Some(&index_path),
        );
        let candidates = vec![
            ("bug_triage".to_string(), 0.9),
            ("draft_doc".to_string(), 0.8),
        ];
        let tools: serde_json::Value =
            serde_json::from_str(&render_candidate_tools(&candidates, &store)).unwrap();
        let arr = tools.as_array().expect("tool array");
        assert_eq!(arr.len(), 2);
        let names: Vec<&str> = arr
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert_eq!(names, vec!["bug_triage", "draft_doc"]);
    }

    #[test]
    fn adjudicator_exact_for_clean_fit() {
        let tmp = TempDir::new().unwrap();
        let index_path = tmp.path().join("workflow_library.sqlite");
        let store = store_with(&[triage_chart_json()], Some(&index_path));
        let adjudicator =
            StubChatBackend::always(r#"{"chart": "bug_triage", "fit": "exact", "gaps": []}"#);
        let selector = selector(store.clone(), Some(Arc::new(adjudicator)), 0.0);
        let m = selector
            .select(
                "Triage a bug report into reproduction, root cause, and fix plan",
                &[report_entity()],
            )
            .expect("adjudicated selection");
        assert_eq!(m.chart, "bug_triage");
        assert_eq!(m.fit, ChartFit::Exact);
    }

    #[test]
    fn adjudicator_partial_with_gaps_when_unbound() {
        let tmp = TempDir::new().unwrap();
        let index_path = tmp.path().join("workflow_library.sqlite");
        let store = store_with(&[triage_chart_json()], Some(&index_path));
        // The LLM picks the chart; binding derives the fit and the gaps.
        let adjudicator = StubChatBackend::always(
            r#"{"chart": "bug_triage", "fit": "partial", "gaps": ["report"]}"#,
        );
        let selector = selector(store, Some(Arc::new(adjudicator)), 0.0);
        let m = selector
            .select(
                "Triage a bug report into reproduction, root cause, and fix plan",
                &[], // no report entity → root_cause is unbound
            )
            .expect("adjudicated selection");
        assert_eq!(m.chart, "bug_triage");
        match m.fit {
            ChartFit::Partial { gaps } => {
                assert!(
                    gaps.iter().any(|g| g == "report"),
                    "expected 'report' in gaps, got {gaps:?}"
                );
            }
            other => panic!("expected Partial, got {other:?}"),
        }
    }

    #[test]
    fn entity_only_capability_dep_classifies_partial_not_exact() {
        // A chart whose capability dep has no in-graph provider and no
        // matching entity classifies `Partial { gaps }` (drives the
        // interview) instead of `Exact`-then-`ChartError::Compile`.
        let tmp = TempDir::new().unwrap();
        let index_path = tmp.path().join("workflow_library.sqlite");
        // Same seed shape, but root_cause depends on a capability nothing
        // provides in-graph (`external_data`) in addition to the report.
        let gapped = r#"{
            "name": "bug_triage",
            "description": "Triage a bug report into reproduction, root cause, and fix plan",
            "schema_version": 1,
            "author_model": "human",
            "targets": [
                {
                    "name": "reproduce",
                    "provides": ["repro_plan"],
                    "depends": [],
                    "template": "reproduce {{ request }}",
                    "essential": true
                },
                {
                    "name": "root_cause",
                    "provides": ["root_cause"],
                    "depends": [
                        { "kind": "capability", "name": "external_data" },
                        { "kind": "entity_match", "name": "report",
                          "description": "the bug report",
                          "predicate": {
                            "fields": [
                                { "path": "title", "ty": "string", "required": true }
                            ]
                          },
                          "required": true }
                    ],
                    "template": "cause {{ request }}",
                    "essential": true
                }
            ]
        }"#;
        let store = store_with(&[gapped.to_string()], Some(&index_path));
        let selector = selector(store, None, 0.0);
        // No entities: neither `external_data` (no provider) nor `report`
        // (no matching entity) is bound.
        let m = selector
            .select(
                "Triage a bug report into reproduction, root cause, and fix plan",
                &[],
            )
            .expect("selection");
        assert_eq!(m.chart, "bug_triage");
        match m.fit {
            ChartFit::Partial { gaps } => {
                assert!(
                    gaps.iter().any(|g| g == "external_data"),
                    "expected 'external_data' in gaps, got {gaps:?}"
                );
            }
            other => panic!("expected Partial, got {other:?}"),
        }
    }

    #[test]
    fn adjudicator_mismatch_when_llm_rejects_candidates() {
        let tmp = TempDir::new().unwrap();
        let index_path = tmp.path().join("workflow_library.sqlite");
        let store = store_with(&[triage_chart_json()], Some(&index_path));
        let adjudicator = StubChatBackend::always(r#"{"chart": null, "fit": "mismatch"}"#);
        let selector = selector(store, Some(Arc::new(adjudicator)), 0.0);
        let m = selector
            .select("Triage a bug report", &[])
            .expect("adjudicated selection");
        assert_eq!(m.fit, ChartFit::Mismatch);
    }

    #[test]
    fn adjudicator_hallucinated_chart_is_rejected() {
        let tmp = TempDir::new().unwrap();
        let index_path = tmp.path().join("workflow_library.sqlite");
        let store = store_with(&[triage_chart_json()], Some(&index_path));
        let adjudicator =
            StubChatBackend::always(r#"{"chart": "not_a_real_chart", "fit": "exact"}"#);
        let selector = selector(store, Some(Arc::new(adjudicator)), 0.0);
        let m = selector
            .select("Triage a bug report", &[])
            .expect("adjudicated selection");
        assert_eq!(m.fit, ChartFit::Mismatch);
    }

    #[test]
    fn parse_adjudicator_output_tolerates_fences() {
        let out = parse_adjudicator_output(
            "```json\n{\"chart\": \"bug_triage\", \"fit\": \"exact\", \"gaps\": []}\n```",
        )
        .unwrap();
        assert_eq!(out.chart.as_deref(), Some("bug_triage"));
        assert_eq!(out.fit, AdjudicatorFit::Exact);
        assert!(out.gaps.is_empty());
    }

    #[test]
    fn parse_adjudicator_output_missing_fit_infers_from_chart() {
        let out = parse_adjudicator_output(r#"{"chart": "bug_triage"}"#).unwrap();
        assert_eq!(out.fit, AdjudicatorFit::Exact);
        let out = parse_adjudicator_output(r#"{"chart": null}"#).unwrap();
        assert_eq!(out.fit, AdjudicatorFit::Mismatch);
    }

    // ── Step 2.5: reranker ────────────────────────────────────────────

    #[test]
    fn parse_rerank_output_accepts_array_and_ranking_object() {
        assert_eq!(
            parse_rerank_output(r#"["draft_doc", "bug_triage"]"#).unwrap(),
            vec!["draft_doc".to_string(), "bug_triage".to_string()]
        );
        assert_eq!(
            parse_rerank_output(r#"{"ranking": ["bug_triage"]}"#).unwrap(),
            vec!["bug_triage".to_string()]
        );
        // Fences tolerated; the noise before the JSON array is dropped.
        let fenced = "Sure!\n```json\n[\"draft_doc\", \"bug_triage\"]\n```";
        assert_eq!(
            parse_rerank_output(fenced).unwrap(),
            vec!["draft_doc".to_string(), "bug_triage".to_string()]
        );
    }

    #[test]
    fn parse_rerank_output_rejects_garbage() {
        assert!(parse_rerank_output("not json at all").is_none());
        assert!(parse_rerank_output(r#"{"chart": "bug_triage"}"#).is_none());
        assert!(
            parse_rerank_output("[]").is_none(),
            "empty ranking is unusable"
        );
    }

    #[test]
    fn rerank_reorders_candidates_and_preserves_scores() {
        let tmp = TempDir::new().unwrap();
        let index_path = tmp.path().join("workflow_library.sqlite");
        let store = store_with(
            &[triage_chart_json(), draft_chart_json()],
            Some(&index_path),
        );
        // HNSW order puts bug_triage first; the reranker prefers draft_doc.
        let candidates = vec![
            ("bug_triage".to_string(), 0.9),
            ("draft_doc".to_string(), 0.8),
        ];
        let reranker = StubChatBackend::always(r#"["draft_doc", "bug_triage"]"#);
        let selector = selector(store, None, 0.0).with_reranker(Arc::new(reranker));
        let reordered = selector.rerank("Draft a design doc", candidates.clone());
        assert_eq!(reordered[0].0, "draft_doc", "reranker order wins");
        assert_eq!(reordered[1].0, "bug_triage");
        assert_eq!(reordered[0].1, 0.8, "original HNSW score preserved");
        assert_eq!(reordered[1].1, 0.9);
        assert_eq!(reordered.len(), candidates.len());
    }

    #[test]
    fn rerank_degrades_to_hnsw_order_on_failure() {
        let tmp = TempDir::new().unwrap();
        let index_path = tmp.path().join("workflow_library.sqlite");
        let store = store_with(
            &[triage_chart_json(), draft_chart_json()],
            Some(&index_path),
        );
        let candidates = vec![
            ("bug_triage".to_string(), 0.9),
            ("draft_doc".to_string(), 0.8),
        ];
        // NoResponse backend → rerank call fails → HNSW order preserved.
        let sel = selector(store.clone(), None, 0.0)
            .with_reranker(Arc::new(StubChatBackend::new(Vec::new())));
        let reordered = sel.rerank("Draft a design doc", candidates.clone());
        assert_eq!(reordered, candidates, "failure must not reorder");

        // Hallucinated chart names are dropped; unnamed candidates re-appended.
        let reranker = StubChatBackend::always(r#"["not_a_real_chart"]"#);
        let sel = selector(store, None, 0.0).with_reranker(Arc::new(reranker));
        let reordered = sel.rerank("Draft a design doc", candidates.clone());
        assert_eq!(
            reordered, candidates,
            "invalid names fall back to HNSW order"
        );
    }

    #[test]
    fn rerank_missing_backend_keeps_candidates_unchanged() {
        let store = store_with(&[triage_chart_json()], None);
        let sel = selector(store, None, 0.0);
        let candidates = vec![("bug_triage".to_string(), 0.9)];
        assert_eq!(sel.rerank("anything", candidates.clone()), candidates);
    }

    // ── Ambiguity adjudication ───────────────────────────────────────

    /// Two `report` entities both matching the bug_triage `report` predicate
    /// → the dep binds ambiguously; adjudication must resolve it.
    fn two_report_entities() -> Vec<Entity> {
        vec![
            Entity {
                id: "issue-42".into(),
                kind: "report".into(),
                value: serde_json::json!({"title": "Segfault on startup"}),
            },
            Entity {
                id: "issue-43".into(),
                kind: "report".into(),
                value: serde_json::json!({"title": "Memory leak on shutdown"}),
            },
        ]
    }

    #[test]
    fn ambiguous_dep_resolved_by_llm_adjudicator() {
        let store = store_with(&[triage_chart_json()], None);
        // Deterministic hit → no step-3 adjudicator call; the only LLM call
        // is the ambiguity pick for `report`.
        let selector = selector(
            store,
            Some(Arc::new(StubChatBackend::always(
                r#"{"entity_id": "issue-43"}"#,
            ))),
            0.6,
        );
        let m = selector
            .select("bug_triage this issue", &two_report_entities())
            .expect("selection");
        assert_eq!(m.fit, ChartFit::Exact, "ambiguity must not force a gap");
        let bindings = m.bindings.as_ref().expect("bindings present");
        assert!(
            bindings.ambiguous.is_empty(),
            "ambiguous deps must be adjudicated away: {:?}",
            bindings.ambiguous
        );
        let picked = &bindings.entity_map["report"][0];
        assert_eq!(picked.id, "issue-43", "LLM pick wins");
        assert!(
            bindings.satisfied.contains("entity:report:issue-43"),
            "picked entity is satisfied"
        );
    }

    #[test]
    fn ambiguous_dep_falls_back_to_deterministic_tie_break() {
        let store = store_with(&[triage_chart_json()], None);
        // Unparseable LLM output → deterministic tie-break (min id).
        let selector = selector(
            store,
            Some(Arc::new(StubChatBackend::always("not json at all"))),
            0.6,
        );
        let m = selector
            .select("bug_triage this issue", &two_report_entities())
            .expect("selection");
        assert_eq!(m.fit, ChartFit::Exact);
        let bindings = m.bindings.as_ref().expect("bindings present");
        assert!(bindings.ambiguous.is_empty());
        assert_eq!(
            bindings.entity_map["report"][0].id, "issue-42",
            "lexicographic tie-break picks the smaller id"
        );
    }

    #[test]
    fn ambiguous_dep_resolved_without_llm_backend() {
        let store = store_with(&[triage_chart_json()], None);
        // No backend at all → deterministic tie-break, no LLM call.
        let selector = selector(store, None, 0.6);
        let m = selector
            .select("bug_triage this issue", &two_report_entities())
            .expect("selection");
        assert_eq!(m.fit, ChartFit::Exact);
        let bindings = m.bindings.as_ref().expect("bindings present");
        assert!(bindings.ambiguous.is_empty());
        assert_eq!(bindings.entity_map["report"][0].id, "issue-42");
    }

    #[test]
    fn ambiguous_dep_llm_named_non_candidate_falls_back() {
        let store = store_with(&[triage_chart_json()], None);
        let selector = selector(
            store,
            Some(Arc::new(StubChatBackend::always(
                r#"{"entity_id": "hallucinated-99"}"#,
            ))),
            0.6,
        );
        let m = selector
            .select("bug_triage this issue", &two_report_entities())
            .expect("selection");
        let bindings = m.bindings.as_ref().expect("bindings present");
        assert_eq!(
            bindings.entity_map["report"][0].id, "issue-42",
            "invalid LLM id falls back to the deterministic pick"
        );
    }

    #[test]
    fn parse_ambiguity_output_tolerates_fences_and_noise() {
        let id = parse_ambiguity_output("```json\n{\"entity_id\": \"issue-7\"}\n```").unwrap();
        assert_eq!(id, "issue-7");
        let id = parse_ambiguity_output(
            "considering candidates... {\"entity_id\": \"issue-8\"} hope that helps",
        )
        .unwrap();
        assert_eq!(id, "issue-8");
        assert!(parse_ambiguity_output(r#"{"entity_id": ""}"#).is_none());
        assert!(parse_ambiguity_output("no json").is_none());
    }
}
