//! `NeedlePreFilter` — the Needle pre-classifier rung.
//!
//! Runs between `DeterministicPreFilter` and `Classifier`: it offers the
//! cheapest structured decision on short, action-oriented commands. It is
//! **actionable-only** (roadmap design decision 2): a route tool produces a
//! `Rerouted` verdict with a resolved `RoutingTarget` (the orchestrator
//! short-circuits before the classifier re-decides); every other outcome —
//! gate rejection, decline, engine failure — is a `Skipped` fall-through to
//! the full classifier. Needle never guesses prose.
//!
//! The stage holds two injected seams:
//!
//! - `Arc<dyn NeedleBackend>` — the engine completion seam (production
//!   [`crate::needle::engine::NativeNeedleEngine`], hermetic
//!   [`crate::needle::backend::MockNeedleBackend`]).
//! - `Arc<dyn ToolRetriever>` — the Milestone-5 shortlister, used only when
//!   the tool catalogue overflows `candidates_per_rung` and `shortlist.mode`
//!   is `hnsw`. On overflow without a shortlister the stage falls through —
//!   never a silently truncated tool set (design decision 4).
//!
//! No Needle failure ever hard-errors a request: every error path emits a
//! `Skipped` decision with the reason recorded in `StageMetadata` for the
//! audit trail.

use std::sync::Arc;

use common_core::tokens::estimate_tokens;
use fluent_wvr::prelude::*;

use crate::config::{NeedleConfig, NeedleRouteSchema, NeedleShortlistMode, RoutingConfig};
use crate::needle::backend::NeedleBackend;
use crate::needle::envelope::{NeedleEnvelope, NeedleEnvelopeType};
use crate::needle::retriever::ToolRetriever;
use crate::needle::schema::{
    build_candidate_schemas, is_general_route, overflows_rung, render_tools_json, schema_for,
};
use crate::needle::template::render_output_template;
use crate::pipeline_types::{
    PipelineStage, StageDecision, StageDecisionProducer, StageMetadata, StageVerdict,
};
use crate::stages::common::{extract_user_message, routing_window};

/// Generation bound for a Needle completion. The engine is non-generative (a
/// single grammar-constrained envelope), so a small budget is plenty.
pub(crate) const MAX_NEW_TOKENS: i32 = 256;
/// The `NeedlePreFilter` stage.
pub struct NeedlePreFilter {
    name: ArcIntern<str>,
    backend: Arc<dyn NeedleBackend>,
    retriever: Arc<dyn ToolRetriever>,
    config: NeedleConfig,
    routing: RoutingConfig,
    depends: Vec<ArcIntern<str>>,
    provides: Vec<ArcIntern<str>>,
}

impl NeedlePreFilter {
    /// Build the stage with the identity shortlister (no HNSW index; the
    /// Milestone-5 `HnswToolRetriever` is injected via [`Self::with_retriever`]
    /// when `shortlist.mode` is `hnsw`).
    pub fn new(
        backend: Arc<dyn NeedleBackend>,
        config: NeedleConfig,
        routing: RoutingConfig,
    ) -> Self {
        Self::with_retriever(backend, Arc::new(crate::needle::retriever::IdentityToolRetriever), config, routing)
    }

    /// Build the stage with an explicit shortlister seam.
    pub fn with_retriever(
        backend: Arc<dyn NeedleBackend>,
        retriever: Arc<dyn ToolRetriever>,
        config: NeedleConfig,
        routing: RoutingConfig,
    ) -> Self {
        Self {
            name: ArcIntern::from("pipeline.needle.pre_filter"),
            backend,
            retriever,
            config,
            routing,
            depends: vec![],
            provides: vec![ArcIntern::from("pipeline.needle.output")],
        }
    }

    /// Produce the typed decision for the running request.
    fn decide(&self, ctx: &WorkContext) -> StageDecision {
        let skipped = |reason: String, window: &str| {
            let mut metadata = StageMetadata::default();
            metadata.set_needle_reason(reason.clone());
            metadata.set_needle_window(window);
            StageDecision::new(
                PipelineStage::NeedlePreFilter,
                StageVerdict::Skipped,
                reason,
            )
            .with_metadata(metadata.into_value())
        };

        // ── Engine availability ──
        if !self.backend.is_available() {
            tracing::warn!(target: "router.pipeline", "needle engine unavailable — falling through to classifier");
            return skipped("needle engine unavailable".into(), "");
        }

        // ── Gate: length + input-token budget (over the routing window) ──
        // Needle always decides on the first sentence/paragraph of the prompt
        // (up to ROUTING_WINDOW_MAX_CHARS, whichever is smallest) — a long
        // message can never bury the actionable intent or blow the rung's gate.
        let message = match extract_user_message(ctx) {
            Ok(c) => c,
            Err(e) => return skipped(format!("needle gate: {e}"), ""),
        };
        let command = routing_window(&message);
        let len = command.chars().count();

        if len < self.config.min_command_chars {
            return skipped(
                format!(
                    "needle gate: command too short ({len} < {} chars)",
                    self.config.min_command_chars
                ),
                command,
            );
        }
        if len > self.config.max_command_chars {
            return skipped(
                format!(
                    "needle gate: command too long ({len} > {} chars)",
                    self.config.max_command_chars
                ),
                command,
            );
        }
        let tokens = estimate_tokens(command) as usize;
        if tokens > self.config.max_input_tokens {
            return skipped(
                format!(
                    "needle gate: input too large ({tokens} > {} tokens)",
                    self.config.max_input_tokens
                ),
                command,
            );
        }

        // ── Candidate tool set for the engine ──
        let Some(candidates) = self.candidates_for(command) else {
            return skipped(
                "needle gate: tool catalogue overflows rung without a shortlister".into(),
                command,
            );
        };
        if candidates.is_empty() {
            return skipped("needle gate: no tools available".into(), command);
        }
        let tools_json = render_tools_json(&candidates);

        // ── Completion ──
        let envelope = match self.backend.complete(command, &tools_json, MAX_NEW_TOKENS) {
            Ok(env) => env,
            Err(e) => {
                tracing::warn!(target: "router.pipeline", error = %e, "needle completion failed — falling through to classifier");
                return skipped(format!("needle completion failed: {e}"), command);
            }
        };

        self.verdict_for(&envelope, command)
    }

    /// The rung candidate set for the command. `None` when the catalogue
    /// overflows `candidates_per_rung` and no shortlister can reduce it — the
    /// stage falls through (design decision 4: the degraded path is never a
    /// silently truncated tool set).
    fn candidates_for(&self, command: &str) -> Option<Vec<NeedleRouteSchema>> {
        let retriever = if overflows_rung(&self.config, &self.routing.routes) {
            match self.config.shortlist.mode {
                NeedleShortlistMode::Hnsw => Some(self.retriever.as_ref()),
                NeedleShortlistMode::None => return None,
            }
        } else {
            None
        };
        let candidates =
            build_candidate_schemas(&self.config, &self.routing.routes, retriever, command);
        // Defensive guard: a degraded shortlister passes the full (over-cap)
        // set through — fall through rather than render more tools than the
        // rung cap (design decision 4: never a silently truncated tool set).
        if candidates.len() > self.config.candidates_per_rung {
            tracing::warn!(
                target: "router.pipeline",
                candidates = candidates.len(),
                cap = self.config.candidates_per_rung,
                "needle tool set still overflows after shortlisting — falling through to classifier",
            );
            return None;
        }
        Some(candidates)
    }

    /// Turn a completed envelope into the stage verdict.
    fn verdict_for(&self, envelope: &NeedleEnvelope, window: &str) -> StageDecision {
        let mut metadata = StageMetadata::default();
        metadata.set_needle_window(window);
        if let Some(confidence) = envelope.confidence {
            metadata.set_needle_confidence(confidence);
        }
        if let Some(reasoning) = &envelope.reasoning {
            metadata.insert("needle_reasoning", serde_json::Value::String(reasoning.clone()));
        }

        let declined = |reason: String, metadata: &mut StageMetadata| {
            metadata.set_needle_reason(reason.clone());
            StageDecision::new(
                PipelineStage::NeedlePreFilter,
                StageVerdict::Skipped,
                reason,
            )
            .with_metadata(metadata.clone().into_value())
        };

        // ── Decline paths ──
        if !envelope.is_call() {
            let reason = match envelope.r#type {
                NeedleEnvelopeType::Refuse => "needle declined (refuse)".into(),
                NeedleEnvelopeType::Text => "needle declined (text)".into(),
                NeedleEnvelopeType::Call => "needle declined (empty call)".into(),
            };
            return declined(reason, &mut metadata);
        }
        if let Some(confidence) = envelope.confidence {
            if confidence < self.config.confidence_threshold {
                return declined(
                    format!(
                        "needle declined (confidence {confidence} < threshold {})",
                        self.config.confidence_threshold
                    ),
                    &mut metadata,
                );
            }
        } else if self.config.decline_on_missing_confidence {
            return declined(
                "needle declined (missing confidence, decline_on_missing_confidence)".into(),
                &mut metadata,
            );
        }

        // ── Actionable tool call ──
        let Some(tool) = envelope.single_tool() else {
            return declined("needle declined (multi-tool call)".into(), &mut metadata);
        };
        metadata.set_needle_tool(tool.to_string());

        // ── Direct tool response (output_template) ──
        // When the called tool declares an `output_template` and the invocation
        // is complete (every referenced arg bound), answer directly by
        // rendering the template — no dispatch, no classifier, no extra
        // inference. A template that cannot be fully rendered (a missing arg or
        // a malformed brace) or a tool without a template falls through to the
        // normal route/action logic unchanged: a template only ever enables a
        // direct answer, it never forces one.
        if let Some(template) = schema_for(&self.config, tool).and_then(|s| s.output_template.as_ref())
        {
            let args = envelope.function_calls[0].arguments.as_object();
            if let Some(rendered) =
                args.and_then(|a| render_output_template(template, a))
            {
                metadata.set_needle_response(rendered);
                let reason = format!("needle direct: {tool}");
                metadata.set_needle_reason(reason.clone());
                let mut decision = StageDecision::new(
                    PipelineStage::NeedlePreFilter,
                    StageVerdict::Passed,
                    reason,
                )
                .with_metadata(metadata.into_value());
                if let Some(confidence) = envelope.confidence {
                    decision.score = Some(confidence);
                }
                return decision;
            }
        }

        if self.routing.routes.contains_key(tool) {
            // General category is NOT a Needle decision: a `general` route
            // (e.g. the `local` general Q&A route) falls through to the
            // classifier LLM, which classifies the whole prompt as-is — Needle
            // never short-circuits a category the operator marked general.
            // Defense-in-depth: `schema.rs` already excludes general routes
            // from the engine grammar by construction, so the engine cannot
            // name one in practice — this check exists so the invariant holds
            // even if a caller injects a non-grammar-derived envelope.
            // Non-general route tools keep the authoritative Rerouted
            // short-circuit below.
            if is_general_route(&self.config, tool) {
                return declined(
                    "needle declined (general category — classifier fallback)".into(),
                    &mut metadata,
                );
            }
            // Route tool → Rerouted with the resolved target. The strict
            // `contains_key` guard keeps `resolve_route`'s default-route
            // fallback from silently diverting a named tool (tree-engine
            // pattern); an unresolvable route is a decline, never a diversion.
            let Some(rt) = self.routing.routing_target(tool, None) else {
                return declined(format!("needle route {tool} not resolvable"), &mut metadata);
            };
            metadata.set_needle_reason(format!("needle route: {tool}"));
            metadata.set_routing_target(&rt);
            let mut decision = StageDecision::new(
                PipelineStage::NeedlePreFilter,
                StageVerdict::Rerouted,
                format!("needle route: {tool}"),
            )
            .with_metadata(metadata.into_value());
            if let Some(confidence) = envelope.confidence {
                decision.score = Some(confidence);
            }
            decision
        } else {
            // Action tool → Passed with the call recorded in metadata. Real
            // deterministic execution (TargetWorkUnit / ExecuteFn) is
            // Milestone 6/7; until then the call is recorded and the request
            // continues through the classifier.
            let reason = format!("needle action: {tool}");
            metadata.set_needle_reason(reason.clone());
            metadata.insert(
                "needle_action",
                serde_json::json!({
                    "tool": tool,
                    "arguments": envelope.function_calls[0].arguments.clone(),
                    "executed": false,
                }),
            );
            let mut decision = StageDecision::new(
                PipelineStage::NeedlePreFilter,
                StageVerdict::Passed,
                reason,
            )
            .with_metadata(metadata.into_value());
            if let Some(confidence) = envelope.confidence {
                decision.score = Some(confidence);
            }
            decision
        }
    }
}

impl WorkUnit for NeedlePreFilter {
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
        let decision = self.decide(ctx);
        WorkOutput::typed("needle_pre_filter", &decision)
    }
}

impl StageDecisionProducer for NeedlePreFilter {
    fn stage_kind(&self) -> PipelineStage {
        PipelineStage::NeedlePreFilter
    }

    fn evaluate(
        &self,
        ctx: &WorkContext,
        _prior: &[StageDecision],
    ) -> Result<StageDecision, WorkError> {
        Ok(self.decide(ctx))
    }
}

impl_fieldless!(NeedlePreFilter);

impl Describable for NeedlePreFilter {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }
}

impl_component!(NeedlePreFilter);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{RouteRef, RoutingConfig};
    use crate::needle::backend::MockNeedleBackend;
    use crate::needle::envelope::{NeedleFunctionCall, NeedleEnvelope};
    use crate::types::{RouterMessage, RouterMessageContent, RouterRequest};
    use serde_json::json;

    fn route_ref(group: &str, description: &str) -> RouteRef {
        RouteRef {
            group: group.into(),
            pipelines: vec!["default".into()],
            description: description.into(),
            always_route: false,
        }
    }

    fn routing() -> RoutingConfig {
        let mut routes = std::collections::HashMap::new();
        routes.insert("fast".into(), route_ref("fast", "fast route for simple prompts"));
        routes.insert("local".into(), route_ref("local", "local route"));
        let mut models = std::collections::HashMap::new();
        models.insert(
            "fast_model".into(),
            serde_json::from_value(json!({
                "endpoint": "http://127.0.0.1:8081/v1/chat/completions",
                "name": "fast-model",
                "intelligence": 1,
                "cost_input": 1e-6,
                "cost_output": 6e-6,
                "cost_cached_read": 4e-7,
                "speed": 8,
            }))
            .unwrap(),
        );
        models.insert(
            "local_model".into(),
            serde_json::from_value(json!({
                "endpoint": "http://127.0.0.1:8082/v1/chat/completions",
                "name": "local-model",
                "intelligence": 2,
                "cost_input": 1e-6,
                "cost_output": 6e-6,
                "cost_cached_read": 4e-7,
                "speed": 7,
            }))
            .unwrap(),
        );
        let mut model_groups = std::collections::HashMap::new();
        model_groups.insert("fast".into(), crate::config::ModelGroup::Array(vec!["fast_model".into()]));
        model_groups.insert("local".into(), crate::config::ModelGroup::Array(vec!["local_model".into()]));
        RoutingConfig {
            routes,
            models,
            model_groups,
            system_prompt: String::new(),
            safety_threshold: 0.7,
            default_route: "local".into(),
            score_matrix: None,
        }
    }

    fn call_envelope(tool: &str, confidence: Option<f64>) -> NeedleEnvelope {
        NeedleEnvelope {
            r#type: NeedleEnvelopeType::Call,
            success: None,
            error: None,
            error_code: None,
            function_calls: vec![NeedleFunctionCall {
                name: tool.into(),
                arguments: json!({}),
            }],
            reasoning: None,
            confidence,
            results: None,
        }
    }

    fn ctx_for(command: &str) -> WorkContext {
        let request = RouterRequest {
            model: "test".into(),
            messages: vec![RouterMessage {
                role: "user".into(),
                content: RouterMessageContent::Text(command.into()),
                tool_calls: None,
                tool_call_id: None,
            }],
            temperature: None,
            max_tokens: None,
            stream: None,
            tools: None,
            tool_choice: None,
            session_id: None,
            agent_id: None,
            adapter: None,
            instance: None,
            snapshot: None,
            id_slot: None,
            metadata: Default::default(),
        };
        let mut ctx = WorkContext::default();
        ctx.set_structured("request", &request);
        ctx
    }

    fn stage(backend: Arc<MockNeedleBackend>) -> NeedlePreFilter {
        NeedlePreFilter::new(backend, NeedleConfig::default(), routing())
    }

    #[test]
    fn gate_skips_short_commands() {
        let backend = Arc::new(MockNeedleBackend::always(call_envelope("fast", Some(0.9))));
        let stage = stage(Arc::clone(&backend));
        let decision = stage.decide(&ctx_for("hi"));
        assert_eq!(decision.verdict, StageVerdict::Skipped);
        assert_eq!(backend.calls(), 0, "gate rejects before any completion");
        assert!(decision.reason.contains("too short"));
        let meta = StageMetadata::from(decision.metadata);
        assert!(meta.needle_reason().unwrap_or_default().contains("gate"));
    }

    #[test]
    fn gate_skips_long_commands() {
        let config = NeedleConfig {
            max_command_chars: 16,
            ..NeedleConfig::default()
        };
        let stage = NeedlePreFilter::new(
            Arc::new(MockNeedleBackend::always(call_envelope("fast", Some(0.9)))),
            config,
            routing(),
        );
        let decision = stage.decide(&ctx_for("a command that is far too long to route"));
        assert_eq!(decision.verdict, StageVerdict::Skipped);
        assert!(decision.reason.contains("too long"));
    }

    #[test]
    fn gate_skips_large_input_tokens() {
        let config = NeedleConfig {
            max_input_tokens: 8,
            ..NeedleConfig::default()
        };
        let stage = NeedlePreFilter::new(
            Arc::new(MockNeedleBackend::always(call_envelope("fast", Some(0.9)))),
            config,
            routing(),
        );
        let decision = stage.decide(&ctx_for(
            "this sentence is comfortably longer than eight tokens of input",
        ));
        assert_eq!(decision.verdict, StageVerdict::Skipped);
        assert!(decision.reason.contains("input too large"));
    }

    #[test]
    fn engine_unavailable_skips_without_completion() {
        let backend = Arc::new(MockNeedleBackend::always(call_envelope("fast", Some(0.9))));
        backend.set_available(false);
        let stage = stage(Arc::clone(&backend));
        let decision = stage.decide(&ctx_for("route me to fast"));
        assert_eq!(decision.verdict, StageVerdict::Skipped);
        assert_eq!(backend.calls(), 0, "unavailable engine is never called");
        assert!(decision.reason.contains("unavailable"));
    }

    #[test]
    fn completion_error_skips_cleanly() {
        let backend = Arc::new(MockNeedleBackend::failing());
        let stage = stage(Arc::clone(&backend));
        let decision = stage.decide(&ctx_for("route me to fast"));
        assert_eq!(decision.verdict, StageVerdict::Skipped);
        assert_eq!(backend.calls(), 1);
        assert!(decision.reason.contains("completion failed"));
    }

    #[test]
    fn empty_call_declines() {
        let env = NeedleEnvelope {
            r#type: NeedleEnvelopeType::Call,
            success: None,
            error: None,
            error_code: None,
            function_calls: vec![],
            reasoning: None,
            confidence: Some(0.9),
            results: None,
        };
        let stage = stage(Arc::new(MockNeedleBackend::always(env)));
        let decision = stage.decide(&ctx_for("route me to fast"));
        assert_eq!(decision.verdict, StageVerdict::Skipped);
        assert!(decision.reason.contains("empty call"));
    }

    #[test]
    fn refuse_declines() {
        let env = NeedleEnvelope {
            r#type: NeedleEnvelopeType::Refuse,
            success: None,
            error: None,
            error_code: None,
            function_calls: vec![],
            reasoning: None,
            confidence: None,
            results: None,
        };
        let stage = stage(Arc::new(MockNeedleBackend::always(env)));
        let decision = stage.decide(&ctx_for("route me to fast"));
        assert_eq!(decision.verdict, StageVerdict::Skipped);
        assert!(decision.reason.contains("refuse"));
    }

    #[test]
    fn below_threshold_declines() {
        let stage = stage(Arc::new(MockNeedleBackend::always(call_envelope(
            "fast",
            Some(0.4),
        ))));
        let decision = stage.decide(&ctx_for("route me to fast"));
        assert_eq!(decision.verdict, StageVerdict::Skipped);
        assert!(
            decision.reason.contains("confidence 0.4")
                && decision.reason.contains("threshold"),
            "decline reason must name the confidence and threshold: {}",
            decision.reason
        );
        let meta = StageMetadata::from(decision.metadata);
        assert_eq!(meta.needle_confidence(), Some(0.4));
        assert!(meta.needle_reason().unwrap_or_default().contains("confidence"));
    }

    #[test]
    fn missing_confidence_without_decline_flag_acts_on_call() {
        let stage = stage(Arc::new(MockNeedleBackend::always(call_envelope(
            "fast",
            None,
        ))));
        let decision = stage.decide(&ctx_for("route me to fast"));
        assert_eq!(decision.verdict, StageVerdict::Rerouted);
    }

    #[test]
    fn missing_confidence_with_decline_flag_declines() {
        let config = NeedleConfig {
            decline_on_missing_confidence: true,
            ..NeedleConfig::default()
        };
        let stage = NeedlePreFilter::new(
            Arc::new(MockNeedleBackend::always(call_envelope("fast", None))),
            config,
            routing(),
        );
        let decision = stage.decide(&ctx_for("route me to fast"));
        assert_eq!(decision.verdict, StageVerdict::Skipped);
        assert!(decision.reason.contains("missing confidence"));
    }

    #[test]
    fn route_tool_emits_rerouted_target() {
        let stage = stage(Arc::new(MockNeedleBackend::always(call_envelope(
            "fast",
            Some(0.95),
        ))));
        let decision = stage.decide(&ctx_for("route me to fast"));
        assert_eq!(decision.verdict, StageVerdict::Rerouted);
        assert_eq!(decision.score, Some(0.95));
        let meta = StageMetadata::from(decision.metadata);
        assert_eq!(meta.needle_tool(), Some("fast"));
        assert_eq!(meta.needle_confidence(), Some(0.95));
        let rt = meta.routing_target().expect("routing target");
        assert_eq!(rt.target_name.as_deref(), Some("fast"));
        assert_eq!(rt.group.as_deref(), Some("fast"));
        assert!(!rt.url.is_empty());
    }

    #[test]
    fn multi_tool_call_declines() {
        let env = NeedleEnvelope {
            r#type: NeedleEnvelopeType::Call,
            success: None,
            error: None,
            error_code: None,
            function_calls: vec![
                NeedleFunctionCall {
                    name: "fast".into(),
                    arguments: json!({}),
                },
                NeedleFunctionCall {
                    name: "local".into(),
                    arguments: json!({}),
                },
            ],
            reasoning: None,
            confidence: Some(0.9),
            results: None,
        };
        let stage = stage(Arc::new(MockNeedleBackend::always(env)));
        let decision = stage.decide(&ctx_for("route me to fast"));
        assert_eq!(decision.verdict, StageVerdict::Skipped);
        assert!(decision.reason.contains("multi-tool"));
    }

    #[test]
    fn unknown_tool_is_action_passed() {
        let stage = stage(Arc::new(MockNeedleBackend::always(call_envelope(
            "calc",
            Some(0.9),
        ))));
        let decision = stage.decide(&ctx_for("compute 2 plus 2"));
        assert_eq!(decision.verdict, StageVerdict::Passed);
        let meta = StageMetadata::from(decision.metadata);
        assert_eq!(meta.needle_tool(), Some("calc"));
        assert_eq!(meta.needle_reason(), Some("needle action: calc"));
        assert_eq!(
            meta.as_value()["needle_action"]["tool"],
            "calc",
            "the action call is recorded for Milestone 6/7 execution"
        );
        assert_eq!(meta.as_value()["needle_action"]["executed"], false);
    }

    #[test]
    fn overflow_without_shortlister_falls_through() {
        // 10 routes overflow candidates_per_rung (5); with shortlist.mode None
        // the stage must fall through, never truncate the tool set.
        let mut routing = routing();
        for i in 0..8 {
            routing
                .routes
                .insert(format!("extra{i}"), route_ref(&format!("extra{i}"), "extra route"));
        }
        let backend: Arc<MockNeedleBackend> =
            Arc::new(MockNeedleBackend::always(call_envelope("fast", Some(0.9))));
        let stage = NeedlePreFilter::new(backend.clone(), NeedleConfig::default(), routing);
        let decision = stage.decide(&ctx_for("route me to fast"));
        assert_eq!(decision.verdict, StageVerdict::Skipped);
        assert!(decision.reason.contains("overflows rung"));
        assert_eq!(backend.calls(), 0, "no completion on fall-through");
    }

    fn template_config() -> NeedleConfig {
        let mut config = NeedleConfig::default();
        config.schema_overrides.insert(
            "fast".into(),
            NeedleRouteSchema {
                name: "fast".into(),
                description: "fast route".into(),
                examples: vec![],
                parameters: json!({"type": "object", "properties": {}}),
                intents: vec![],
                output_template: Some("Answer: {value}".into()),
                general: false,
            },
        );
        config
    }

    #[test]
    fn template_tool_produces_direct_response() {
        let mut env = call_envelope("fast", Some(0.9));
        env.function_calls[0].arguments = json!({"value": "42"});
        let stage = NeedlePreFilter::new(
            Arc::new(MockNeedleBackend::always(env)),
            template_config(),
            routing(),
        );
        let decision = stage.decide(&ctx_for("route me to fast"));
        assert_eq!(decision.verdict, StageVerdict::Passed);
        assert_eq!(decision.score, Some(0.9));
        let meta = StageMetadata::from(decision.metadata);
        assert_eq!(meta.needle_response(), Some("Answer: 42"));
        assert_eq!(meta.needle_reason(), Some("needle direct: fast"));
        assert_eq!(meta.needle_tool(), Some("fast"));
        assert_eq!(meta.needle_confidence(), Some(0.9));
    }

    #[test]
    fn template_tool_missing_arg_falls_through_to_route() {
        // `{}` args leave `{value}` unbound → the template cannot render → the
        // route path (fast is a route key) runs instead, so it still reroutes.
        let stage = NeedlePreFilter::new(
            Arc::new(MockNeedleBackend::always(call_envelope("fast", Some(0.9)))),
            template_config(),
            routing(),
        );
        let decision = stage.decide(&ctx_for("route me to fast"));
        assert_eq!(decision.verdict, StageVerdict::Rerouted);
        let meta = StageMetadata::from(decision.metadata);
        assert!(meta.needle_response().is_none());
        assert_eq!(meta.needle_reason(), Some("needle route: fast"));
    }

    #[test]
    fn tool_without_template_is_unchanged() {
        // "local" is a route key with no template override — even with the
        // template config present, a call to it still reroutes.
        let stage = NeedlePreFilter::new(
            Arc::new(MockNeedleBackend::always(call_envelope("local", Some(0.9)))),
            template_config(),
            routing(),
        );
        let decision = stage.decide(&ctx_for("route me to local"));
        assert_eq!(decision.verdict, StageVerdict::Rerouted);
        let meta = StageMetadata::from(decision.metadata);
        assert!(meta.needle_response().is_none());
    }

    fn general_config() -> NeedleConfig {
        let mut config = NeedleConfig::default();
        config.schema_overrides.insert(
            "local".into(),
            NeedleRouteSchema {
                name: "local".into(),
                description: "local general route".into(),
                examples: vec![],
                parameters: json!({"type": "object", "properties": {}}),
                intents: vec!["general".into()],
                output_template: None,
                general: true,
            },
        );
        config
    }

    #[test]
    fn general_route_tool_falls_through_to_classifier() {
        // A `general` route tool (the `local` general Q&A category) is NOT a
        // Needle decision: the call declines (Skipped) so the classifier LLM
        // classifies the whole prompt as-is — no Rerouted short-circuit.
        let stage = NeedlePreFilter::new(
            Arc::new(MockNeedleBackend::always(call_envelope("local", Some(0.95)))),
            general_config(),
            routing(),
        );
        let decision = stage.decide(&ctx_for("route me to local"));
        assert_eq!(decision.verdict, StageVerdict::Skipped);
        let meta = StageMetadata::from(decision.metadata);
        assert_eq!(meta.needle_tool(), Some("local"));
        assert_eq!(
            meta.needle_reason(),
            Some("needle declined (general category — classifier fallback)")
        );
        assert!(meta.routing_target().is_none(), "no routing target for a general fallback");
    }

    #[test]
    fn general_route_with_template_still_answers_directly() {
        // A template on a general route still answers directly (a complete
        // direct answer beats falling back to the classifier).
        let mut config = general_config();
        config
            .schema_overrides
            .get_mut("local")
            .unwrap()
            .output_template = Some("Direct: {value}".into());
        let mut env = call_envelope("local", Some(0.95));
        env.function_calls[0].arguments = json!({"value": "42"});
        let stage = NeedlePreFilter::new(
            Arc::new(MockNeedleBackend::always(env)),
            config,
            routing(),
        );
        let decision = stage.decide(&ctx_for("route me to local"));
        assert_eq!(decision.verdict, StageVerdict::Passed);
        let meta = StageMetadata::from(decision.metadata);
        assert_eq!(meta.needle_response(), Some("Direct: 42"));
    }

    #[test]
    fn non_general_route_tool_still_reroutes() {
        // "fast" is a route key with no `general` marker — its call keeps the
        // authoritative Rerouted short-circuit even when other routes are
        // marked general.
        let mut config = general_config();
        config.schema_overrides.insert(
            "fast".into(),
            NeedleRouteSchema {
                name: "fast".into(),
                description: "fast route".into(),
                examples: vec![],
                parameters: json!({"type": "object", "properties": {}}),
                intents: vec![],
                output_template: None,
                general: false,
            },
        );
        let stage = NeedlePreFilter::new(
            Arc::new(MockNeedleBackend::always(call_envelope("fast", Some(0.95)))),
            config,
            routing(),
        );
        let decision = stage.decide(&ctx_for("route me to fast"));
        assert_eq!(decision.verdict, StageVerdict::Rerouted);
        let meta = StageMetadata::from(decision.metadata);
        assert_eq!(meta.routing_target().expect("routing target").target_name.as_deref(), Some("fast"));
    }

    #[test]
    fn overflow_with_degraded_shortlister_falls_through() {
        // A shortlister that cannot reduce the set (pass-all) must still fall
        // through — the stage never renders more tools than the rung cap.
        #[derive(Debug, Default)]
        struct PassAllRetriever;
        impl crate::needle::retriever::ToolRetriever for PassAllRetriever {
            fn shortlist(
                &self,
                _query: &str,
                candidates: &[NeedleRouteSchema],
                _k: usize,
            ) -> Vec<NeedleRouteSchema> {
                candidates.to_vec()
            }
        }

        let mut routing = routing();
        for i in 0..8 {
            routing
                .routes
                .insert(format!("extra{i}"), route_ref(&format!("extra{i}"), "extra route"));
        }
        let config = NeedleConfig {
            shortlist: crate::config::NeedleShortlistConfig {
                mode: NeedleShortlistMode::Hnsw,
                ..crate::config::NeedleShortlistConfig::default()
            },
            ..NeedleConfig::default()
        };
        let backend: Arc<MockNeedleBackend> =
            Arc::new(MockNeedleBackend::always(call_envelope("fast", Some(0.9))));
        let stage = NeedlePreFilter::with_retriever(
            backend.clone(),
            Arc::new(PassAllRetriever),
            config,
            routing,
        );
        let decision = stage.decide(&ctx_for("route me to fast"));
        assert_eq!(decision.verdict, StageVerdict::Skipped);
        assert!(decision.reason.contains("overflows rung"));
        assert_eq!(backend.calls(), 0, "no completion on fall-through");
    }
}