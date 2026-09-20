//! The classification-tree engine walk: evaluates a [`ClassificationTree`]
//! recursively (filter / classifier / terminal / fallback nodes) and produces
//! the final classifier `StageDecision`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use fluent_concurrency::pool::Limiter;
use fluent_llm::client::ChatBackend;
use fluent_llm::ChatMessage;
use fluent_wvr::prelude::*;

use crate::config::compiled_tree::{CompiledChild, CompiledFilter, CompiledNode, CompiledTree};
use crate::config::filters::FilterOutcome;
use crate::config::RoutingConfig;
use crate::pipeline::RoutingTarget;
use crate::pipeline_types::{StageDecision, StageVerdict};
use crate::stages::classifier::ClassifierBackendResolver;
use crate::target_match::{AssessmentRecord, RoleExpansion, TargetMatcher};

use super::decisions::{
    fallback_child, final_decision, node_decision, terminal_decision, TreeEvaluation, TreeOutcome,
};
use super::verdict::{parse_tree_verdict, TreeClassifierVerdict};

pub struct ClassificationEngine {
    tree: Arc<CompiledTree>,
    routing: RoutingConfig,
    /// Backend used for every classifier node that has no dedicated entry in
    /// `clients` (mock/transcript injection, or the root classifier client).
    default_client: Arc<dyn ChatBackend>,
    /// Per-node backends keyed by `classifier` node `model` key (real mode
    /// only), so a sub-classifier on a different model dispatches to its own
    /// endpoint.
    clients: HashMap<String, Arc<dyn ChatBackend>>,
    /// Late-bound backend resolution shared with the owning classifier stage.
    /// `Some` re-resolves each node key per request; a miss falls back to
    /// the boot-built `clients` map and `default_client` unchanged. `None`
    /// (injected mock path) serves the boot-built backends exactly as before.
    backend_resolver: Option<ClassifierBackendResolver>,
    /// Bounds concurrent classifier LLM calls (same primitive as the flat
    /// stage).
    limiter: Arc<Limiter>,
    /// Coherence threshold for classifier nodes that don't set their own.
    default_coherence_threshold: f64,
    /// The target-matching ladder, shared with the flat classifier path
    /// (DRY — one climbing implementation). `Some` (pipeline `target_match:
    /// "self_assess"`) resolves terminals through per-candidate
    /// self-assessment for 2+ member groups; singleton groups flow through
    /// with zero assessment calls (expansion pick preserved); `None` keeps
    /// the static cheapest-qualifying pick.
    target_matcher: Option<TargetMatcher>,
}

impl ClassificationEngine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tree: Arc<CompiledTree>,
        routing: RoutingConfig,
        default_client: Arc<dyn ChatBackend>,
        clients: HashMap<String, Arc<dyn ChatBackend>>,
        limiter: Arc<Limiter>,
        default_coherence_threshold: f64,
        target_matcher: Option<TargetMatcher>,
        backend_resolver: Option<ClassifierBackendResolver>,
    ) -> Self {
        Self {
            tree,
            routing,
            default_client,
            clients,
            backend_resolver,
            limiter,
            default_coherence_threshold,
            target_matcher,
        }
    }

    /// Evaluate the whole tree against the user message and produce the final
    /// classifier `StageDecision`. `interlingua` is the `NlpStage` handoff
    /// (ROADMAP §14.6, C6): when present, `Filter` nodes with
    /// `match_interlingua` dispatch deterministically on the parse's ids.
    /// `route_hints` is the overlay stage's handoff (ROADMAP_20260827_ORT
    /// §2.6): scored route recommendations appended to classifier-node LLM
    /// context as deterministic routing context.
    pub fn evaluate(
        &self,
        user_text: &str,
        interlingua: Option<&[spacy_rs::routing::InterlinguaSignal]>,
        route_hints: Option<&[crate::pipeline_types::RouteHint]>,
    ) -> Result<TreeEvaluation, WorkError> {
        self.evaluate_with_expansion(
            user_text,
            interlingua,
            route_hints,
            &RoleExpansion::default(),
        )
    }

    /// [`Self::evaluate`] with the request's availability view for
    /// group-member sentinel expansion. Callers on the serving path (which
    /// carry a request context) pass the context-derived expansion; all other
    /// callers use `evaluate` and get unexpanded behavior.
    pub fn evaluate_with_expansion(
        &self,
        user_text: &str,
        interlingua: Option<&[spacy_rs::routing::InterlinguaSignal]>,
        route_hints: Option<&[crate::pipeline_types::RouteHint]>,
        expansion: &RoleExpansion,
    ) -> Result<TreeEvaluation, WorkError> {
        let mut visited: Vec<StageDecision> = Vec::new();
        let siblings = HashMap::new();
        let outcome = self.evaluate_node(
            &self.tree.root,
            &siblings,
            user_text,
            interlingua,
            route_hints,
            None,
            &mut visited,
            expansion,
        )?;

        for d in &visited {
            crate::audit::emit(
                "tree_node",
                serde_json::json!({
                    "node_type": d.metadata.get("node_type"),
                    "verdict": format!("{:?}", d.verdict),
                    "reason": d.reason,
                }),
            );
        }

        let handoff = final_decision(outcome, visited);
        Ok(TreeEvaluation {
            decision: handoff.decision,
            target: handoff.target,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate_node(
        &self,
        node: &CompiledNode,
        siblings: &HashMap<String, CompiledNode>,
        user_text: &str,
        interlingua: Option<&[spacy_rs::routing::InterlinguaSignal]>,
        route_hints: Option<&[crate::pipeline_types::RouteHint]>,
        complexity: Option<u8>,
        visited: &mut Vec<StageDecision>,
        expansion: &RoleExpansion,
    ) -> Result<TreeOutcome, WorkError> {
        match node {
            CompiledNode::Classifier {
                description,
                model,
                coherence_threshold,
                safety_threshold,
                children,
                ..
            } => {
                // The node's duty key: its literal model, else the pipeline's
                // resolved classifier client serves (unknown keys behave as
                // before — `call_classifier` falls back to the default).
                // Availability sentinels never reach a backend literally:
                // they resolve per request through viable-last (global
                // recency when fittable without a cold load, else the
                // fleet-viable member). The sync stage cannot host async
                // `ensure_target_ready`, so readiness is by construction —
                // the resolver prefers running models and the live registry
                // observes post-build registrations; dispatch-time ensure
                // remains the hard guarantee. Empty fleets keep the literal
                // (the default-client path serves as before).
                let model_key: String = model.clone().unwrap_or_default();
                let model_key: String = if model_key == "last" || model_key == "any" {
                    crate::target_match::viable_last_key(
                        &self.routing,
                        expansion.recency(),
                        &|base| expansion.supervisor_loaded(base),
                    )
                    .unwrap_or(model_key)
                } else {
                    model_key
                };
                self.evaluate_classifier(
                    description,
                    &model_key,
                    *coherence_threshold,
                    *safety_threshold,
                    children,
                    node,
                    user_text,
                    interlingua,
                    route_hints,
                    visited,
                    expansion,
                )
            }
            CompiledNode::Terminal {
                route,
                role,
                description,
                ..
            } => Ok(self.evaluate_terminal(
                route,
                role.as_deref(),
                description,
                complexity,
                user_text,
                visited,
                expansion,
            )),
            CompiledNode::Filter(filter) => self.evaluate_filter(
                filter,
                siblings,
                user_text,
                interlingua,
                route_hints,
                complexity,
                visited,
                expansion,
            ),
            CompiledNode::Fallback { node, .. } => self.evaluate_node(
                node,
                siblings,
                user_text,
                interlingua,
                route_hints,
                complexity,
                visited,
                expansion,
            ),
        }
    }

    /// The last-resort target for a classifier node whose verdict names no
    /// child (or whose LLM call failed): the explicit `fallback` child when
    /// present, else the child serving the configured default route. The
    /// flag is true for the default-route path (audit wording differs).
    /// `None` when neither exists — the caller rejects as before.
    fn fallback_target<'a>(
        &self,
        children: &'a [CompiledChild],
    ) -> Option<(&'a CompiledChild, bool)> {
        if let Some(fb) = fallback_child(children) {
            return Some((fb, false));
        }
        let default = self.routing.default_route.as_str();
        if default.is_empty() {
            return None;
        }
        children
            .iter()
            .find(|c| {
                c.key == default
                    || matches!(&c.node, CompiledNode::Terminal { route, .. } if route == default)
            })
            .map(|c| (c, true))
    }

    /// Classifier node: run deterministic filter children first (short-circuit),
    /// then an LLM call over the auto-built prompt, thresholds, then pick a child.
    #[allow(clippy::too_many_arguments)]
    fn evaluate_classifier(
        &self,
        description: &str,
        model: &str,
        coherence_threshold: Option<f64>,
        safety_threshold: Option<f64>,
        children: &[CompiledChild],
        node: &CompiledNode,
        user_text: &str,
        interlingua: Option<&[spacy_rs::routing::InterlinguaSignal]>,
        route_hints: Option<&[crate::pipeline_types::RouteHint]>,
        visited: &mut Vec<StageDecision>,
        expansion: &RoleExpansion,
    ) -> Result<TreeOutcome, WorkError> {
        // Deterministic filter children short-circuit before any LLM call.
        let siblings: HashMap<String, CompiledNode> = children
            .iter()
            .map(|c| (c.key.clone(), c.node.clone()))
            .collect();
        for child in children {
            if let CompiledNode::Filter { .. } = child.node {
                match self.evaluate_node(
                    &child.node,
                    &siblings,
                    user_text,
                    interlingua,
                    route_hints,
                    None,
                    visited,
                    expansion,
                )? {
                    TreeOutcome::Pass => {}
                    other => return Ok(other),
                }
            }
        }

        let coherence = coherence_threshold.unwrap_or(self.default_coherence_threshold);
        let safety = safety_threshold.unwrap_or(self.routing.safety_threshold);

        let Some(prompt) = node.render_classifier_prompt(coherence, safety) else {
            let reason = "classifier node has no routeable children".to_string();
            visited.push(node_decision(
                "classifier",
                description,
                StageVerdict::Rejected,
                reason.clone(),
                serde_json::json!({ "model": model }),
            ));
            return Ok(TreeOutcome::Reject(reason));
        };

        let verdict = match self.call_classifier(model, &prompt, user_text, route_hints) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    target: "router.pipeline.stage2.tree",
                    model = %model,
                    error = %e,
                    "tree classifier LLM call failed — falling to fallback/reject",
                );
                if let Some((fb, is_default)) = self.fallback_target(children) {
                    visited.push(node_decision(
                        "classifier",
                        description,
                        StageVerdict::Rerouted,
                        if is_default {
                            format!(
                                "classifier LLM error, no fallback child — using default route '{}': {e}",
                                fb.key
                            )
                        } else {
                            format!("classifier LLM error, falling back: {e}")
                        },
                        serde_json::json!({ "model": model, "route": fb.key }),
                    ));
                    return self.evaluate_node(
                        &fb.node,
                        &siblings,
                        user_text,
                        interlingua,
                        route_hints,
                        None,
                        visited,
                        expansion,
                    );
                }
                let reason = format!("classifier LLM error: {e}");
                visited.push(node_decision(
                    "classifier",
                    description,
                    StageVerdict::Rejected,
                    reason.clone(),
                    serde_json::json!({ "model": model }),
                ));
                return Ok(TreeOutcome::Reject(reason));
            }
        };

        // Three-axis gating: incoherent / unsafe queries are rejected before
        // any child is picked (VISION §"Three axes of routing").
        if verdict.coherence < coherence || verdict.safety < safety {
            let reason = if verdict.coherence < coherence {
                format!(
                    "rejected: coherence {:.2} below threshold {:.2}",
                    verdict.coherence, coherence
                )
            } else {
                format!(
                    "rejected: safety {:.2} below threshold {:.2}",
                    verdict.safety, safety
                )
            };
            visited.push(node_decision(
                "classifier",
                description,
                StageVerdict::Rejected,
                reason.clone(),
                serde_json::json!({
                    "model": model,
                    "coherence": verdict.coherence,
                    "safety": verdict.safety,
                    "reason": verdict.reason,
                }),
            ));
            return Ok(TreeOutcome::Reject(reason));
        }

        let route = verdict.route.as_deref().filter(|r| !r.is_empty());
        if let Some(route) = route {
            if let Some(child) = children.iter().find(|c| c.key == route) {
                visited.push(node_decision(
                    "classifier",
                    description,
                    StageVerdict::Rerouted,
                    format!("picked child '{route}'"),
                    serde_json::json!({
                        "model": model,
                        "route": route,
                        "coherence": verdict.coherence,
                        "safety": verdict.safety,
                        "complexity": verdict.complexity,
                        "reason": verdict.reason,
                    }),
                ));
                return self.evaluate_node(
                    &child.node,
                    &siblings,
                    user_text,
                    interlingua,
                    route_hints,
                    Some(verdict.complexity),
                    visited,
                    expansion,
                );
            }
        }

        // No named child picked (or unknown name): fall back if configured,
        // else serve the child behind the configured default route.
        if let Some((fb, is_default)) = self.fallback_target(children) {
            visited.push(node_decision(
                "classifier",
                description,
                StageVerdict::Rerouted,
                if is_default {
                    format!(
                        "no named child picked (route {:?}) — no fallback child, using default route '{}'",
                        verdict.route, fb.key
                    )
                } else {
                    format!(
                        "no named child picked (route {:?}) — falling back",
                        verdict.route
                    )
                },
                serde_json::json!({
                    "model": model,
                    "route": verdict.route,
                    "coherence": verdict.coherence,
                    "safety": verdict.safety,
                    "complexity": verdict.complexity,
                    "reason": verdict.reason,
                }),
            ));
            return self.evaluate_node(
                &fb.node,
                &siblings,
                user_text,
                interlingua,
                route_hints,
                Some(verdict.complexity),
                visited,
                expansion,
            );
        }

        let reason = "classifier picked no valid child and neither a fallback child nor the default route is configured"
            .to_string();
        visited.push(node_decision(
            "classifier",
            description,
            StageVerdict::Rejected,
            reason.clone(),
            serde_json::json!({
                "model": model,
                "route": verdict.route,
                "coherence": verdict.coherence,
                "safety": verdict.safety,
                "reason": verdict.reason,
            }),
        ));
        Ok(TreeOutcome::Reject(reason))
    }

    /// Terminal node: resolve a dispatch target. When the pipeline opts into
    /// target-matching (`target_match: "self_assess"`), the role resolves
    /// through the shared ladder (`crate::target_match::TargetMatcher`) —
    /// with per-candidate self-assessment for 2+ member groups, assessment-
    /// free for singletons; otherwise the static cheapest-qualifying pick
    /// runs (`RoutingConfig::routing_target` /
    /// [`Self::resolve_role_target`]).
    fn evaluate_terminal(
        &self,
        route: &str,
        role: Option<&str>,
        description: &str,
        complexity: Option<u8>,
        user_text: &str,
        visited: &mut Vec<StageDecision>,
        expansion: &RoleExpansion,
    ) -> TreeOutcome {
        // Strict resolution: a terminal names an explicit route. Unknown route
        // names must not silently divert to the default route — `resolve_route`
        // would fall back; check the flat map first.
        if self.routing.routes.contains_key(route) {
            if let Some((rt, assessments)) =
                self.resolve_route_with_matcher(route, complexity, user_text, expansion)
            {
                visited.push(terminal_decision(description, &rt, complexity, assessments));
                return TreeOutcome::Route(Box::new(rt));
            }
        }

        // The terminal carries its own `role`: resolve directly through the
        // role's models (the flat routes map has no entry for this route).
        if let Some(role) = role {
            if let Some((rt, assessments)) =
                self.resolve_role_target(route, role, complexity, user_text, expansion)
            {
                visited.push(terminal_decision(description, &rt, complexity, assessments));
                return TreeOutcome::Route(Box::new(rt));
            }
        }

        let reason = format!("terminal route not resolvable: {route}");
        visited.push(node_decision(
            "terminal",
            description,
            StageVerdict::Rejected,
            reason.clone(),
            serde_json::json!({ "route": route }),
        ));
        TreeOutcome::Reject(reason)
    }

    /// Flat-route terminal resolution through the shared target-matching
    /// ladder when available, falling back to the static
    /// `RoutingConfig::routing_target` (defense-in-depth — never fails harder
    /// than today). Singleton roles run the ladder too: with one candidate
    /// no assessment call fires, but the expansion's residency-aware pick
    /// still flows into the primary (the static rule is residency-blind).
    fn resolve_route_with_matcher(
        &self,
        route: &str,
        complexity: Option<u8>,
        user_text: &str,
        expansion: &RoleExpansion,
    ) -> Option<(RoutingTarget, Option<Vec<AssessmentRecord>>)> {
        if let Some(matcher) = &self.target_matcher {
            if let Some(role) = self.routing.route_role(route) {
                let candidates = crate::target_match::expanded_candidates_for_role(
                    &self.routing,
                    role,
                    expansion.recency(),
                    expansion.session(),
                    &|base| expansion.supervisor_loaded(base),
                    complexity,
                );
                if let Some(tm) = matcher.match_target(
                    route,
                    role,
                    &self.routing,
                    &candidates,
                    complexity,
                    user_text,
                ) {
                    // `matched_via: self_assess` marks terminals an assessment actually
                    // ran for: singletons flow through the ladder with zero
                    // assessments (expansion pick, no calls), so they report
                    // no marker.
                    let assessed = Some(tm.assessments).filter(|a| !a.is_empty());
                    return Some((tm.primary, assessed));
                }
            }
        }
        self.routing
            .routing_target(route, complexity)
            .map(|rt| (rt, None))
    }

    /// Resolve a terminal's own role when no flat `routes` entry
    /// exists. When the target-matching ladder is available, runs the
    /// self-assessment climb for any non-empty role; otherwise picks the
    /// viable model in the role (intelligence-passing, loaded-first,
    /// cheapest — the shared static rule). Singleton roles climb with zero
    /// assessment calls (nothing to veto) — the ladder still runs so the
    /// expansion's residency-aware pick flows into the primary.
    fn resolve_role_target(
        &self,
        route: &str,
        role: &str,
        min_complexity: Option<u8>,
        user_text: &str,
        expansion: &RoleExpansion,
    ) -> Option<(RoutingTarget, Option<Vec<AssessmentRecord>>)> {
        if let Some(matcher) = &self.target_matcher {
            let candidates = crate::target_match::expanded_candidates_for_role(
                &self.routing,
                role,
                expansion.recency(),
                expansion.session(),
                &|base| expansion.supervisor_loaded(base),
                min_complexity,
            );
            if let Some(tm) = matcher.match_target(
                route,
                role,
                &self.routing,
                &candidates,
                min_complexity,
                user_text,
            ) {
                // `matched_via: self_assess` marks terminals an assessment actually
                    // ran for: singletons flow through the ladder with zero
                    // assessments (expansion pick, no calls), so they report
                    // no marker.
                    let assessed = Some(tm.assessments).filter(|a| !a.is_empty());
                    return Some((tm.primary, assessed));
            }
        }

        // Static fallback through the shared viability rule (no
        // recency/liveness here, so residency input is `None` — identical to
        // the flat static path; unknown literals fall out through the entry
        // lookup, as today).
        let model_keys: Vec<String> = self.routing.role_dispatch_members(role);
        let entry_key = crate::config::select_viable_member(
            role,
            &model_keys,
            &self.routing.models,
            min_complexity,
            None,
        )?;
        let mut rt = self.routing.target_for_key(&entry_key)?;
        // Same role-window rule as every other dispatch path: the target
        // names `<base>:<role>` (opened on demand when unbound).
        self.routing.apply_terminal_role(&mut rt, &entry_key, role);
        rt.target_name = Some(route.to_string());
        Some((rt, None))
    }

    /// Filter node: deterministic short-circuit over the user message. With
    /// `match_interlingua` set, dispatches on the request's parsed ids instead
    /// of regexes (ROADMAP §14.6, C6) — same phrasing → same ids → same route,
    /// zero tokens. When `interlingua` is `None` (NLP stage absent) an
    /// interlingua filter is a non-match → Pass (graceful degradation,
    /// identical to an invalid regex).
    #[allow(clippy::too_many_arguments)]
    fn evaluate_filter(
        &self,
        filter: &CompiledFilter,
        siblings: &HashMap<String, CompiledNode>,
        user_text: &str,
        interlingua: Option<&[spacy_rs::routing::InterlinguaSignal]>,
        route_hints: Option<&[crate::pipeline_types::RouteHint]>,
        complexity: Option<u8>,
        visited: &mut Vec<StageDecision>,
        expansion: &RoleExpansion,
    ) -> Result<TreeOutcome, WorkError> {
        let description = filter.description.as_str();
        let matched = if let Some(m) = filter.match_interlingua.as_ref() {
            // Interlingua dispatch: AND of the set fields across any sentence,
            // gated by the sentence confidence floor. `confidence_min` is
            // fail-closed: a sentence whose parse carried no confidence is
            // treated as below the floor (low-confidence parses escalate).
            interlingua.is_some_and(|signals| {
                signals.iter().any(|s| {
                    m.predicate_id.is_none_or(|p| s.predicate_id == Some(p))
                        && m.subject_id.is_none_or(|s_| s.subject_id == Some(s_))
                        && m.object_id.is_none_or(|o| s.direct_object_id == Some(o))
                        && m.confidence_min
                            .is_none_or(|min| s.confidence.unwrap_or(0.0) >= min)
                })
            })
        } else {
            if filter.order.is_empty() {
                visited.push(node_decision(
                    "filter",
                    description,
                    StageVerdict::Passed,
                    "filter has no patterns — passing through".into(),
                    serde_json::json!({}),
                ));
                return Ok(TreeOutcome::Pass);
            }
            filter.matches(user_text)
        };
        if !matched {
            visited.push(node_decision(
                "filter",
                description,
                StageVerdict::Passed,
                if filter.match_interlingua.is_some() {
                    "interlingua filter did not match — passing through".into()
                } else {
                    "filter did not match — passing through".into()
                },
                serde_json::json!({}),
            ));
            return Ok(TreeOutcome::Pass);
        }

        match &filter.outcome {
            FilterOutcome::HardReject => {
                let reason = format!("blocked by filter: {description}");
                visited.push(node_decision(
                    "filter",
                    description,
                    StageVerdict::Rejected,
                    reason.clone(),
                    serde_json::json!({ "outcome": "hard_reject" }),
                ));
                Ok(TreeOutcome::Reject(reason))
            }
            FilterOutcome::SoftRedirect => {
                if let Some(target) = filter.redirect_to.as_deref() {
                    if let Some(node) = siblings.get(target) {
                        visited.push(node_decision(
                            "filter",
                            description,
                            StageVerdict::Rerouted,
                            format!("soft redirect to '{target}'"),
                            serde_json::json!({ "outcome": "soft_redirect", "redirect_to": target }),
                        ));
                        return self.evaluate_node(
                            node,
                            siblings,
                            user_text,
                            interlingua,
                            route_hints,
                            complexity,
                            visited,
                            expansion,
                        );
                    }
                    tracing::warn!(
                        target: "router.pipeline.stage2.tree",
                        target = %target,
                        "soft_redirect target not found among classifier children",
                    );
                }
                visited.push(node_decision(
                    "filter",
                    description,
                    StageVerdict::Passed,
                    "soft_redirect target missing — passing through".into(),
                    serde_json::json!({ "outcome": "soft_redirect" }),
                ));
                Ok(TreeOutcome::Pass)
            }
            FilterOutcome::OutputFilter => {
                // The tree records the match and continues; wiring the actual
                // redaction into the eventual dispatch response is post-tree.
                visited.push(node_decision(
                    "filter",
                    description,
                    StageVerdict::Passed,
                    "output_filter matched — flagged for redaction".into(),
                    serde_json::json!({ "outcome": "output_filter" }),
                ));
                Ok(TreeOutcome::Pass)
            }
        }
    }

    /// One classifier LLM call: build messages from the auto-constructed
    /// prompt, run through the shared limiter, and parse the three-axis verdict.
    /// `route_hints` (the overlay handoff) is appended to the system message as
    /// deterministic routing context (ROADMAP_20260827_ORT §2.6).
    fn call_classifier(
        &self,
        model: &str,
        prompt: &str,
        user_text: &str,
        route_hints: Option<&[crate::pipeline_types::RouteHint]>,
    ) -> Result<TreeClassifierVerdict, WorkError> {
        let mut system = prompt.to_string();
        if let Some(hints) = route_hints.filter(|h| !h.is_empty()) {
            system.push('\n');
            system.push_str(
                &crate::stages::classifier::ClassifierStage::route_hints_prompt_context(hints),
            );
        }
        let messages = vec![
            ChatMessage {
                role: "system".into(),
                content: system,
            },
            ChatMessage {
                role: "user".into(),
                content: user_text.to_string(),
            },
        ];

        // Late-bound first: a backend registered (or rewritten) after boot
        // wins; otherwise the boot-built per-node map and default serve
        // unchanged (injected mock path and unknown keys behave as before).
        let live = self
            .backend_resolver
            .as_ref()
            .and_then(|resolve| resolve(model));
        let client: &Arc<dyn ChatBackend> = live
            .as_ref()
            .or_else(|| self.clients.get(model))
            .unwrap_or(&self.default_client);
        tracing::info!(
            target: "router.pipeline.stage2.tree",
            model = %model,
            input_len = user_text.len(),
            system_prompt_len = prompt.len(),
            "tree classifier LLM request",
        );

        let call_start = Instant::now();
        let mut llm_latency_ms = 0u64;
        let response = self.limiter.run_sync(|| async {
            let llm_start = Instant::now();
            let result = client.chat_complete(&messages);
            llm_latency_ms = llm_start.elapsed().as_millis() as u64;
            result
        });
        let total_latency_ms = call_start.elapsed().as_millis() as u64;
        let limiter_wait_ms = total_latency_ms.saturating_sub(llm_latency_ms);

        let response = match response {
            Ok(r) => {
                tracing::info!(
                    target: "router.pipeline.stage2.tree",
                    model = %model,
                    llm_latency_ms = llm_latency_ms,
                    limiter_wait_ms = limiter_wait_ms,
                    response_len = r.len(),
                    "tree classifier LLM call succeeded",
                );
                r
            }
            Err(e) => {
                return Err(WorkError::Execution(format!(
                    "tree classifier LLM error for model '{model}': {e}"
                )));
            }
        };

        parse_tree_verdict(&response)
    }
}

/// Cost of a model entry for the cheapest-first group resolution.
pub fn cost(entry: &crate::config::ModelEntry) -> f64 {
    entry.cost_input + entry.cost_output
}
