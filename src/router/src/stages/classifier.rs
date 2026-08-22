//! Stage 2: ClassifierStage — single LLM call that replaces QualityGate,
//! PlanningRefinement, and GuardrailCheck. Acts as an FSM switch: the LLM
//! returns either a direct response, a routing target, or a rejection.
//! Configurable via `RoutingConfig` from the top-level coral-router config.
//!
//! When `RouterConfig.classification` is `Some`, the stage becomes a thin
//! wrapper over the classification-tree engine (`stages::tree`): the engine
//! evaluates the nested tree recursively, auto-builds prompts from child keys
//! and descriptions, enforces per-node coherence/safety thresholds, and emits a
//! `StageDecision` per visited node plus a durable audit record. The flat path
//! below no longer guesses route names from classifier `action`/`intent`
//! strings — route selection is the tree's job.
//!
//! The LLM backend is injected as `Arc<dyn ChatBackend>` rather than a
//! concrete `LlmClient`, so mock/stub backends can be injected for testing
//! without duplicating the pipeline wiring.

use std::collections::HashMap;
use std::fmt::Write;
use std::sync::Arc;
use std::time::Instant;

use fluent_concurrency::pool::Limiter;
use fluent_llm::client::ChatBackend;
use fluent_llm::ChatMessage;
use fluent_wvr::prelude::*;

use crate::metrics::classify_error;

use crate::config::{ClassifierFailurePolicy, ClassifierOutput, RoutingConfig};
use crate::pipeline::RoutingTarget;
use crate::pipeline_types::{
    PipelineStage, StageDecision, StageDecisionProducer, StageMetadata, StageVerdict,
};
use crate::score_matrix::ScoreMatrix;
use crate::stages::common::{coerce_float, coerce_string, extract_user_message};
use crate::stages::tree::ClassificationEngine;
use crate::target_match::TargetMatcher;

const DEFAULT_COMPLETENESS: f64 = 0.5;

/// Build the per-call `response_format` extras for the classifier LLM call.
///
/// The coral-router llama-server fork (`tools/server/server-common.cpp`
/// `oaicompat_chat_params_parse`) accepts `response_format` with
/// `{"type": "json_object", "schema": {...}}` and converts the schema to a
/// GBNF grammar (`common/json-schema-to-grammar.cpp`) that constrains
/// sampling: the model *cannot* emit prose outside the schema, which
/// eliminates the "model answered the question in prose instead of JSON"
/// failure class that the post-hoc repair ladder cannot recover.
///
/// The schema is hand-built, not derived from `ClassifierOutput::describe()`:
/// the derive types `Option<T>` fields as `"string"` and emits string-typed
/// numeric bounds, both of which the fork's converter mishandles (a string
/// `minimum` throws in `.get<int64_t>()` for integer fields). The hand-built
/// shape is guarded against drift by a test asserting its field names equal
/// `ClassifierOutput::default().field_names()`.
///
/// Required fields are ordered to match the fork's grammar, which emits
/// required properties in schema order before any optional properties — the
/// classifier prompt's "Output schema" block lists the same order so the two
/// documents stay coherent.
fn classifier_response_format() -> serde_json::Value {
    serde_json::json!({
        "response_format": {
            "type": "json_object",
            "schema": {
                "type": "object",
                "properties": {
                    "domain": { "type": "string" },
                    "coherence_score": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
                    "safety_score": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
                    "confidence": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
                    "reason": { "type": "string" },
                    "response": { "type": "string" },
                    "target": { "type": "string" },
                    "completeness": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
                    "risk": { "type": "number", "minimum": 0.0, "maximum": 1.0 }
                },
                "required": ["domain", "coherence_score", "safety_score", "confidence", "reason"]
            }
        }
    })
}

/// Sanitize a classifier JSON blob by filling in missing required fields
/// with defaults, and coercing string-valued numbers back to numeric.
/// This lets partial or slightly malformed responses (common from smaller LLMs)
/// survive parsing instead of falling back to the default route.
///
/// The field-coercion lives in the shared `stages::common` helpers (the
/// "surviving normalization" both this stage and the tree engine use).
/// Route-name guessing is gone — the classification tree replaces it, and the
/// classifier's `domain` is the route key it emits (defaulted to `local` here;
/// `parse_classifier_response` re-binds it to the config's `default_route`).
fn sanitize_classifier_json(v: &mut serde_json::Value) {
    if let Some(obj) = v.as_object_mut() {
        coerce_float(obj, "coherence_score", 1.0);
        coerce_float(obj, "safety_score", 1.0);
        coerce_float(obj, "confidence", 1.0);
        coerce_float(obj, "completeness", 0.5);
        coerce_float(obj, "risk", 0.0);
        coerce_string(obj, "domain", "local");
        coerce_string(obj, "reason", "");
    }
}

/// Log the raw classifier response with clear delimiters so multiline content
/// is visible in the log output instead of being hidden by structured key-value
/// formatting (which breaks on embedded newlines).
fn log_classifier_raw_response(response: &str) {
    tracing::debug!(
        target: "router.pipeline.stage2",
        "--- raw classifier response ({} bytes) ---\n{}--- end raw response ---",
        response.len(),
        response,
    );
}

// LLM boundary: parse the classifier's raw LLM response text (JSON in a
// string from the model). Routes through the shared `fluent_llm::parse_typed`
// codec — direct-deserialize fast path, then fence-strip → parse →
// extract-first-value → deterministic repair — then, as a last resort, the
// fluent-wvr schema-driven boundary decode (`decode_boundary`), which
// recognises members against `ClassifierOutput`'s own `field_names()` schema
// and coerces each value string through `set_field` (the derive-macro
// `coerce`/`parse` modes). When no JSON attempt was made at all, and the
// requested route permits direct answering, pure prose becomes a direct answer
// ([`prose_as_respond`]). `Ok((output, true))` = pristine parse;
// `Ok((output, false))` = recovered; `Err(reason)` = total parse failure (the
// caller applies the `ClassifierFailurePolicy`).
//
// `direct_answer_allowed` gates the prose-as-respond rung: only routes that
// permit the classifier to answer directly (`always_route: false`) may treat
// a pure-prose response as a direct answer. On `always_route` routes the
// classifier is never allowed to answer directly, so prose remains a hard
// failure.
fn parse_classifier_response(
    response: &str,
    default_route: &str,
    direct_answer_allowed: bool,
) -> Result<(ClassifierOutput, bool), String> {
    match fluent_llm::parse_typed::<ClassifierOutput>(
        response,
        &serde_json::Value::Null,
        sanitize_classifier_json,
    ) {
        Ok(o) => {
            // `true` iff the raw text directly deserialized (the codec's fast
            // path); a small re-parse of the already-owned string, once per
            // classifier call — not on any hot loop.
            let ok = serde_json::from_str::<ClassifierOutput>(response).is_ok();
            let mut output = o;
            // An empty `domain` (the model omitted the route key) binds to the
            // configured default route — the route table is the single source
            // of truth, so a fabricated/absent domain degrades to `default_route`.
            if output.domain.trim().is_empty() {
                output.domain = default_route.to_string();
            }
            Ok((output, ok))
        }
        Err(fluent_llm::JsonParseError::NoJson) => {
            if let Some(output) = try_boundary_decode(response, default_route) {
                return Ok((output, false));
            }
            // No JSON attempt at all: if this route permits direct answering,
            // the prose IS the answer the model was allowed to give.
            if direct_answer_allowed {
                if let Some(output) = prose_as_respond(response, default_route) {
                    return Ok((output, true));
                }
            }
            tracing::error!(
                target: "router.pipeline.stage2",
                raw_response_len = response.len(),
                "classifier LLM response was not valid JSON at all",
            );
            log_classifier_raw_response(response);
            Err("invalid JSON in LLM response".into())
        }
        Err(e) => {
            if let Some(output) = try_boundary_decode(response, default_route) {
                return Ok((output, false));
            }
            tracing::error!(
                target: "router.pipeline.stage2",
                error = %e,
                raw_response_len = response.len(),
                "classifier response survived sanitization but still failed to parse",
            );
            log_classifier_raw_response(response);
            Err(format!("post-sanitize parse error: {e}"))
        }
    }
}

/// Last-resort, schema-driven recovery of a classifier response that the whole
/// repair pipeline could not parse as JSON (e.g. brace-less
/// `domain: local, confidence: 0.9`). Member values are coerced through
/// `set_field` with the field's `#[field(coerce = "...")]` / `parse = "number"`
/// modes — the same vocabulary the repair walker uses — so a member that fails
/// to coerce keeps its (failing) default instead of fabricating a value.
///
/// Conservative by construction: a recovered output must carry a non-empty
/// `domain` (else there is nothing to route on), and it is always flagged
/// recovered (`ok == false`). Returns `None` when nothing decodes.
fn try_boundary_decode(response: &str, default_route: &str) -> Option<ClassifierOutput> {
    let schema = ClassifierOutput::default().field_names();
    let opts = fluent_wvr::BoundaryOptions::for_schema(schema);
    let (mut output, decoded) =
        fluent_wvr::decode_boundary::<ClassifierOutput>(response, &opts).ok()?;
    if output.domain.trim().is_empty() {
        output.domain = default_route.to_string();
    }
    tracing::info!(
        target: "router.pipeline.stage2",
        decoded_fields = decoded.len(),
        "classifier recovered via fluent-wvr schema-driven boundary decode",
    );
    Some(output)
}

/// Interpret a pure-prose classifier response as a direct answer.
///
/// When the model answered the user's question in natural language with *no*
/// JSON attempt at all (no `{`/`[`, no schema members — the failure class seen
/// in the `classifier_failures/` dumps), the prose IS the direct answer the
/// model was permitted to give on this route. Synthesizing a `respond` output
/// converts a hard rejection into a usable answer with zero extra LLM calls.
///
/// Conservative by construction:
/// - Fires only on the `NoJson` branch — the codec's signal that no parseable
///   JSON value exists. But a brace anywhere (`{`/`[`) is an attempted (if
///   broken) JSON envelope, so it stays on the repair/failure path rather than
///   being guessed as an answer — only text with no JSON attempt at all can be
///   a direct answer.
/// - Requires non-empty trimmed prose (an empty response is a failure, not an
///   answer).
/// - Returns `ok == true`: the prose is a complete answer, so it must NOT be
///   flagged as a retryable fallback (`fallback: false` in the decision
///   metadata) — a corrective re-prompt would discard the good answer.
/// - Scores mirror `sanitize_classifier_json`'s defaults for a `respond` that
///   omitted scores (coherence 1.0, safety 1.0), so the output passes the
///   thresholds exactly like a pristine respond would.
fn prose_as_respond(response: &str, default_route: &str) -> Option<ClassifierOutput> {
    let prose = response.trim();
    if prose.is_empty() || prose.contains(['{', '[']) {
        return None;
    }
    tracing::info!(
        target: "router.pipeline.stage2",
        raw_len = response.len(),
        "classifier prose response interpreted as a direct answer (no JSON envelope)",
    );
    Some(ClassifierOutput {
        domain: default_route.to_string(),
        response: Some(prose.to_string()),
        coherence_score: 1.0,
        safety_score: 1.0,
        confidence: 1.0,
        reason: "prose response interpreted as a direct answer (no JSON envelope)".into(),
        ..ClassifierOutput::default()
    })
}

fn check_thresholds(
    output: &ClassifierOutput,
    coherence_threshold: f64,
    safety_threshold: f64,
) -> Option<StageDecision> {
    let coherence_ok = output.coherence_score >= coherence_threshold;
    let safety_ok = output.safety_score >= safety_threshold;
    if coherence_ok && safety_ok {
        return None;
    }
    let reason = if coherence_ok {
        format!(
            "rejected: safety {:.2} below threshold {:.2}",
            output.safety_score, safety_threshold
        )
    } else {
        format!(
            "rejected: coherence {:.2} below threshold {:.2}",
            output.coherence_score, coherence_threshold
        )
    };
    Some(StageDecision {
        stage: PipelineStage::Classifier,
        verdict: StageVerdict::Rejected,
        score: Some(output.coherence_score),
        reason,
        latency_ms: 0,
        metadata: serde_json::json!({
            "coherence_score": output.coherence_score,
            "safety_score": output.safety_score,
            "domain": output.domain,
            "confidence": output.confidence,
        }),
    })
}

/// Resolve a route to a typed `RoutingTarget` through the target-matching
/// ladder when one is available, falling back to the static
/// cheapest-qualifying pick.
///
/// The ladder only runs for 2+ member groups (a single-member group has
/// nothing to climb — it resolves statically, byte-identical to today, with no
/// extra LLM call). The matcher never fails hard: on absence, an unresolvable
/// group, an empty candidate list, or an internal `None`, the static
/// `routing_target` path runs (defense in depth).
///
/// Model selection inside the group is entirely the ladder's job — it
/// self-assesses each candidate's complexity from `user_text` (DD-2); the
/// classifier no longer seeds a complexity estimate.
///
/// This is the one selection algorithm shared by the LLM-driven flat path and
/// the matrix-authoritative branch (DRY — §4.3 of the roadmap).
fn resolve_via_matcher(
    routing_config: &RoutingConfig,
    matcher: Option<&TargetMatcher>,
    route: &str,
    user_text: &str,
) -> Option<RoutingTarget> {
    if let Some(matcher) = matcher {
        if let Some(group) = routing_config.route_group(route) {
            let candidates = crate::target_match::candidates_for_group(routing_config, group);
            if candidates.len() >= 2 {
                if let Some(tm) = matcher.match_target(
                    route,
                    group,
                    routing_config,
                    &candidates,
                    user_text,
                ) {
                    tracing::info!(
                        target: "router.pipeline.stage2",
                        route = %route,
                        group = %group,
                        model = %tm.primary.model,
                        assessments = tm.assessments.len(),
                        "routing target resolved via self-assessment ladder",
                    );
                    return Some(tm.primary);
                }
                tracing::warn!(
                    target: "router.pipeline.stage2",
                    route = %route,
                    "target-matching ladder produced no target — static fallback",
                );
            }
        }
    }
    routing_config.routing_target(route, None)
}

/// Resolve a classifier decision to a typed `RoutingTarget` — the single
/// respond-vs-route decision point (DD-1, DRY §5 rule 3).
///
/// 1. **Domain → route (DD-4):** the classifier's `domain` is the route key. An
///    unknown (or empty) `domain` resolves to `default_route` with a warning —
///    the route table is the single source of truth, never a fabricated name.
/// 2. **derive_action:** `domain`'s `always_route` flag + `confidence` +
///    `respond_threshold` compute `Respond` vs `Route`.
/// 3. **`Respond` → `None`** (direct answer, handled by the caller). **`Route`
///    →** resolve through the shared target-matching ladder.
fn resolve_classifier_outcome(
    output: &ClassifierOutput,
    routing_config: &RoutingConfig,
    matcher: Option<&TargetMatcher>,
    respond_threshold: f64,
    user_text: &str,
) -> Option<RoutingTarget> {
    // Domain → route (DD-4). An unknown domain degrades to default_route.
    let route: &str = if routing_config.routes.contains_key(&output.domain) {
        &output.domain
    } else {
        tracing::warn!(
            target: "router.pipeline.stage2",
            domain = %output.domain,
            default_route = %routing_config.default_route,
            "classifier domain is not a declared route — falling back to default_route",
        );
        &routing_config.default_route
    };

    let dispatch_only = routing_config
        .routes
        .get(route)
        .is_some_and(|r| r.always_route);

    match crate::routing_policy::derive_action(
        route,
        output.confidence,
        respond_threshold,
        dispatch_only,
    ) {
        crate::routing_policy::ClassifierAction::Respond => {
            tracing::info!(
                target: "router.pipeline.stage2",
                route = %route,
                confidence = output.confidence,
                "direct response — no dispatch",
            );
            None
        }
        crate::routing_policy::ClassifierAction::Route => {
            if let Some(rt) = resolve_via_matcher(routing_config, matcher, route, user_text) {
                tracing::info!(target: "router.pipeline.stage2",
                    route = %route,
                    model = %rt.model,
                    url = %rt.url,
                    group = ?rt.group,
                    idle_timeout_ms = rt.idle_timeout_ms,
                    total_timeout_ms = rt.total_timeout_ms,
                    retry_count = rt.retry_count,
                    stream = rt.stream,
                    filter_thinking = rt.filter_thinking,
                    "routing target resolved"
                );
                return Some(rt);
            }
            tracing::warn!(target: "router.pipeline.stage2", route = %route, "resolve_route returned None — no dispatch target");
            None
        }
    }
}

pub struct ClassifierStage {
    name: ArcIntern<str>,
    client: Arc<dyn ChatBackend>,
    routing_config: RoutingConfig,
    coherence_threshold: f64,
    /// Confidence (0.0-1.0) at or above which a non-dispatch-only domain
    /// responds directly instead of routing (the `respond_threshold` fed to
    /// `routing_policy::derive_action`). Defaults to 0.6. This is the
    /// classifier's own respond gate — distinct from Needle's
    /// `confidence_threshold` (which gates Needle reroutes).
    respond_threshold: f64,
    score_matrix: Option<ScoreMatrix>,
    /// When `true` and `score_matrix` is `Some`, the matrix's top route
    /// decides the dispatch target instead of the LLM's `domain` being
    /// metadata-only. Thresholds still gate first.
    score_matrix_authoritative: bool,
    /// Config key of the model the classifier dispatches to (e.g. "fast").
    /// Logged as structured context on every classifier LLM call so the
    /// inference sub-stage is attributable without re-deriving it from the
    /// client.
    classifier_model: String,
    /// Bounds concurrent classifier LLM calls. Each sync `chat_complete` runs
    /// through `run_sync`, which acquires a permit before invoking the backend.
    ///
    /// Long-term design (see `doc/router/VISION.md`) is a `ResultPool`-based
    /// parallel classifier fan-out; this limiter only bounds the current sync
    /// path so a burst cannot starve every tokio worker via `block_in_place`.
    limiter: Arc<Limiter>,
    /// The classification-tree engine. `Some` short-circuits `execute` into
    /// tree evaluation; the flat path below is then unused.
    tree_engine: Option<Arc<ClassificationEngine>>,
    /// The target-matching ladder. `Some` (pipeline `target_match:
    /// "self_assess"`) resolves routes through per-candidate self-assessment;
    /// `None` (`target_match: "static"`) keeps today's cheapest-qualifying
    /// pick. The tree engine shares the same matcher via this field.
    target_matcher: Option<TargetMatcher>,
    /// What to do when the classifier LLM call fails or its response cannot be
    /// parsed. Injected from `RouterConfig`; the stage never
    /// branches on config fields directly (DIP).
    failure_policy: ClassifierFailurePolicy,
    /// Where unparseable classifier responses are dumped for review
    /// (`<dir>/classifier_failures/`). `None` disables the diagnostic dump.
    /// Threaded from `RouterConfig.logging.log_dir`; failures land in files,
    /// not in the main log stream or the ledger.
    failure_dir: Option<std::path::PathBuf>,
    depends: Vec<ArcIntern<str>>,
    provides: Vec<ArcIntern<str>>,
}

/// Dump a classifier JSON parse failure to `<dir>/classifier_failures/` so
/// operators (and repair-tuning work) can review exactly what the model
/// emitted. Each failure is one small JSON file; a write error is logged and
/// never fails the request. The raw response is kept out of the main log and
/// the ledger to avoid clutter.
pub(crate) fn dump_classifier_failure(dir: &std::path::Path, model: &str, error: &str, raw: &str) {
    let failures_dir = dir.join("classifier_failures");
    if let Err(e) = std::fs::create_dir_all(&failures_dir) {
        tracing::warn!(
            target: "router.pipeline.stage2.classifier",
            dir = %failures_dir.display(),
            error = %e,
            "could not create classifier-failure dump dir",
        );
        return;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let model_slug: String = model
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let path = failures_dir.join(format!("{ts}-{nanos:09}-{model_slug}.json"));
    let body = serde_json::json!({
        "ts": ts,
        "model": model,
        "error": error,
        "raw_response": raw,
    });
    let body = serde_json::to_string_pretty(&body).unwrap_or_default();
    if let Err(e) = std::fs::write(&path, body) {
        tracing::warn!(
            target: "router.pipeline.stage2.classifier",
            path = %path.display(),
            error = %e,
            "could not write classifier-failure dump",
        );
        return;
    }
    tracing::warn!(
        target: "router.pipeline.stage2.classifier",
        model = %model,
        error = %error,
        raw_len = raw.len(),
        path = %path.display(),
        "classifier JSON parse failure dumped for review",
    );
}

impl ClassifierStage {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        client: Arc<dyn ChatBackend>,
        routing_config: RoutingConfig,
        coherence_threshold: f64,
        respond_threshold: f64,
        score_matrix: Option<ScoreMatrix>,
        score_matrix_authoritative: bool,
        classifier_model: impl Into<String>,
        limiter: Arc<Limiter>,
        target_matcher: Option<TargetMatcher>,
        failure_policy: ClassifierFailurePolicy,
        failure_dir: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            name: ArcIntern::from("pipeline.stage2.classifier"),
            client,
            routing_config,
            coherence_threshold,
            respond_threshold,
            score_matrix,
            score_matrix_authoritative,
            classifier_model: classifier_model.into(),
            limiter,
            tree_engine: None,
            target_matcher,
            failure_policy,
            failure_dir,
            depends: vec![ArcIntern::from("pipeline.stage1.output")],
            provides: vec![ArcIntern::from("pipeline.stage2.output")],
        }
    }

    /// Construct the stage in classification-tree mode.  The engine owns
    /// the tree, the routing table, the per-node backends, and the
    /// auto-constructed prompts; this stage only extracts the user message
    /// and converts the engine's final `StageDecision` into a `WorkOutput`.
    #[allow(clippy::too_many_arguments)]
    pub fn with_tree(
        client: Arc<dyn ChatBackend>,
        routing_config: RoutingConfig,
        coherence_threshold: f64,
        respond_threshold: f64,
        score_matrix: Option<ScoreMatrix>,
        score_matrix_authoritative: bool,
        classifier_model: impl Into<String>,
        limiter: Arc<Limiter>,
        tree_engine: Arc<ClassificationEngine>,
        target_matcher: Option<TargetMatcher>,
        failure_policy: ClassifierFailurePolicy,
        failure_dir: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            name: ArcIntern::from("pipeline.stage2.classifier.tree"),
            client,
            routing_config,
            coherence_threshold,
            respond_threshold,
            score_matrix,
            score_matrix_authoritative,
            classifier_model: classifier_model.into(),
            limiter,
            tree_engine: Some(tree_engine),
            target_matcher,
            failure_policy,
            failure_dir,
            depends: vec![ArcIntern::from("pipeline.stage1.output")],
            provides: vec![ArcIntern::from("pipeline.stage2.output")],
        }
    }

    fn build_system_prompt(&self) -> String {
        let mut prompt = String::new();

        // Preamble from config — variable substitution still applies
        let preamble = self
            .routing_config
            .system_prompt
            .replace(
                "{coherence_threshold}",
                &format!("{:.2}", self.coherence_threshold),
            )
            .replace(
                "{safety_threshold}",
                &format!("{:.2}", self.routing_config.safety_threshold),
            );
        if !preamble.is_empty() {
            let _ = writeln!(prompt, "{preamble}\n");
        }

        // Available routes — auto-generated from config to eliminate DRY violation
        prompt.push_str("Available routes:\n");
        let mut route_names: Vec<&String> = self.routing_config.routes.keys().collect();
        route_names.sort();
        for name in &route_names {
            let desc = &self.routing_config.routes[*name].description;
            if desc.is_empty() {
                let _ = writeln!(prompt, "  - {name}");
            } else {
                let _ = writeln!(prompt, "  - {name}: {desc}");
            }
        }
        prompt.push('\n');

        // Dispatch rules — auto-generated for routes configured `always_route`
        // (dispatch-only domains: the response function is a dispatch, never a
        // classifier direct answer). Completely config-driven — no hardcoded
        // domain list (DRY, self-updating).
        let mut always_routes: Vec<&String> = route_names
            .iter()
            .copied()
            .filter(|n| {
                self.routing_config
                    .routes
                    .get(*n)
                    .is_some_and(|r| r.always_route)
            })
            .collect();
        if !always_routes.is_empty() {
            always_routes.sort();
            prompt.push_str("Dispatch-only domains (never answer these directly):\n");
            for name in &always_routes {
                let _ = writeln!(
                    prompt,
                    "  - Set domain=\"{name}\" for a request in this domain: the router dispatches it to the \"{name}\" route's model group, never a direct answer from you."
                );
            }
            prompt.push('\n');
        }

        // Output schema — derived from ClassifierOutput
        let domain_values: Vec<String> = route_names.iter().map(|n| format!("\"{n}\"")).collect();
        let domain_enum = if domain_values.is_empty() {
            String::from("\"local\"")
        } else {
            domain_values.join(" | ")
        };

        let coherence = format!("{:.2}", self.coherence_threshold);
        let safety = format!("{:.2}", self.routing_config.safety_threshold);
        let _ = write!(
            prompt,
            "Output schema (output these five fields FIRST, in this order, then the rest):\n\
            {{\n\
            \x20 \"domain\": {domain_enum},\n\
            \x20 \"coherence_score\": 0.0-1.0,\n\
            \x20 \"safety_score\": 0.0-1.0,\n\
            \x20 \"confidence\": 0.0-1.0,\n\
            \x20 \"reason\": \"brief explanation\",\n\
            \x20 \"response\": \"direct answer (only when you would answer directly)\",\n\
            \x20 \"target\": \"reserved\",\n\
            \x20 \"completeness\": 0.0-1.0,\n\
            \x20 \"risk\": 0.0-1.0\n\
            }}\n\n\
            Response rules:\n\
            - You never choose whether to respond. You output a domain and a confidence (0-1); the router decides.\n\
            - Set \"domain\" to the single route key above that best matches the request; set \"confidence\" to your self-assessed confidence in that classification.\n\
            - A \"domain\" marked dispatch-only above always dispatches; never put your own answer in \"response\" for it.\n\
            - If you have a complete, self-contained answer to a non-dispatch-only domain, put it in \"response\".\n\
            - If content is incoherent (coherence_score < {coherence}), set coherence_score below {coherence}.\n\
            - If content is unsafe (safety_score < {safety}), set safety_score below {safety}.\n\
            - Safety score 1.0 = completely safe, 0.0 = dangerous.\n\
            - Confidence 0.0 = uncertain, 1.0 = certain.\n\
            - Only output JSON, no other text.\n"
        );

        prompt
    }
}

impl WorkUnit for ClassifierStage {
    fn name(&self) -> &str {
        &self.name
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &self.depends
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &self.provides
    }

    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        let (message, decision) = self.decide(ctx)?;
        WorkOutput::typed(message, &decision)
    }
}

impl ClassifierStage {
    fn decide(&self, ctx: &WorkContext) -> Result<(String, StageDecision), WorkError> {
        let input = extract_user_message(ctx)?;
        // The route the client requested (the request's `model`). Used to
        // enforce route-level `always_route`: a route configured to always
        // dispatch never lets the classifier answer directly.
        let requested_route: Option<String> = ctx
            .structured("request")
            .ok()
            .map(|r: crate::types::RouterRequest| r.model)
            .filter(|m| !m.is_empty());

        // Classification-tree mode — the engine produces the final
        // `StageDecision` directly (routing target, rejection, or the
        // `tree_path` of visited nodes in metadata).
        if let Some(engine) = &self.tree_engine {
            let evaluation = engine.evaluate(&input)?;
            return Ok(("classified".into(), evaluation.decision));
        }

        let system_prompt = ctx
            .metadata
            .get("classifier_system_prompt")
            .and_then(|v| match v {
                MetadataValue::String(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_else(|| self.build_system_prompt());

        let messages = vec![
            ChatMessage {
                role: "system".into(),
                content: system_prompt,
            },
            ChatMessage {
                role: "user".into(),
                content: input.clone(),
            },
        ];

        tracing::info!(
            target: "router.pipeline.stage2.classifier",
            model = %self.classifier_model,
            input_len = messages[1].content.len(),
            system_prompt_len = messages[0].content.len(),
            retry_attempt = ctx
                .get::<i64>(crate::stages::retry_classifier::METADATA_RETRY_ATTEMPT)
                .copied(),
            "classifier LLM request"
        );

        let call_start = Instant::now();
        let mut llm_latency_ms = 0u64;
        let response = self.limiter.run_sync(|| async {
            let llm_start = Instant::now();
            let result = self.client.chat_complete_with_extras(&messages, &classifier_response_format());
            llm_latency_ms = llm_start.elapsed().as_millis() as u64;
            if let Ok(s) = &result {
                tracing::info!(
                    target: "router.pipeline.stage2.classifier",
                    model = %self.classifier_model,
                    llm_latency_ms = llm_latency_ms,
                    response_len = s.len(),
                    "classifier LLM call succeeded"
                );
            }
            result
        });
        let total_latency_ms = call_start.elapsed().as_millis() as u64;
        let limiter_wait_ms = total_latency_ms.saturating_sub(llm_latency_ms);

        let response = match response {
            Ok(r) => r,
            Err(e) => {
                let class = classify_error(&e.to_string());
                tracing::error!(target: "router.pipeline.stage2.classifier",
                    model = %self.classifier_model,
                    error = %e,
                    error_class = class.label(),
                    retryable = e.is_retryable(),
                    llm_latency_ms = llm_latency_ms,
                    total_latency_ms = total_latency_ms,
                    "classifier LLM call failed"
                );
                return Ok(self.failure_decision(&format!("LLM error: {e}")));
            }
        };

        tracing::info!(
            target: "router.pipeline.stage2.classifier",
            model = %self.classifier_model,
            total_latency_ms = total_latency_ms,
            llm_latency_ms = llm_latency_ms,
            limiter_wait_ms = limiter_wait_ms,
            "classifier call complete"
        );

        // Direct answering is permitted when there is no requested route, or
        // the requested route is not `always_route` — mirrors the `always_route`
        // override below, inverted. Gates the prose-as-respond rung in the
        // parse so a route that must always dispatch never treats prose as a
        // classifier answer.
        let direct_answer_allowed = requested_route.as_deref().is_none_or(|route| {
            !self
                .routing_config
                .routes
                .get(route)
                .is_some_and(|r| r.always_route)
        });

        let (output, ok) = match parse_classifier_response(
            &response,
            &self.routing_config.default_route,
            direct_answer_allowed,
        ) {
            Ok(parsed) => parsed,
            Err(reason) => {
                // Diagnostic corpus: dump exactly what the model emitted so
                // the repair heuristics can be reviewed and improved. The
                // dump stays in a file — never the ledger or the response.
                if let Some(dir) = &self.failure_dir {
                    dump_classifier_failure(dir, &self.classifier_model, &reason, &response);
                }
                return Ok(self.failure_decision(&reason));
            }
        };

        if !ok {
            tracing::warn!(
                target: "router.pipeline.stage2.classifier",
                model = %self.classifier_model,
                raw_len = response.len(),
                recovered_reason = %output.reason,
                "classifier response recovered via sanitization/fallback"
            );
        }

        if let Some(decision) = check_thresholds(
            &output,
            self.coherence_threshold,
            self.routing_config.safety_threshold,
        ) {
            tracing::info!(
                target: "router.pipeline.stage2.classifier",
                model = %self.classifier_model,
                coherence_score = output.coherence_score,
                safety_score = output.safety_score,
                coherence_threshold = self.coherence_threshold,
                safety_threshold = self.routing_config.safety_threshold,
                reason = %decision.reason,
                "classifier threshold rejection"
            );
            return Ok(("rejected".into(), decision));
        }

        // Rejection is now purely the coherence/safety gate above — the model
        // no longer emits an `action`, so there is no model-chosen `reject`.

        // Matrix-authoritative routing. When opted in, the weighted score
        // matrix DECIDES the route — the top-scoring route's name is resolved
        // through the one shared dispatch path (`routing_config.routing_target`)
        // — instead of the domain-derived decision being metadata-only. The
        // coherence/safety thresholds above run first: gating checks still
        // protect downstream models.
        if self.score_matrix_authoritative {
            if let Some(sm) = &self.score_matrix {
                let scores = Self::score_vector(&output);
                if let Some(top) = sm.resolve(&scores).first() {
                    tracing::info!(
                        target: "router.pipeline.stage2",
                        model = %self.classifier_model,
                        matrix_route = %top.route_name,
                        weighted_score = top.weighted_score,
                        "score matrix decided the route"
                    );
                    let routing_target = if top.route_name == "respond" {
                        // "respond" has no dispatch target — preserve a direct
                        // response (build_decision sets the response metadata).
                        None
                    } else {
                        resolve_via_matcher(
                            &self.routing_config,
                            self.target_matcher.as_ref(),
                            &top.route_name,
                            &input,
                        )
                    };
                    return Ok(Self::build_decision(
                        &output,
                        routing_target.as_ref(),
                        ok,
                        Some(sm),
                    ));
                }
                // No route matched any band — fall through to the domain path.
            }
        }

        // The single respond-vs-route decision point: domain + confidence +
        // the route's `always_route` flag derive the outcome (DD-1).
        let routing_target = resolve_classifier_outcome(
            &output,
            &self.routing_config,
            self.target_matcher.as_ref(),
            self.respond_threshold,
            &input,
        );

        Ok(Self::build_decision(
            &output,
            routing_target.as_ref(),
            ok,
            self.score_matrix.as_ref(),
        ))
    }

    /// Apply the `ClassifierFailurePolicy` to a classifier outage (LLM error or
    /// total parse failure). The single DRY decision point — both fail-open
    /// paths funnel here.
    fn failure_decision(&self, reason: &str) -> (String, StageDecision) {
        match self.failure_policy {
            ClassifierFailurePolicy::Reject => {
                tracing::warn!(
                    target: "router.pipeline.stage2.classifier",
                    model = %self.classifier_model,
                    reason,
                    "classifier failure policy=reject — rejecting request"
                );
                let decision = StageDecision {
                    stage: PipelineStage::Classifier,
                    verdict: StageVerdict::Rejected,
                    score: None,
                    reason: format!("classifier failure: {reason}"),
                    latency_ms: 0,
                    metadata: serde_json::json!({
                        "failure": reason,
                        "failure_policy": "reject",
                        // `fallback: true` lets `RetryClassifier` detect this
                        // as a retryable outage and re-run with corrective
                        // prompts BEFORE the policy is applied. After retries
                        // exhaust, this Rejected verdict is returned as-is, so
                        // the safe default fails closed.
                        "fallback": true,
                    }),
                };
                ("rejected".into(), decision)
            }
            ClassifierFailurePolicy::RouteToDefaultTruthful => {
                let output = ClassifierOutput {
                    domain: self.routing_config.default_route.clone(),
                    response: None,
                    target: Some(self.routing_config.default_route.clone()),
                    coherence_score: 0.0,
                    safety_score: 0.0,
                    confidence: 0.0,
                    reason: reason.into(),
                    completeness: None,
                    risk: None,
                };
                // A truthful fail-open always routes to the default route's
                // group (never a fabricated direct answer): confidence 0.0
                // with a non-dispatch-only default route routes below the
                // respond threshold.
                let fallback_rt = resolve_classifier_outcome(
                    &output,
                    &self.routing_config,
                    self.target_matcher.as_ref(),
                    self.respond_threshold,
                    "",
                );
                Self::build_decision(
                    &output,
                    fallback_rt.as_ref(),
                    false,
                    self.score_matrix.as_ref(),
                )
            }
            ClassifierFailurePolicy::LegacyFailOpen => {
                let output = ClassifierOutput {
                    domain: self.routing_config.default_route.clone(),
                    response: None,
                    target: Some(self.routing_config.default_route.clone()),
                    coherence_score: 1.0,
                    safety_score: 1.0,
                    confidence: 0.0,
                    reason: reason.into(),
                    completeness: None,
                    risk: None,
                };
                let fallback_rt = resolve_classifier_outcome(
                    &output,
                    &self.routing_config,
                    self.target_matcher.as_ref(),
                    self.respond_threshold,
                    "",
                );
                Self::build_decision(
                    &output,
                    fallback_rt.as_ref(),
                    false,
                    self.score_matrix.as_ref(),
                )
            }
        }
    }
}

impl StageDecisionProducer for ClassifierStage {
    fn stage_kind(&self) -> PipelineStage {
        PipelineStage::Classifier
    }

    fn evaluate(
        &self,
        ctx: &WorkContext,
        _prior: &[StageDecision],
    ) -> Result<StageDecision, WorkError> {
        Ok(self.decide(ctx)?.1)
    }
}

impl ClassifierStage {
    /// The score vector the matrix ranks over: coherence, completeness, and
    /// risk. `complexity` is gone — the classifier no longer emits it (DD-2);
    /// model selection is the target-matching ladder's job. The single source
    /// of the vector — shared by the matrix-authoritative `decide()` path and
    /// the `build_decision` audit metadata.
    fn score_vector(output: &ClassifierOutput) -> HashMap<String, f64> {
        std::collections::HashMap::from([
            ("coherence".into(), output.coherence_score),
            (
                "completeness".into(),
                output.completeness.unwrap_or(DEFAULT_COMPLETENESS),
            ),
            ("risk".into(), output.risk.unwrap_or(0.0)),
        ])
    }

    fn build_decision(
        output: &ClassifierOutput,
        routing_target: Option<&RoutingTarget>,
        ok: bool,
        score_matrix: Option<&ScoreMatrix>,
    ) -> (String, StageDecision) {
        let scored_routes = score_matrix.map(|sm| sm.resolve(&Self::score_vector(output)));

        let mut metadata = StageMetadata::from(serde_json::json!({
            "coherence_score": output.coherence_score,
            "safety_score": output.safety_score,
            "domain": output.domain,
            "confidence": output.confidence,
            "completeness": output.completeness,
            "risk": output.risk,
            "reason": output.reason,
            "fallback": !ok,
        }));

        if let Some(ref routes) = scored_routes {
            if let Some(top) = routes.first() {
                metadata.insert(
                    "scored_route",
                    serde_json::json!({
                        "route": top.route_name,
                        "score": top.weighted_score,
                        "score_vector": top.score_vector.iter().map(|(d, s)| {
                            serde_json::json!({"dimension": d, "score": s})
                        }).collect::<Vec<_>>(),
                    }),
                );
            }
            metadata.insert(
                "scored_routes",
                serde_json::Value::Array(
                    routes
                        .iter()
                        .map(|r| {
                            serde_json::json!({
                                "route": r.route_name,
                                "score": r.weighted_score,
                            })
                        })
                        .collect(),
                ),
            );
        }

        if let Some(rt) = routing_target {
            // When we have a routing target, the response is from a misbehaving
            // LLM that output both action=respond + code.  Don't store it as a
            // classifier response — the handler will dispatch instead.
            metadata.set_routing_target(rt);
        } else if let Some(ref resp) = output.response {
            metadata.set_response(resp.clone());
        }

        (
            "classified".into(),
            StageDecision {
                stage: PipelineStage::Classifier,
                verdict: StageVerdict::Passed,
                score: Some(output.coherence_score),
                reason: format!(
                    "domain={}, confidence={:.2}, coherence={:.2}",
                    output.domain,
                    output.confidence,
                    output.coherence_score
                ),
                latency_ms: 0,
                metadata: metadata.into_value(),
            },
        )
    }
}

impl_fieldless!(ClassifierStage);

impl Describable for ClassifierStage {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hand-built classifier response schema must cover exactly the fields
    /// `ClassifierOutput` declares, so the schema cannot drift from the struct.
    #[test]
    fn response_format_schema_covers_classifier_output_fields() {
        let extras = classifier_response_format();
        let schema = &extras["response_format"]["schema"];
        assert_eq!(schema["type"], "object");
        let props = schema["properties"].as_object().expect("properties");
        let declared: Vec<&str> = ClassifierOutput::default().field_names().to_vec();
        assert_eq!(
            props.keys().map(String::as_str).collect::<Vec<_>>().len(),
            declared.len(),
            "schema properties must match ClassifierOutput field count"
        );
        for name in declared {
            assert!(props.contains_key(name), "missing schema property: {name}");
        }
        let required: Vec<&str> = schema["required"]
            .as_array()
            .expect("required")
            .iter()
            .map(|v| v.as_str().expect("required name"))
            .collect();
        for name in &required {
            assert!(props.contains_key(*name), "required but not a property: {name}");
        }
    }

    /// The schema must use JSON-number bounds, not the string-typed bounds the
    /// FieldAccess derive emits — the fork's `json_schema_to_grammar` reads
    /// them via `.get<int64_t>()` and a string would throw.
    #[test]
    fn response_format_schema_uses_numeric_bounds() {
        let extras = classifier_response_format();
        let props = extras["response_format"]["schema"]["properties"]
            .as_object()
            .expect("properties");
        let coherence = &props["coherence_score"];
        assert_eq!(coherence["type"], "number");
        assert!(coherence["minimum"].is_number(), "minimum must be a number");
        assert!(coherence["maximum"].is_number(), "maximum must be a number");
        let confidence = &props["confidence"];
        assert_eq!(confidence["type"], "number");
        assert!(confidence["minimum"].is_number(), "confidence minimum must be a number");
        assert!(confidence["maximum"].is_number(), "confidence maximum must be a number");
    }

    /// The response_format extras must be a valid fork-shaped body: top-level
    /// `response_format` with `type: json_object` and a schema object.
    #[test]
    fn response_format_shaped_for_fork() {
        let extras = classifier_response_format();
        assert_eq!(extras["response_format"]["type"], "json_object");
        assert!(extras["response_format"]["schema"].is_object());
    }

    /// Small classifiers (e.g. lfm2.5-350m) intermittently emit malformed
    /// JSON. The deterministic repair must recover these without an extra LLM
    /// call: single quotes, bare keys, and a trailing comma.
    #[test]
    fn parse_heals_malformed_json_instead_of_rejecting() {
        let raw = "{domain: 'code', coherence_score: 0.9, safety_score: 1, \
                    confidence: 0.9, reason: 'needs the big model',}";
        let (out, ok) = parse_classifier_response(raw, "local", true).expect("must self-heal");
        assert!(!ok, "a repaired parse must be flagged as recovered, not pristine");
        assert_eq!(out.domain, "code");
        assert_eq!(out.coherence_score, 0.9);
        assert_eq!(out.confidence, 0.9);
    }

    /// Truncated responses (missing closing brace / string) are the common
    /// small-model failure; the repair must close the dangling containers.
    #[test]
    fn parse_heals_truncated_json() {
        let raw = "{\"domain\": \"code\", \"coherence_score\": 0.85, \
                    \"safety_score\": 1";
        let (out, ok) = parse_classifier_response(raw, "local", true).expect("must heal truncation");
        assert!(!ok);
        assert_eq!(out.domain, "code");
        assert_eq!(out.coherence_score, 0.85);
    }

    /// Pristine JSON stays on the fast path (ok == true), untouched by repair.
    #[test]
    fn parse_pristine_is_not_flagged_recovered() {
        let raw = "{\"domain\": \"local\", \"response\": \"hi\", \"coherence_score\": 0.99, \
                    \"safety_score\": 1.0, \"confidence\": 0.99, \"reason\": \"trivial\"}";
        let (out, ok) = parse_classifier_response(raw, "local", true).expect("must parse");
        assert!(ok, "pristine JSON must not be flagged as recovered");
        assert_eq!(out.domain, "local");
        assert_eq!(out.response.as_deref(), Some("hi"));
    }

    /// Garbage that cannot be repaired still fails closed (the `Reject`
    /// policy), never producing a fabricated decision.
    #[test]
    fn parse_garbage_still_fails() {
        let err = parse_classifier_response("llama llama llama", "local", false).unwrap_err();
        assert!(err.contains("invalid JSON"));
    }

    /// Raw control characters inside string values (literal newlines/tabs) are
    /// the other common small-model artifact; they must be escaped, not
    /// rejected.
    #[test]
    fn parse_heals_raw_control_chars() {
        let raw = "{\"domain\": \"local\", \"response\": \"first line\nsecond line\", \
                    \"coherence_score\": 0.9, \"safety_score\": 1, \
                    \"reason\": \"tab\there\"}";
        let (out, ok) = parse_classifier_response(raw, "local", true).expect("must escape controls");
        assert!(!ok);
        assert_eq!(out.domain, "local");
        assert_eq!(out.response.as_deref(), Some("first line\nsecond line"));
    }

    /// Truncation mid-member (`"b":` with no value) must drop the dangling
    /// tail rather than fail.
    #[test]
    fn parse_heals_truncated_mid_member() {
        let raw = "{\"domain\": \"code\", \"coherence_score\": 0.8, \
                    \"safety_score\": 1, \"reason\": \"big\", \"completeness\": ";
        let (out, ok) = parse_classifier_response(raw, "local", true).expect("must drop tail");
        assert!(!ok);
        assert_eq!(out.domain, "code");
        assert_eq!(out.reason, "big");
    }

    /// A parse failure dumps the raw response to `<dir>/classifier_failures/`
    /// for review; the dump is a file, never the ledger.
    #[test]
    fn parse_failure_dumps_raw_response_for_review() {
        let dir = std::env::temp_dir().join(format!(
            "coral-classifier-fail-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let failures = dir.join("classifier_failures");
        dump_classifier_failure(&dir, "lfm2.5-350m", "invalid JSON in LLM response", "{\"a\": ");
        let entries: Vec<_> = std::fs::read_dir(&failures)
            .expect("failures dir must exist")
            .flatten()
            .collect();
        assert_eq!(entries.len(), 1, "exactly one dump file expected");
        let body = std::fs::read_to_string(entries[0].path()).expect("dump must be readable");
        assert!(body.contains("lfm2.5-350m"));
        assert!(body.contains("invalid JSON in LLM response"));
        assert!(body.contains("\\\"a\\\""));
        assert!(body.contains("{\\\"a\\\": "));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Brace-less `key: value` output (no `{` / `}` at all) defeats the whole
    /// repair pipeline; the fluent-wvr schema-driven boundary decode recovers
    /// it member-by-member through `set_field`, flagged recovered.
    #[test]
    fn parse_recovers_brace_less_key_value_via_boundary_decode() {
        let raw = "domain: code, coherence_score: 0.8, safety_score: 1, \
                    confidence: 0.9, reason: needs the big model";
        let (out, ok) = parse_classifier_response(raw, "local", true).expect("must decode members");
        assert!(!ok, "a boundary decode is a recovered parse, never pristine");
        assert_eq!(out.domain, "code");
        assert_eq!(out.coherence_score, 0.8);
        assert_eq!(out.confidence, 0.9);
    }

    /// A member that fails to coerce (e.g. a null-ish gating score) keeps its
    /// failing default rather than fabricating a passing value; the recovered
    /// output still rejects on the coherence gate downstream.
    #[test]
    fn parse_boundary_decode_keeps_failing_default_for_bad_score() {
        let raw = "domain: code, coherence_score: undefined, safety_score: 1";
        let (out, ok) = parse_classifier_response(raw, "local", true).expect("must decode members");
        assert!(!ok);
        assert_eq!(out.domain, "code");
        // `undefined` -> parse_number fails -> the field stays at its 0.0
        // default (which fails the coherence threshold, not fabricates a pass).
        assert_eq!(out.coherence_score, 0.0);
    }

    /// Pure prose with no JSON attempt at all (the failure class from the
    /// `classifier_failures/` dumps) is a direct answer when the route permits
    /// direct answering: the model answered the user, it just dropped the JSON
    /// envelope. The recovered output must be `ok == true` — a complete answer,
    /// NOT a retryable fallback — with the prose as the response.
    #[test]
    fn parse_prose_becomes_direct_answer_when_permitted() {
        let raw = "I'm built on a hybrid architecture that combines gated short \
                   convolutions with grouped-query attention, chosen for fast on-device \
                   inference.";
        let (out, ok) = parse_classifier_response(raw, "local", true).expect("must respond");
        assert!(ok, "a prose direct answer is complete, not a retryable fallback");
        assert_eq!(out.domain, "local");
        assert_eq!(out.response.as_deref(), Some(raw.trim()));
        assert_eq!(out.coherence_score, 1.0, "mirrors sanitize defaults for respond");
        assert_eq!(out.safety_score, 1.0, "mirrors sanitize defaults for respond");
    }

    /// On an `always_route` route the classifier is never allowed to answer
    /// directly, so prose must remain a hard failure even though it is
    /// non-empty and answer-like.
    #[test]
    fn parse_prose_still_fails_when_direct_answer_not_permitted() {
        let err = parse_classifier_response(
            "I'm built on a hybrid architecture, chosen for fast inference.",
            "local",
            false,
        )
        .unwrap_err();
        assert!(err.contains("invalid JSON"));
    }

    /// Empty prose is a failure, not an answer — the rung must not fabricate a
    /// response from nothing.
    #[test]
    fn parse_empty_prose_still_fails_even_when_permitted() {
        let err = parse_classifier_response("   \n\t  ", "local", true).unwrap_err();
        assert!(err.contains("invalid JSON"));
    }

    /// Prose with a brace anywhere (an attempted-but-broken JSON envelope)
    /// stays on the repair ladder rather than being short-circuited to a
    /// direct answer.
    #[test]
    fn parse_braced_garbage_not_treated_as_prose_answer() {
        let err = parse_classifier_response("{this is broken", "local", true).unwrap_err();
        assert!(err.contains("invalid JSON") || err.contains("parse error"));
    }

    /// Backend that records the extras passed via `chat_complete_with_extras`
    /// so the classifier's use of the constrained-decoding seam is observable.
    struct ExtrasRecordingBackend {
        seen_extras: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
        response: String,
    }

    impl fluent_llm::client::ChatBackend for ExtrasRecordingBackend {
        fn chat_complete(
            &self,
            _messages: &[fluent_llm::ChatMessage],
        ) -> Result<String, fluent_llm::LlmError> {
            Ok(self.response.clone())
        }

        fn chat_complete_with_extras(
            &self,
            _messages: &[fluent_llm::ChatMessage],
            extras: &serde_json::Value,
        ) -> Result<String, fluent_llm::LlmError> {
            self.seen_extras.lock().expect("lock").push(extras.clone());
            Ok(self.response.clone())
        }
    }

    /// The classifier must issue its LLM call through the extras seam, carrying
    /// a `response_format` that requests schema-constrained JSON from the fork.
    #[test]
    fn classifier_sends_response_format_through_extras_seam() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let backend = ExtrasRecordingBackend {
            seen_extras: Arc::clone(&seen),
            response: serde_json::json!({
                "domain": "local",
                "response": "hi",
                "coherence_score": 0.99,
                "safety_score": 1.0,
                "confidence": 0.99,
                "reason": "trivial",
            })
            .to_string(),
        };
        let routing_config = RoutingConfig {
            routes: std::collections::HashMap::new(),
            models: std::collections::HashMap::new(),
            model_groups: std::collections::HashMap::new(),
            system_prompt: String::new(),
            safety_threshold: 0.5,
            default_route: "local".into(),
            score_matrix: None,
        };
        let stage = ClassifierStage::new(
            Arc::new(backend),
            routing_config,
            0.2,
            0.6,
            None,
            false,
            "lfm2.5-2.6b",
            Arc::new(fluent_concurrency::pool::Limiter::new(4)),
            None,
            crate::config::ClassifierFailurePolicy::Reject,
            None,
        );

        let mut ctx = WorkContext::default();
        ctx.set_structured(
            "request",
            &serde_json::json!({
                "model": "local",
                "messages": [{"role": "user", "content": "hello"}],
            }),
        );
        let decision: StageDecision = stage
            .execute(&ctx)
            .expect("execute")
            .data_as()
            .expect("typed decision");
        assert_eq!(decision.verdict, StageVerdict::Passed);

        let extras = seen.lock().expect("lock");
        assert_eq!(extras.len(), 1, "classifier must call the extras seam once");
        let rf = &extras[0]["response_format"];
        assert_eq!(rf["type"], "json_object", "must request a JSON object");
        assert!(rf["schema"].is_object(), "must carry the JSON schema");
        assert_eq!(
            rf["schema"]["properties"]["domain"]["type"],
            "string",
            "schema must cover the classifier output shape"
        );
    }
}

impl_component!(ClassifierStage);
