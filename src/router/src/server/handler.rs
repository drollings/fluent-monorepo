use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use common_core::hash::uuid_v4;
use common_core::ResponseCache;
use http_body_util::BodyExt;

use crate::config::{ModelEntry, RouteRef, ToolPlan};
use crate::dag_session::{DependencySession, SessionRegistry, SessionStep, StepResult};
use crate::dispatch::escalation::{EscalationContext, Ladder};
use crate::ledger::ContentNodeLedger;
use crate::normalize;
use crate::pipeline::{PipelineOrchestrator, RoutingTarget};
use crate::routes::plan::PlanRoute;
use crate::routes::rigor::RigorRoute;
use crate::server::dispatch::handle_dispatch;
use crate::server::responses::completion_to_response;
use crate::server::responses::empty_response;
use crate::server::responses::error_response;
use crate::server::responses::make_error_completion;
use crate::server::responses::make_text_completion;
use crate::server::responses::HyperResponse;
use crate::server::responses::ServerStats;
use crate::testing::mock::MockDispatchContext;
use crate::types::RouterRequest;

/// The request-context dependency bundle handed to every HTTP handler.
/// Collapses the former 12-`Option` parameter list so escalation
/// (`ladders`, `context_cache`) and future concerns thread through one
/// struct instead of a growing signature.
#[derive(Clone)]
pub struct ServerDeps {
    pub pipelines: Arc<HashMap<String, Arc<PipelineOrchestrator>>>,
    pub routes: Arc<HashMap<String, RouteRef>>,
    pub models: Arc<HashMap<String, ModelEntry>>,
    pub stats: Arc<ServerStats>,
    pub max_payload: usize,
    pub classifier: Option<(String, ModelEntry)>,
    pub mock_dispatch: Option<Arc<MockDispatchContext>>,
    pub ledger: Option<Arc<ContentNodeLedger>>,
    pub cache: Option<Arc<ResponseCache>>,
    pub plan_route: Option<Arc<PlanRoute>>,
    pub rigor_route: Option<Arc<RigorRoute>>,
    pub sessions: Option<Arc<SessionRegistry>>,
    pub http_client: Arc<reqwest::Client>,
    /// Per-model-group escalation ladders. Keyed by
    /// `RoutingTarget.group`; resolved after the local chain exhausts.
    pub ladders: HashMap<String, Arc<Ladder>>,
    /// Deterministic-fact cache consulted before escalating.
    pub context_cache: Option<Arc<dyn fluent_types::ContextCache>>,
    /// Sidecar instance pool: aggregates the public `/instances` API and
    /// is consulted on a 503 group-miss to allocate fresh KV before retrying.
    pub instance_pool: Option<Arc<crate::instances::InstancePool>>,
    /// Env var naming the management API key (enforced on `/instances`).
    pub api_key_env_name: Option<String>,
    /// Managed llama-server supervisor (the process owner). `None` in mock
    /// mode. Backs `POST /models/unload` and the `/metrics` aggregation.
    pub supervisor: Option<Arc<crate::supervisor::LlamaServerSupervisor>>,
    /// The `LedgerAgentCoordinator`, when the operator opts in. `None`
    /// (the default) leaves dispatch unchanged — requests fall through to the
    /// existing pipeline.
    pub coordinator: Option<Arc<crate::ledger::orchestrator::LedgerAgentCoordinator>>,
    /// Config-declared bounded tool plans (from `needle.tool_plans`).
    /// Keys are route keys; values are ordered step sequences.
    pub tool_plans: HashMap<String, ToolPlan>,
    /// Global `needle.max_rounds` — the default round budget for tool plans
    /// that don't override it.
    pub needle_max_rounds: usize,
    /// Read-only `Lookup`-step resolvers for tool plans. A plan whose `Lookup`
    /// step names a kind without an installed resolver is declined to plain
    /// group dispatch (never a placeholder lookup).
    pub tool_lookup: crate::server::tool_lookup::ToolLookupRegistry,
}

impl ServerDeps {
    /// The escalation ladder for a model's route group, if the group
    /// configured one. Direct-model requests (no route - no group) get `None`
    /// - they never escalate.
    pub fn ladder_for_model(&self, model_name: &str) -> Option<&Arc<Ladder>> {
        let group = self.routes.get(model_name).map(|r| &r.group)?;
        self.ladders.get(group)
    }
}

/// Best-effort ledger insert, moved off the async handler via
/// `spawn_blocking` so sync rusqlite never runs on a tokio worker thread.
///
/// Both failure modes are swallowed by design: a panicked blocking task
/// (`.ok()`) and a ledger error (`.flatten()`) degrade to "no ledger row",
/// matching the documented best-effort logging contract.
pub(crate) async fn record_ledger_request(
    ledger: Option<&Arc<ContentNodeLedger>>,
    session_id: String,
    request_id: String,
    request_text: String,
) -> Option<fluent_types::NodeId> {
    let l = Arc::clone(ledger?);
    tokio::task::spawn_blocking(move || l.record_request(&session_id, &request_id, &request_text))
        .await
        .ok()
        .and_then(Result::ok)
}

/// Best-effort ledger update, off the async handler (see
/// `record_ledger_request` for the swallow semantics).
pub(crate) async fn record_ledger_result(
    ledger: Option<&Arc<ContentNodeLedger>>,
    node_id: Option<fluent_types::NodeId>,
    accepted: bool,
    score: Option<f64>,
    content: String,
) {
    let (Some(l), Some(node_id)) = (ledger, node_id) else {
        return;
    };
    let l = Arc::clone(l);
    tokio::task::spawn_blocking(move || {
        let _ = l.record_result(node_id, accepted, score, &content);
    })
    .await
    .ok();
}

/// Opt-in: run a request through the `LedgerAgentCoordinator`'s
/// synchronization loop when one is attached. Returns `Some(response)` when the
/// coordinator handled the request; `None` when no coordinator is attached (or
/// it produced no response), so the caller falls through to the existing
/// pipeline unchanged. Strictly additive — a deployment without a coordinator
/// is byte-identical to today.
async fn coordinator_dispatch(
    coordinator: Option<&Arc<crate::ledger::orchestrator::LedgerAgentCoordinator>>,
    session_id: &str,
    model: &str,
    user_text: &str,
) -> Option<HyperResponse> {
    let coord = coordinator.as_ref()?;
    let worker = crate::ledger::prompt::WorkerContext::new(
        model,
        "Answer the user's request using the provided ledger context.",
    );
    let outcome = match coord
        .run_agent(session_id, model, &worker, user_text)
        .await
    {
        Ok(o) => o,
        Err(e) => {
            tracing::error!(
                target: "router.server",
                session_id = %session_id,
                model = %model,
                error = %e,
                "coordinator run failed",
            );
            return Some(error_response(
                hyper::StatusCode::INTERNAL_SERVER_ERROR,
                &format!("coordinator error: {e}"),
            ));
        }
    };
    tracing::info!(
        target: "router.server",
        session_id = %session_id,
        model = %model,
        kv_restored = outcome.kv_restored,
        node_id = outcome.node_id.as_int(),
        "coordinator handled request",
    );
    Some(completion_to_response(
        &make_text_completion(model, &outcome.content),
        model,
        false,
        None,
    ))
}

/// Record a dispatch outcome into the session ledger + step.
///
/// Buffered dispatches carry the answer text synchronously and record it here.
/// Streaming dispatches assemble the answer as the client consumes the body,
/// so the record is deferred to a detached task that waits on the
/// [`StreamAnswer`](crate::streaming::StreamAnswer) finalizer (bounded by the
/// target's total timeout) and then records - never delaying the HTTP response.
/// `label` is the fallback content when no answer is available (escalation,
/// empty body).
async fn record_dispatch_outcome(
    answer_text: Option<String>,
    label: String,
    stream_answer: Option<crate::streaming::StreamAnswer>,
    ledger: Option<&Arc<ContentNodeLedger>>,
    ledger_node_id: Option<fluent_types::NodeId>,
    session_step: Option<&SessionStepHandle>,
    wait_timeout_ms: u64,
) {
    let Some(finalizer) = stream_answer else {
        let answer = answer_text.unwrap_or_default();
        let content = if answer.is_empty() {
            label
        } else {
            answer
        };
        record_ledger_result(
            ledger,
            ledger_node_id,
            true,
            Some(1.0),
            content.clone(),
        )
        .await;
        if let Some(step) = session_step {
            step.complete(true, Some(1.0), content.clone(), None);
        }
        return;
    };

    let ledger = ledger.map(Arc::clone);
    let node_id = ledger_node_id;
    let step = session_step.cloned();
    tokio::spawn(async move {
        let content = finalizer
            .wait(std::time::Duration::from_millis(wait_timeout_ms))
            .await
            .unwrap_or_else(|| label.clone());
        record_ledger_result(ledger.as_ref(), node_id, true, Some(1.0), content.clone()).await;
        if let Some(step) = step {
            step.complete(true, Some(1.0), content.clone(), None);
        }
    });
}

/// Per-request handle into a `DependencySession` step. Holds the session
/// `Arc` and the request's step id so the outcome can be recorded exactly once
/// from whichever terminal branch the request takes. Locking is scoped to the
/// `complete` call - never held across an await.
#[derive(Clone)]
struct SessionStepHandle {
    session: Arc<Mutex<DependencySession>>,
    step_id: String,
}

impl SessionStepHandle {
    fn complete(&self, accepted: bool, score: Option<f64>, content: String, error: Option<String>) {
        let mut session = match self.session.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        let _ = session.complete_step(
            &self.step_id,
            StepResult {
                content,
                accepted,
                score,
                latency_ms: 0,
                error,
            },
        );
    }
}

/// Register the request as a step in the session keyed by `session_id` (if a
/// `SessionRegistry` is wired) and return a handle to complete it when the
/// outcome is known.
fn begin_session_step(
    sessions: Option<&Arc<SessionRegistry>>,
    session_id: &str,
    model_name: &str,
    adapter: Option<&str>,
    request_id: &str,
    request_text: &str,
) -> Option<SessionStepHandle> {
    let registry = sessions?;
    let session = registry.get_or_create(session_id);
    {
        let mut s = match session.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        s.set_model(model_name);
        if let Some(adapter) = adapter {
            s.adapter = Some(adapter.to_string());
        }
        let step_id = format!("req-{request_id}");
        if s.get_step(&step_id).is_none() {
            let _ = s.add_step(SessionStep::new(step_id.clone(), request_text));
        }
    }
    Some(SessionStepHandle {
        session,
        step_id: format!("req-{request_id}"),
    })
}

/// Record a tool-plan step result into the session ledger (best-effort).
async fn record_step_to_ledger(
    ledger: Option<&Arc<ContentNodeLedger>>,
    step_id: &str,
    router_request: &RouterRequest,
    content: &str,
) {
    let Some(l) = ledger else { return };
    let l = Arc::clone(l);
    let session = router_request.session_id.clone().unwrap_or_default();
    let step_id = step_id.to_string();
    let content = content.to_string();
    tokio::task::spawn_blocking(move || {
        let _ = l.record_request(&session, &step_id, &content);
    })
    .await
    .ok();
}

/// Durable-audit one tool-plan step onto the canonical `router.audit`
/// stream (VISION: "auditable by construction"): the route, step id, ordinal,
/// kind, and dispatch target (model group for `Dispatch`/`Passthrough`,
/// lookup kind for `Lookup`) so a subagent-dispatched request is legible
/// post-hoc.
fn emit_step_audit(route: &str, step: &crate::config::ToolPlanStep, step_idx: usize) {
    let kind = match step.kind {
        crate::config::ToolPlanStepKind::Dispatch => "dispatch",
        crate::config::ToolPlanStepKind::Lookup => "lookup",
        crate::config::ToolPlanStepKind::Compose => "compose",
        crate::config::ToolPlanStepKind::Passthrough => "passthrough",
    };
    let target = match step.kind {
        crate::config::ToolPlanStepKind::Lookup => {
            step.lookup_kind.clone().unwrap_or_else(|| "unknown".into())
        }
        _ => step.target_group.clone().unwrap_or_else(|| "route".into()),
    };
    crate::audit::emit(
        "tool_plan_step",
        serde_json::json!({
            "route": route,
            "step": step.id,
            "step_idx": step_idx,
            "kind": kind,
            "target": target,
        }),
    );
}

/// Execute a bounded, config-declared tool plan.
///
/// When a `Rerouted` target matches a route with a `tool_plans` entry, this
/// function runs the plan's ordered steps instead of a single `handle_dispatch`
/// call. Each step records its result into the session ledger as a typed
/// `ContentNode` by origin and writes audit metadata (step id, target,
/// confidence) so the dispatch is legible post-hoc (VISION: "auditable by
/// construction").
///
/// Exceeding the plan's `max_rounds` (or the global `needle.max_rounds` when
/// the plan has none) falls back to the route's plain group dispatch rather
/// than looping (VISION: "terminate, don't loop").
///
/// `Dispatch` steps go through the standard `ChatBackend` chain via
/// `handle_dispatch`; `Lookup` steps perform a read-only data-store or
/// knowledge-graph lookup through an installed [`ToolLookup`] resolver;
/// `Compose` steps synthesize the final answer from prior step results.
///
/// A plan is executed only when it is *executable*: every `Lookup` step's
/// `lookup_kind` must have an installed resolver in `tool_lookup` (or the plan
/// has no `Lookup` steps). A plan with an unresolved `Lookup` kind is declined
/// to plain group dispatch — never executed with a placeholder (VISION: "no
/// fabricated data"). A `Lookup` step that resolves to nothing (absent) is
/// omitted from the composition; a `Lookup` step that errors degrades to plain
/// group dispatch.
async fn run_tool_plan(
    plan: &ToolPlan,
    rt: &RoutingTarget,
    router_request: &RouterRequest,
    model_name: &str,
    user_text: &str,
    mock_dispatch: Option<&Arc<MockDispatchContext>>,
    is_stream: bool,
    dispatch_deps: &crate::server::dispatch::DispatchDeps,
    ledger: Option<&Arc<ContentNodeLedger>>,
    ledger_node_id: Option<fluent_types::NodeId>,
    needle_max_rounds: usize,
    tool_lookup: &crate::server::tool_lookup::ToolLookupRegistry,
) -> Result<crate::server::dispatch::DispatchOutcome, std::convert::Infallible> {
    let plan_max = plan.max_rounds.unwrap_or(needle_max_rounds);
    let route = rt.target_name.as_deref().unwrap_or("unknown");

    tracing::info!(
        target: "router.server.tool_plan",
        route = %route,
        step_count = plan.steps.len(),
        max_rounds = plan_max,
        "executing bounded tool plan"
    );

    // A plan with a `Lookup` step of an unsupported kind is never executed —
    // decline to plain group dispatch (the fabricated-lookup leak is closed by
    // construction, not by a placeholder).
    if !crate::server::tool_lookup::plan_is_executable(plan, tool_lookup) {
        let unresolved: Vec<String> = plan
            .steps
            .iter()
            .filter(|s| matches!(s.kind, crate::config::ToolPlanStepKind::Lookup))
            .filter_map(|s| s.lookup_kind.clone())
            .collect();
        tracing::warn!(
            target: "router.server.tool_plan",
            route = %route,
            unresolved_lookup_kinds = ?unresolved,
            "tool plan has an unresolved lookup kind — declining to plain dispatch"
        );
        crate::audit::emit(
            "tool_plan_declined",
            serde_json::json!({
                "route": route,
                "reason": "unresolved_lookup",
                "lookup_kinds": unresolved,
            }),
        );
        return crate::server::dispatch::handle_dispatch(
            rt,
            router_request,
            model_name,
            user_text,
            mock_dispatch,
            is_stream,
            dispatch_deps,
        )
        .await;
    }

    // Accumulate step results for the Compose step. Only real step results are
    // pushed — a step that resolved to nothing is absent, never synthesized.
    let mut step_results: Vec<(String, String)> = Vec::new();
    let mut last_outcome: Option<crate::server::dispatch::DispatchOutcome> = None;

    crate::audit::emit(
        "tool_plan",
        serde_json::json!({
            "route": route,
            "steps": plan.steps.len(),
            "max_rounds": plan_max,
        }),
    );

    for (step_idx, step) in plan.steps.iter().enumerate() {
        if step_idx >= plan_max {
            tracing::warn!(
                target: "router.server.tool_plan",
                route = %route,
                step = %step.id,
                step_idx = step_idx,
                max_rounds = plan_max,
                "tool plan exceeded max_rounds — falling back to plain dispatch"
            );
            crate::audit::emit(
                "tool_plan_fallback",
                serde_json::json!({
                    "route": route,
                    "step": step.id,
                    "reason": "max_rounds_exceeded",
                    "max_rounds": plan_max,
                }),
            );
            // Fall back to plain group dispatch for the remainder.
            let outcome = crate::server::dispatch::handle_dispatch(
                rt,
                router_request,
                model_name,
                user_text,
                mock_dispatch,
                is_stream,
                dispatch_deps,
            )
            .await?;
            return Ok(outcome);
        }

        tracing::info!(
            target: "router.server.tool_plan",
            route = %route,
            step = %step.id,
            step_idx = step_idx,
            kind = ?step.kind,
            "executing tool plan step"
        );
        emit_step_audit(route, step, step_idx);

        match &step.kind {
            crate::config::ToolPlanStepKind::Dispatch => {
                let step_rt = if let Some(ref group) = step.target_group {
                    let mut t = rt.clone();
                    t.group = Some(group.clone());
                    t
                } else {
                    rt.clone()
                };
                let outcome = crate::server::dispatch::handle_dispatch(
                    &step_rt,
                    router_request,
                    model_name,
                    user_text,
                    mock_dispatch,
                    is_stream,
                    dispatch_deps,
                )
                .await?;
                // A mechanical failure of every target in the chain degrades
                // to the canned fallback completion (see `dispatch_real`) —
                // that sentinel, not the HTTP status (always 200), is the
                // step-failure signal.
                let ok = outcome.response.status().is_success()
                    && outcome.answer_text.as_deref()
                        != Some(crate::server::responses::FALLBACK_ANSWER);
                let answer = outcome.answer_text.clone().unwrap_or_default();
                step_results.push((step.id.clone(), answer.clone()));
                record_step_to_ledger(ledger, &step.id, router_request, &answer).await;
                if !ok {
                    // A failed step does not loop or continue the plan
                    // (VISION: "terminate, don't loop") — fall back to the
                    // route's plain group dispatch.
                    tracing::warn!(
                        target: "router.server.tool_plan",
                        route = %route,
                        step = %step.id,
                        status = %outcome.response.status(),
                        "tool plan step failed - falling back to plain dispatch"
                    );
                    crate::audit::emit(
                        "tool_plan_fallback",
                        serde_json::json!({
                            "route": route,
                            "step": step.id,
                            "reason": "step_failed",
                            "status": outcome.response.status().as_u16(),
                        }),
                    );
                    return crate::server::dispatch::handle_dispatch(
                        rt,
                        router_request,
                        model_name,
                        user_text,
                        mock_dispatch,
                        is_stream,
                        dispatch_deps,
                    )
                    .await;
                }
                last_outcome = Some(outcome);
            }
            crate::config::ToolPlanStepKind::Lookup => {
                let lookup_kind = step.lookup_kind.as_deref().unwrap_or("unknown");
                // The plan is executable, so a resolver is installed for this
                // kind. A resolver returns `Ok(Some)` (present material),
                // `Ok(None)` (absent — nothing relevant), or `Err` (failure —
                // degrade to plain dispatch, never a composed half-answer).
                let resolver = tool_lookup.get(lookup_kind);
                let ctx = crate::server::tool_lookup::LookupContext {
                    request: router_request,
                    ledger,
                    user_text,
                    session: dispatch_deps.session.as_ref(),
                };
                // `resolver` is installed (`plan_is_executable` guarantees it);
                // the defensive `Ok(None)` arm (no resolver) is treated as
                // absent rather than a hard failure.
                let resolved = resolver
                    .map(|r| r.lookup(user_text, &ctx))
                    .transpose();
                match resolved {
                    Ok(Some(Some(result))) if !result.is_empty() => {
                        let text = result.to_string();
                        step_results.push((step.id.clone(), text.clone()));
                        tracing::info!(
                            target: "router.server.tool_plan",
                            route = %route,
                            step = %step.id,
                            lookup_kind = %lookup_kind,
                            result_items = result.items.len(),
                            result_chars = text.chars().count(),
                            "lookup step resolved",
                        );
                        crate::audit::emit(
                            "tool_plan_lookup",
                            serde_json::json!({
                                "route": route,
                                "step": step.id,
                                "lookup_kind": lookup_kind,
                                "present": true,
                                "result_count": result.items.len(),
                            }),
                        );
                        record_step_to_ledger(ledger, &step.id, router_request, &text).await;
                    }
                    Ok(_) => {
                        // Absent: nothing relevant in the backing store. The
                        // step contributes no material — it is omitted from the
                        // composition, never replaced with placeholder text.
                        tracing::info!(
                            target: "router.server.tool_plan",
                            route = %route,
                            step = %step.id,
                            lookup_kind = %lookup_kind,
                            "lookup step absent — omitted from composition",
                        );
                        crate::audit::emit(
                            "tool_plan_lookup",
                            serde_json::json!({
                                "route": route,
                                "step": step.id,
                                "lookup_kind": lookup_kind,
                                "present": false,
                                "result_count": 0,
                            }),
                        );
                    }
                    Err(e) => {
                        // A genuine lookup failure degrades to plain group
                        // dispatch — never a half-composed answer.
                        tracing::warn!(
                            target: "router.server.tool_plan",
                            route = %route,
                            step = %step.id,
                            lookup_kind = %lookup_kind,
                            error = %e,
                            "tool plan lookup failed - falling back to plain dispatch"
                        );
                        crate::audit::emit(
                            "tool_plan_fallback",
                            serde_json::json!({
                                "route": route,
                                "step": step.id,
                                "reason": "lookup_failed",
                                "lookup_kind": lookup_kind,
                                "error": e.to_string(),
                            }),
                        );
                        return crate::server::dispatch::handle_dispatch(
                            rt,
                            router_request,
                            model_name,
                            user_text,
                            mock_dispatch,
                            is_stream,
                            dispatch_deps,
                        )
                        .await;
                    }
                }
            }
            crate::config::ToolPlanStepKind::Compose => {
                // Join only the step results that are real, non-empty material
                // from a successful Dispatch/Lookup/Passthrough. A step that
                // resolved to nothing (absent) never lands in `step_results`,
                // and an empty string is defensively omitted too — the
                // composition never carries a placeholder or a synthesized
                // stand-in for an absent step.
                let real: Vec<&(String, String)> =
                    step_results.iter().filter(|(_, result)| !result.is_empty()).collect();
                let composed = if real.is_empty() {
                    tracing::warn!(
                        target: "router.server.tool_plan",
                        route = %route,
                        step = %step.id,
                        "compose step with no real results",
                    );
                    String::new()
                } else {
                    real.iter()
                        .map(|(id, result)| format!("[{id}]: {result}"))
                        .collect::<Vec<_>>()
                        .join("\n\n")
                };
                step_results.push((step.id.clone(), composed.clone()));
                let completion =
                    crate::server::responses::make_text_completion(model_name, &composed);
                let response = crate::server::responses::completion_to_response(
                    &completion,
                    model_name,
                    is_stream,
                    None,
                );
                record_ledger_result(ledger, ledger_node_id, true, Some(1.0), composed.clone())
                    .await;
                crate::audit::emit(
                    "tool_plan_composed",
                    serde_json::json!({
                        "route": route,
                        "step": step.id,
                        "inputs": step_results.len() - 1,
                        "answer_chars": composed.chars().count(),
                    }),
                );
                return Ok(crate::server::dispatch::DispatchOutcome {
                    response,
                    answer_text: Some(composed),
                    stream_answer: None,
                });
            }
            crate::config::ToolPlanStepKind::Passthrough => {
                let step_rt = {
                    let mut t = rt.clone();
                    if let Some(ref group) = step.target_group {
                        t.group = Some(group.clone());
                    }
                    t
                };
                let outcome = crate::server::dispatch::handle_dispatch(
                    &step_rt,
                    router_request,
                    model_name,
                    user_text,
                    mock_dispatch,
                    is_stream,
                    dispatch_deps,
                )
                .await?;
                let answer = outcome.answer_text.clone().unwrap_or_default();
                step_results.push((step.id.clone(), answer));
                last_outcome = Some(outcome);
            }
        }
    }

    // If we exhausted all steps without a Compose step, return the last
    // dispatch outcome (degrades to plain dispatch behavior).
    if let Some(outcome) = last_outcome {
        Ok(outcome)
    } else {
        tracing::warn!(
            target: "router.server.tool_plan",
            route = %route,
            "tool plan completed with no outcome - falling back to plain dispatch"
        );
        crate::server::dispatch::handle_dispatch(
            rt,
            router_request,
            model_name,
            user_text,
            mock_dispatch,
            is_stream,
            dispatch_deps,
        )
        .await
    }
}

/// On-demand residency for the classifier, mirroring the dispatch path's
/// `ensure_target_ready` + allocate-on-miss guarantee.
///
/// The classifier's LLM call runs through a plain sync `LlmClient` with no
/// sidecar access, so when its work-pool group has no resident member the
/// fork answers `400 model or instance not found: '<group>'` and every
/// request is rejected. Before the pipeline runs, ensure the classifier's
/// managed model is loaded and its work-pool group exists — created on demand
/// exactly as the dispatch path would. Everything is derived from config
/// (`RouterConfig.classifier_model` → the model entry's `pool_qualifier`);
/// nothing is hardcoded. Best-effort: a load/allocate failure degrades to the
/// classifier's own error path below.
async fn ensure_classifier_ready(
    classifier: Option<&(String, ModelEntry)>,
    instance_pool: Option<&Arc<crate::instances::InstancePool>>,
) {
    let (Some((key, entry)), Some(pool)) = (classifier, instance_pool) else {
        return;
    };
    if !entry.is_managed() {
        return;
    }
    let Some(group) = entry.pool_qualifier() else {
        return;
    };
    let Some(manager) = pool.manager_for_url(&entry.endpoint) else {
        return;
    };
    pool.ensure_target_ready(&entry.endpoint, None).await;
    if let Err(e) = manager.ensure_group_ready(&group).await {
        tracing::warn!(
            target: "router.server",
            classifier_model = %entry.name.as_deref().unwrap_or(key),
            group = %group,
            error = %e,
            "classifier work-pool ensure failed",
        );
    }
}

async fn handle_chat_completion(
    req: hyper::Request<hyper::body::Incoming>,
    deps: ServerDeps,
) -> Result<HyperResponse, std::convert::Infallible> {
    let ServerDeps {
        pipelines,
        routes,
        models,
        stats,
        max_payload,
        classifier,
        mock_dispatch,
        ledger,
        cache,
        plan_route,
        rigor_route: _,
        sessions,
        http_client,
        ladders,
        context_cache,
        instance_pool,
        api_key_env_name: _,
        supervisor: _,
        coordinator,
        tool_plans,
        needle_max_rounds,
        tool_lookup,
    } = deps;
    // The dispatch post-processing hook (workflow extraction), if the
    // operator configured it. Passed through to successful dispatches only.
    let workflow_extractor = plan_route
        .as_ref()
        .and_then(|p| p.workflow_extractor().cloned());
    // The query string is captured before the body is consumed.
    let query_string = req.uri().query().map(ToOwned::to_owned);
    let body_bytes = match req.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(e) => {
            stats.errors.fetch_add(1, Ordering::Relaxed);
            return Ok(error_response(
                hyper::StatusCode::BAD_REQUEST,
                &format!("body read error: {e}"),
            ));
        }
    };

    if body_bytes.len() > max_payload {
        return Ok(empty_response(hyper::StatusCode::PAYLOAD_TOO_LARGE));
    }

    if body_bytes.is_empty() {
        stats.errors.fetch_add(1, Ordering::Relaxed);
        return Ok(error_response(hyper::StatusCode::BAD_REQUEST, "empty body"));
    }

    let body_str = std::str::from_utf8(&body_bytes).unwrap_or("");
    let mut body_json: serde_json::Value = match serde_json::from_str(body_str) {
        Ok(v) => v,
        Err(e) => {
            stats.errors.fetch_add(1, Ordering::Relaxed);
            return Ok(error_response(
                hyper::StatusCode::BAD_REQUEST,
                &format!("invalid JSON: {e}"),
            ));
        }
    };

    // The routing fields (`model`/`instance`/`snapshot`/`id_slot`) are read
    // from BOTH the JSON body and the query string, body wins. Merge query
    // values only for keys the body does not define.
    if let Some(query) = query_string.as_deref() {
        for (key, value) in crate::server::instances_api::parse_query(query) {
            if !matches!(key.as_str(), "model" | "instance" | "snapshot" | "id_slot") {
                continue;
            }
            if body_json.get(&key).is_some() {
                continue;
            }
            let value = if key == "id_slot" {
                value.parse::<i32>().ok().map_or_else(
                    || serde_json::Value::String(value),
                    |n| serde_json::json!(n),
                )
            } else {
                serde_json::Value::String(value)
            };
            if let serde_json::Value::Object(ref mut obj) = body_json {
                obj.insert(key, value);
            }
        }
    }

    let router_request = match normalize::normalize_request(body_json) {
        Ok(r) => r,
        Err(e) => {
            stats.errors.fetch_add(1, Ordering::Relaxed);
            return Ok(error_response(
                hyper::StatusCode::BAD_REQUEST,
                &e.to_string(),
            ));
        }
    };

    let is_stream = router_request.stream.unwrap_or(false);
    let model_name = router_request.model.clone();
    let user_message: String = router_request
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| common_core::string::truncate_utf8(&m.content.to_string_lossy(), 120))
        .unwrap_or_default();

    tracing::info!(
        target: "router.server",
        model = %model_name,
        user_message = %user_message,
        messages = router_request.messages.len(),
        stream = is_stream,
        "incoming request"
    );

    stats.requests.fetch_add(1, Ordering::Relaxed);

    let session_id = router_request.session_id.clone().unwrap_or_else(uuid_v4);
    let request_id = uuid_v4();
    let request_text = router_request
        .messages
        .iter()
        .find(|m| m.role == "user")
        .map(|m| m.content.to_string_lossy())
        .unwrap_or_default();

    // Opt-in: when a coordinator is attached, route the request through its
    // run loop (restore-or-assemble → execute → record → snapshot → enqueue).
    // `None` falls through to the existing pipeline unchanged.
    if let Some(resp) =
        coordinator_dispatch(coordinator.as_ref(), &session_id, &model_name, &request_text).await
    {
        return Ok(resp);
    }

    let ledger_node_id = record_ledger_request(
        ledger.as_ref(),
        session_id.clone(),
        request_id.clone(),
        request_text.clone(),
    )
    .await;

    // Canonical session: register the request as a step and complete it at
    // whichever terminal branch the request takes (outcome recorded exactly
    // once).
    let session_step = begin_session_step(
        sessions.as_ref(),
        &session_id,
        &model_name,
        router_request.adapter.as_deref(),
        &request_id,
        &request_text,
    );

    // Bypass: a session the turnover mode marked frontier-owned skips
    // the local pipeline and goes straight to the frontier.
    if let Some(step) = &session_step {
        let frontier_owned = step.session.lock().is_ok_and(|s| s.is_frontier_owned());
        if frontier_owned {
            let group = routes.get(&model_name).map(|r| r.group.as_str());
            if let Some(ladder) = group.and_then(|g| ladders.get(g)) {
                tracing::info!(
                    target: "router.server",
                    session_id = %session_id,
                    "session is frontier-owned - bypassing local pipeline"
                );
                let esc_ctx = EscalationContext {
                    request: &router_request,
                    user_text: &user_message,
                    model_name: &model_name,
                    context_cache: context_cache.as_ref(),
                    session: Some(&step.session),
                };
                if let Some(resp) = ladder.dispatch_frontier(&esc_ctx).await {
                    step.complete(
                        resp.status().is_success(),
                        None,
                        format!("frontier dispatch: {}", resp.status()),
                        None,
                    );
                    return Ok(resp);
                }
            }
        }
    }

    // On-demand residency for the classifier: ensure its managed model is
    // loaded and its work-pool group is resident before the pipeline's
    // (sync, sidecar-less) classifier LLM call would 400 on a missing group.
    ensure_classifier_ready(classifier.as_ref(), instance_pool.as_ref()).await;

    let pipeline_result =
        resolve_pipeline(&model_name, &routes, &models, &pipelines, &router_request);

    let user_text = router_request
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.to_string_lossy())
        .unwrap_or_default();

    if pipeline_result.rejected {
        stats.rejections.fetch_add(1, Ordering::Relaxed);
        let reason = pipeline_result
            .reject_reason
            .as_deref()
            .unwrap_or("request rejected");

        if let Some(ref mock) = mock_dispatch {
            if let Some(entry) = mock.lookup(&user_text) {
                mock.validate_rejection(entry, reason);
            }
        }

        record_ledger_result(
            ledger.as_ref(),
            ledger_node_id,
            is_stream,
            Some(0.0),
            reason.to_string(),
        )
        .await;

        if let Some(ref step) = session_step {
            step.complete(
                is_stream,
                Some(0.0),
                reason.to_string(),
                Some(reason.to_string()),
            );
        }

        let completion = make_error_completion(&model_name, reason);
        return Ok(completion_to_response(
            &completion,
            &model_name,
            is_stream,
            None,
        ));
    }

    if let Some(ref resp_str) = pipeline_result.classifier_response {
        tracing::info!(
            target: "router.server",
            model = %model_name,
            response_len = resp_str.len(),
            "responding with classifier direct response"
        );
        record_ledger_result(
            ledger.as_ref(),
            ledger_node_id,
            true,
            Some(1.0),
            resp_str.clone(),
        )
        .await;
        if let Some(ref step) = session_step {
            step.complete(true, Some(1.0), resp_str.clone(), None);
        }
        let completion = make_text_completion(&model_name, resp_str);
        return Ok(completion_to_response(
            &completion,
            &model_name,
            is_stream,
            None,
        ));
    }

    let dispatch_deps = crate::server::dispatch::DispatchDeps {
        http_client: Arc::clone(&http_client),
        cache: cache.clone(),
        stats: Arc::clone(&stats),
        extractor: workflow_extractor.clone(),
        ladders,
        context_cache,
        session: session_step.as_ref().map(|s| s.session.clone()),
        instance_pool: instance_pool.map(|p| p.as_ref().clone()),
    };

    if let Some(ref rt) = pipeline_result.routing_target {
        // Check if this route has a config-declared tool plan.
        let route_name = rt.target_name.as_deref().unwrap_or("");
        if let Some(plan) = tool_plans.get(route_name) {
            let outcome = run_tool_plan(
                plan,
                rt,
                &router_request,
                &model_name,
                &user_text,
                mock_dispatch.as_ref(),
                is_stream,
                &dispatch_deps,
                ledger.as_ref(),
                ledger_node_id,
                needle_max_rounds,
                &tool_lookup,
            )
            .await?;
            let status = outcome.response.status();
            record_dispatch_outcome(
                outcome.answer_text.clone(),
                format!("tool plan dispatched: {route_name}: {status}"),
                outcome.stream_answer.clone(),
                ledger.as_ref(),
                ledger_node_id,
                session_step.as_ref(),
                rt.total_timeout_ms,
            )
            .await;
            return Ok(outcome.response);
        }
        let outcome = handle_dispatch(
            rt,
            &router_request,
            &model_name,
            &user_text,
            mock_dispatch.as_ref(),
            is_stream,
            &dispatch_deps,
        )
        .await?;
        let status = outcome.response.status();
        record_dispatch_outcome(
            outcome.answer_text.clone(),
            format!("dispatched to {}: {status}", rt.model),
            outcome.stream_answer.clone(),
            ledger.as_ref(),
            ledger_node_id,
            session_step.as_ref(),
            rt.total_timeout_ms,
        )
        .await;
        return Ok(outcome.response);
    }

    if let Some((ref key, ref entry)) = classifier {
        let rt_for_fallback = RoutingTarget::from_model_entry(key, entry);
        tracing::info!(
            target: "router.server",
            model = %rt_for_fallback.model,
            fallback_url = %rt_for_fallback.url,
            "no routing target - dispatching to classifier fallback"
        );
        let outcome = handle_dispatch(
            &rt_for_fallback,
            &router_request,
            &model_name,
            &user_text,
            mock_dispatch.as_ref(),
            is_stream,
            &dispatch_deps,
        )
        .await?;
        let status = outcome.response.status();
        record_dispatch_outcome(
            outcome.answer_text.clone(),
            format!("dispatched to classifier fallback: {status}"),
            outcome.stream_answer.clone(),
            ledger.as_ref(),
            ledger_node_id,
            session_step.as_ref(),
            0,
        )
        .await;
        return Ok(outcome.response);
    }

    tracing::warn!(
        target: "router.server",
        model = %model_name,
        "no routing target, no classifier response, no classifier url - returning fallback"
    );
    record_ledger_result(
        ledger.as_ref(),
        ledger_node_id,
        true,
        Some(0.5),
        "fallback response".to_string(),
    )
    .await;
    if let Some(ref step) = session_step {
        step.complete(true, Some(0.5), "fallback response".to_string(), None);
    }
    let completion = crate::server::responses::fallback_completion(&model_name);
    Ok(completion_to_response(
        &completion,
        &model_name,
        is_stream,
        None,
    ))
}
#[allow(clippy::implicit_hasher)]
pub async fn handle_request(
    req: hyper::Request<hyper::body::Incoming>,
    deps: ServerDeps,
) -> Result<HyperResponse, std::convert::Infallible> {
    // Install the router's knowledge capability for the life of this
    // request so `ContentNodeStore`'s gated `KnowledgeCapability` impl
    // (`crate::knowledge.rs`) is *actually reachable* on the serving path —
    // not only in tests. Every gated knowledge read/write during dispatch
    // checks this token in the current task-local. Effects the request path
    // does not need are simply absent from this set.
    fluent_wvr::CURRENT_CAPS
        .scope(
            fluent_wvr::CapabilitySet::new().with(crate::knowledge::RouterKnowledgeCapability),
            handle_request_inner(req, deps),
        )
        .await
}

async fn handle_request_inner(
    req: hyper::Request<hyper::body::Incoming>,
    deps: ServerDeps,
) -> Result<HyperResponse, std::convert::Infallible> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let ServerDeps { stats, cache, .. } = &deps;

    if method == hyper::Method::OPTIONS {
        return Ok(empty_response(hyper::StatusCode::NO_CONTENT));
    }

    match (method.as_str(), path.as_str()) {
        ("GET", "/health") => {
            stats.requests.fetch_add(1, Ordering::Relaxed);
            Ok(crate::server::responses::json_response(
                hyper::StatusCode::OK,
                &serde_json::json!({
                    "status": "ok",
                    "cache_hits": stats.cache_hits.load(Ordering::Relaxed),
                    "cache_misses": stats.cache_misses.load(Ordering::Relaxed),
                }),
            ))
        }
        ("GET", "/stats") => {
            stats.requests.fetch_add(1, Ordering::Relaxed);
            let body = serde_json::json!({
                "requests": stats.requests.load(Ordering::Relaxed),
                "errors": stats.errors.load(Ordering::Relaxed),
                "rejections": stats.rejections.load(Ordering::Relaxed),
                "cache_hits": stats.cache_hits.load(Ordering::Relaxed),
                "cache_misses": stats.cache_misses.load(Ordering::Relaxed),
            });
            Ok(crate::server::responses::json_response(
                hyper::StatusCode::OK,
                &body,
            ))
        }
        ("POST", "/admin/cache/invalidate") => {
            if !is_local_request(&req) {
                return Ok(crate::server::responses::forbidden_response());
            }
            if let Some(ref cache) = cache {
                cache.invalidate_all();
                stats.requests.fetch_add(1, Ordering::Relaxed);
                Ok(crate::server::responses::json_response(
                    hyper::StatusCode::OK,
                    &serde_json::json!({"status": "ok"}),
                ))
            } else {
                Ok(crate::server::responses::json_response(
                    hyper::StatusCode::OK,
                    &serde_json::json!({"status": "no_cache"}),
                ))
            }
        }
        ("POST", "/v1/chat/completions") => handle_chat_completion(req, deps).await,
        ("POST", "/v1/plan") => {
            stats.requests.fetch_add(1, Ordering::Relaxed);
            crate::routes::plan::handle_plan_request(
                req,
                deps.plan_route.clone(),
                deps.max_payload,
                stats,
            )
            .await
        }
        ("POST", "/v1/rigor") => {
            stats.requests.fetch_add(1, Ordering::Relaxed);
            crate::routes::rigor::handle_rigor_request(req, deps).await
        }
        // -- Shared-weight instance management API (mirrors the llama-server
        //    contract; aggregated across every managed model) --------------
        ("GET", "/instances" | "/v1/instances") => {
            stats.requests.fetch_add(1, Ordering::Relaxed);
            if let Some(resp) = crate::server::instances_api::check_management_key(&deps, req.headers())
            {
                return Ok(resp);
            }
            let query = crate::server::instances_api::parse_query(req.uri().query().unwrap_or(""));
            Ok(crate::server::instances_api::handle_get_instances(&deps, &query).await)
        }
        ("POST", "/instances" | "/v1/instances") => {
            stats.requests.fetch_add(1, Ordering::Relaxed);
            if let Some(resp) = crate::server::instances_api::check_management_key(&deps, req.headers())
            {
                return Ok(resp);
            }
            let query = crate::server::instances_api::parse_query(req.uri().query().unwrap_or(""));
            Ok(crate::server::instances_api::handle_post_instances(req, &deps, &query).await)
        }
        ("GET", "/memory") => {
            stats.requests.fetch_add(1, Ordering::Relaxed);
            if let Some(resp) = crate::server::instances_api::check_management_key(&deps, req.headers())
            {
                return Ok(resp);
            }
            Ok(crate::server::instances_api::handle_memory(&deps).await)
        }
        ("GET", "/v1/models" | "/models") => {
            stats.requests.fetch_add(1, Ordering::Relaxed);
            Ok(crate::server::instances_api::handle_list_models(&deps).await)
        }
        ("POST", "/models/unload" | "/v1/models/unload") => {
            stats.requests.fetch_add(1, Ordering::Relaxed);
            Ok(crate::server::admin::handle_unload_model(req, &deps).await)
        }
        ("GET", "/metrics") => {
            stats.requests.fetch_add(1, Ordering::Relaxed);
            let query = crate::server::instances_api::parse_query(req.uri().query().unwrap_or(""));
            Ok(crate::server::admin::handle_metrics(&deps, &query).await)
        }
        ("GET" | "POST", "/props") => {
            stats.requests.fetch_add(1, Ordering::Relaxed);
            Ok(crate::server::instances_api::handle_props(&deps).await)
        }
        // Model-less llama-server endpoints (proxied to the pool's default
        // server).
        ("POST", "/tokenize" | "/detokenize" | "/apply-template" | "/control") => {
            stats.requests.fetch_add(1, Ordering::Relaxed);
            let path = path.clone();
            Ok(crate::server::instances_api::handle_model_less_proxy(req, &deps, &path).await)
        }
        _ => {
            // Instance management sub-resources: `/instances/:name[/...]`.
            if path.starts_with("/instances/") {
                stats.requests.fetch_add(1, Ordering::Relaxed);
                if let Some(resp) =
                    crate::server::instances_api::check_management_key(&deps, req.headers())
                {
                    return Ok(resp);
                }
                let query =
                    crate::server::instances_api::parse_query(req.uri().query().unwrap_or(""));
                return Ok(match route_instance_resource(method.as_str(), &path) {
                    Some((op, name, snapshot)) => {
                        crate::server::instances_api::handle_snapshot_op_or_instance_op(
                            req, &deps, op, &name, snapshot.as_deref(), &query,
                        )
                        .await
                    }
                    None => crate::server::responses::empty_response(
                        hyper::StatusCode::NOT_FOUND,
                    ),
                });
            }
            if method == hyper::Method::DELETE && path.starts_with("/admin/cache/") {
                if !is_local_request(&req) {
                    return Ok(crate::server::responses::forbidden_response());
                }
                let key = &path["/admin/cache/".len()..];
                if key.is_empty() {
                    return Ok(crate::server::responses::error_response(
                        hyper::StatusCode::BAD_REQUEST,
                        "missing cache key",
                    ));
                }
                if let Some(ref cache_backend) = cache {
                    cache_backend.invalidate_key_raw(key);
                    stats.requests.fetch_add(1, Ordering::Relaxed);
                    Ok(crate::server::responses::json_response(
                        hyper::StatusCode::OK,
                        &serde_json::json!({"status": "deleted"}),
                    ))
                } else {
                    Ok(crate::server::responses::json_response(
                        hyper::StatusCode::OK,
                        &serde_json::json!({"status": "no_cache"}),
                    ))
                }
            } else {
                let code = if path == "/v1/chat/completions" {
                    hyper::StatusCode::METHOD_NOT_ALLOWED
                } else {
                    hyper::StatusCode::NOT_FOUND
                };
                Ok(empty_response(code))
            }
        }
    }
}

/// Route an `/instances/<resource>` path to an operation:
///
/// - `DELETE /instances/:name` -> `("delete", name, None)`
/// - `POST /instances/:name/pin|unpin|resize` -> the matching op
/// - `POST /instances/:name/snapshot` -> `("save", name, None)`
/// - `GET /instances/:name/snapshots` -> `("list", name, None)`
/// - `DELETE /instances/:name/snapshot/:snapshot` -> `("delete_snapshot", name, Some(snapshot))`
fn route_instance_resource(method: &str, path: &str) -> Option<(&'static str, String, Option<String>)> {
    let rest = path.strip_prefix("/instances/")?;
    let parts: Vec<&str> = rest.split('/').collect();
    match (method, parts.as_slice()) {
        ("DELETE", [name]) => Some(("delete", name.to_string(), None)),
        ("POST", [name, "pin"]) => Some(("pin", name.to_string(), None)),
        ("POST", [name, "unpin"]) => Some(("unpin", name.to_string(), None)),
        ("POST", [name, "resume"]) => Some(("resume", name.to_string(), None)),
        ("POST", [name, "no-resume"]) => Some(("no_resume", name.to_string(), None)),
        ("POST", [name, "resize"]) => Some(("resize", name.to_string(), None)),
        ("POST", [name, "snapshot"]) => Some(("save", name.to_string(), None)),
        ("GET", [name, "snapshots"]) => Some(("list", name.to_string(), None)),
        ("DELETE", [name, "snapshot", snapshot]) => {
            Some(("delete_snapshot", name.to_string(), Some(snapshot.to_string())))
        }
        _ => None,
    }
}

pub(crate) fn is_local_request(req: &hyper::Request<hyper::body::Incoming>) -> bool {
    req.headers()
        .get(hyper::header::HOST)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host.eq_ignore_ascii_case("127.0.0.1")
                || host.eq_ignore_ascii_case("::1")
                || host.starts_with("localhost:")
                || host.starts_with("127.0.0.1:")
                || host.starts_with("[::1]:")
        })
}

fn resolve_pipeline(
    model_name: &str,
    routes: &std::collections::HashMap<String, RouteRef>,
    models: &std::collections::HashMap<String, ModelEntry>,
    pipelines: &std::collections::HashMap<String, Arc<PipelineOrchestrator>>,
    router_request: &RouterRequest,
) -> crate::pipeline::PipelineResult {
    use fluent_wvr::prelude::*;

    // The model id grammar `<model_id>[:<instance|group|latest>]`: a qualified
    // id resolves directly to the owning model's server, bypassing the route
    // table. `<id>:latest` means the pool's default instance.
    if let Some((base_model, qualifier)) = model_name.split_once(':') {
        if let Some(entry) = models.get(base_model) {
            let rt = if qualifier == "latest" {
                RoutingTarget::from_model_entry(base_model, entry)
            } else {
                RoutingTarget::from_model_entry_instance(base_model, entry, qualifier)
            };
            tracing::info!(
                target: "router.server",
                model = %model_name,
                target = %rt.model,
                "qualified model id resolved to owning server",
            );
            return crate::pipeline::PipelineResult {
                decisions: vec![],
                final_response: None,
                rejected: false,
                reject_reason: None,
                routing_target: Some(rt),
                classifier_response: None,
            };
        }
    }

    let route = routes.get(model_name).cloned();

    let pipeline_names: Vec<String> = if let Some(ref r) = route {
        r.pipelines.clone()
    } else if let Some(model_entry) = models.get(model_name) {
        let rt = RoutingTarget::from_model_entry(model_name, model_entry);
        return crate::pipeline::PipelineResult {
            decisions: vec![],
            final_response: None,
            rejected: false,
            reject_reason: None,
            routing_target: Some(rt),
            classifier_response: None,
        };
    } else {
        routes
            .get("local")
            .map_or_else(|| vec!["default".into()], |r| r.pipelines.clone())
    };

    let mut ctx = WorkContext::default();
    ctx.set_structured("request", router_request);

    let mut all_decisions = Vec::new();
    let mut last_result: Option<crate::pipeline::PipelineResult> = None;

    for name in &pipeline_names {
        let Some(pipeline) = pipelines.get(name) else {
            tracing::warn!(target: "router.server", pipeline = %name, "pipeline not found, skipping");
            continue;
        };

        let output = match pipeline.execute(&ctx) {
            Ok(o) => o,
            Err(e) => {
                return crate::pipeline::PipelineResult {
                    decisions: all_decisions,
                    final_response: None,
                    rejected: true,
                    reject_reason: Some(format!("pipeline '{name}' error: {e}")),
                    routing_target: None,
                    classifier_response: None,
                };
            }
        };

        let mut result: crate::pipeline::PipelineResult = match output.data_take() {
            Ok(r) => r,
            Err(e) => {
                return crate::pipeline::PipelineResult {
                    decisions: all_decisions,
                    final_response: None,
                    rejected: true,
                    reject_reason: Some(format!("pipeline '{name}' output decode: {e}")),
                    routing_target: None,
                    classifier_response: None,
                };
            }
        };

        if result.rejected {
            return result;
        }

        all_decisions.append(&mut result.decisions);
        last_result = Some(result);
    }

    let mut final_result = match last_result {
        Some(result) => result,
        None => {
            // No requested pipeline is built (boot logged the drop). In a
            // healthy boot every route's pipeline exists; an empty build
            // means a config error — most commonly a `classifier_model` that
            // does not resolve to a configured model. Surface a legible
            // error rather than a canned success.
            if pipeline_names.is_empty() {
                crate::pipeline::PipelineResult {
                    decisions: vec![],
                    final_response: None,
                    rejected: false,
                    reject_reason: None,
                    routing_target: None,
                    classifier_response: None,
                }
            } else {
                return crate::pipeline::PipelineResult {
                    decisions: all_decisions,
                    final_response: None,
                    rejected: true,
                    reject_reason: Some(format!(
                        "none of the requested pipelines are built (missing: {}); \
                         check the config — a classifier model that does not resolve \
                         to a configured model prevents pipeline build",
                        pipeline_names.join(", ")
                    )),
                    routing_target: None,
                    classifier_response: None,
                };
            }
        }
    };
    final_result.decisions = all_decisions;
    final_result
}


#[cfg(test)]
mod tool_plan_tests {
    use super::*;
    use crate::config::{ToolPlan, ToolPlanStep, ToolPlanStepKind};
    use crate::server::dispatch::DispatchDeps;
    use crate::testing::test_request;

    fn block<T>(f: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Runtime::new().unwrap().block_on(f)
    }

    fn plan_target() -> crate::pipeline::RoutingTarget {
        crate::pipeline::RoutingTarget {
            url: "http://x/v1/chat/completions".into(),
            model: "plan-model".into(),
            group: Some("explain".into()),
            target_name: Some("explain".into()),
            params: None,
            instance: None,
            snapshot: None,
            id_slot: None,
            filter_thinking: false,
            retry_count: 0,
            retry_base_interval_s: 1,
            stream: false,
            idle_timeout_ms: 5000,
            total_timeout_ms: 30000,
            fallbacks: vec![],
            target: None,
        }
    }

    fn dispatch_step(id: &str) -> ToolPlanStep {
        ToolPlanStep {
            id: id.to_string(),
            kind: ToolPlanStepKind::Dispatch,
            target_group: Some("explain".into()),
            description: None,
            lookup_kind: None,
            step_max_rounds: None,
        }
    }

    fn compose_step() -> ToolPlanStep {
        ToolPlanStep {
            id: "compose".to_string(),
            kind: ToolPlanStepKind::Compose,
            target_group: None,
            description: None,
            lookup_kind: None,
            step_max_rounds: None,
        }
    }

    fn deps() -> DispatchDeps {
        DispatchDeps {
            http_client: Arc::new(reqwest::Client::new()),
            cache: None,
            stats: Arc::new(crate::server::responses::ServerStats::default()),
            extractor: None,
            ladders: HashMap::new(),
            context_cache: None,
            session: None,
            instance_pool: None,
        }
    }

    fn mock_canned(answer: &str) -> Arc<crate::testing::MockDispatchContext> {
        Arc::new(crate::testing::MockDispatchContext::new(
            vec![crate::testing::MockTranscriptEntry {
                user_message: "hello".into(),
                classifier_response: String::new(),
                dispatch_response: Some(answer.into()),
                ..Default::default()
            }],
            vec![],
        ))
    }

    fn empty_lookups() -> crate::server::tool_lookup::ToolLookupRegistry {
        crate::server::tool_lookup::ToolLookupRegistry::new()
    }

    #[test]
    fn plan_within_rounds_composes_step_results() {
        let plan = ToolPlan {
            steps: vec![dispatch_step("identify_subject"), compose_step()],
            max_rounds: Some(3),
        };
        let mock = mock_canned("subject identified");
        let (outcome, lines) = crate::test_support::capture_logs(|| {
            block(async {
                run_tool_plan(
                    &plan,
                    &plan_target(),
                    &test_request("hello"),
                    "test-model",
                    "hello",
                    Some(&mock),
                    false,
                    &deps(),
                    None,
                    None,
                    3,
                    &empty_lookups(),
                )
                .await
                .expect("run_tool_plan is infallible")
            })
        });

        assert!(outcome.response.status().is_success());
        // The Compose step synthesized the answer from the prior step result.
        let answer = outcome.answer_text.unwrap_or_default();
        assert!(
            answer.contains("[identify_subject]: subject identified"),
            "composed answer must carry the step results, got: {answer}"
        );

        // Every step is audited on the durable `router.audit` stream.
        let joined = lines.join("\n");
        assert!(joined.contains("router.audit"), "audit stream used: {joined}");
        assert!(joined.contains("tool_plan_step"), "per-step audit: {joined}");
        assert!(
            joined.contains("identify_subject"),
            "step id in audit detail: {joined}"
        );
        assert!(
            joined.contains("tool_plan_composed"),
            "compose audited: {joined}"
        );
    }

    #[test]
    fn exceeding_max_rounds_falls_back_to_plain_dispatch() {
        let plan = ToolPlan {
            steps: vec![
                dispatch_step("identify_subject"),
                dispatch_step("second"),
                compose_step(),
            ],
            max_rounds: Some(1),
        };
        let mock = mock_canned("plain answer");

        let (outcome, lines) = crate::test_support::capture_logs(|| {
            block(async {
                run_tool_plan(
                    &plan,
                    &plan_target(),
                    &test_request("hello"),
                    "test-model",
                    "hello",
                    Some(&mock),
                    false,
                    &deps(),
                    None,
                    None,
                    3,
                    &empty_lookups(),
                )
                .await
                .expect("infallible")
            })
        });

        assert!(outcome.response.status().is_success());
        // The budget cut the plan off after one round: no Compose ran, so the
        // answer is the plain-dispatch fallback — not a composed synthesis.
        let answer = outcome.answer_text.unwrap_or_default();
        assert_eq!(answer, "plain answer");

        let joined = lines.join("\n");
        assert!(
            joined.contains("tool_plan_fallback") && joined.contains("max_rounds_exceeded"),
            "round exhaustion must be audited as a fallback: {joined}"
        );
    }

    #[test]
    fn failed_dispatch_step_falls_back_to_plain_dispatch() {
        use crate::instances::stub::StubServer;
        let (outcome, lines) = crate::test_support::capture_logs(|| {
            block(async {
                let stub = StubServer::start(std::sync::Arc::new(
                    |_m: &str, path: &str, _b: &str| {
                        if path.ends_with("/chat/completions") {
                            (500, r#"{"error":{"message":"boom"}}"#.into())
                        } else {
                            (200, "{}".into())
                        }
                    },
                ));
                let mut target = plan_target();
                target.url = format!("{}/v1/chat/completions", stub.base_url());
                let plan = ToolPlan {
                    steps: vec![dispatch_step("identify_subject"), compose_step()],
                    max_rounds: Some(3),
                };
                run_tool_plan(
                    &plan,
                    &target,
                    &test_request("hello"),
                    "test-model",
                    "hello",
                    None,
                    false,
                    &deps(),
                    None,
                    None,
                    3,
                    &empty_lookups(),
                )
                .await
                .expect("infallible")
            })
        });

        // The failed step aborted the plan; the plain group dispatch was
        // attempted instead (it hits the same failing server, which degrades
        // to the canned fallback completion — a 200 with no step synthesis).
        let answer = outcome.answer_text.unwrap_or_default();
        assert!(
            !answer.contains("[identify_subject]"),
            "no composed answer may survive a failed plan: {answer}"
        );
        let joined = lines.join("\n");
        assert!(
            joined.contains("tool_plan_fallback") && joined.contains("step_failed"),
            "a failed step must be audited as a fallback to plain dispatch: {joined}"
        );
    }

    #[test]
    fn lookup_steps_are_audited_with_their_lookup_kind() {
        let step = ToolPlanStep {
            id: "lookup_or_dispatch".into(),
            kind: ToolPlanStepKind::Lookup,
            target_group: None,
            description: None,
            lookup_kind: Some("dag".into()),
            step_max_rounds: None,
        };
        let (_, lines) =
            crate::test_support::capture_logs(|| emit_step_audit("explain", &step, 1));
        let joined = lines.join("\n");
        assert!(joined.contains("router.audit"), "{joined}");
        assert!(joined.contains("tool_plan_step"), "{joined}");
        assert!(
            joined.contains("dag") && joined.contains("lookup_or_dispatch"),
            "lookup kind + step id must appear in the audit detail: {joined}"
        );
    }

    fn lookup_step(id: &str, kind: &str) -> ToolPlanStep {
        ToolPlanStep {
            id: id.into(),
            kind: ToolPlanStepKind::Lookup,
            target_group: None,
            description: None,
            lookup_kind: Some(kind.into()),
            step_max_rounds: None,
        }
    }

    #[test]
    fn unresolvable_lookup_plan_is_declined_to_plain_dispatch() {
        // A plan whose `Lookup` step names a kind with no installed resolver is
        // declined to plain group dispatch — never executed with a placeholder
        // (the fabricated-lookup leak is closed by construction).
        let plan = ToolPlan {
            steps: vec![lookup_step("lookup_dag", "dag")],
            max_rounds: Some(3),
        };
        let mock = mock_canned("plain answer");
        let (outcome, lines) = crate::test_support::capture_logs(|| {
            block(async {
                run_tool_plan(
                    &plan,
                    &plan_target(),
                    &test_request("hello"),
                    "test-model",
                    "hello",
                    Some(&mock),
                    false,
                    &deps(),
                    None,
                    None,
                    3,
                    &empty_lookups(),
                )
                .await
                .expect("infallible")
            })
        });

        assert!(outcome.response.status().is_success());
        let answer = outcome.answer_text.unwrap_or_default();
        assert_eq!(answer, "plain answer", "declined plan falls back to plain dispatch");
        assert!(
            !answer.contains("[lookup:"),
            "a declined plan must never produce a [lookup: placeholder, got: {answer}"
        );

        let joined = lines.join("\n");
        assert!(
            joined.contains("tool_plan_declined") && joined.contains("unresolved_lookup"),
            "decline must be audited with reason unresolved_lookup: {joined}"
        );
        assert!(
            joined.contains("dag"),
            "the unresolved kind must be named in the audit: {joined}"
        );
    }

    #[test]
    fn compose_with_empty_result_set_falls_back_cleanly() {
        // A `Lookup` step that resolves to nothing (absent) contributes no
        // material; the `Compose` step then has no real results and falls back
        // cleanly — an empty answer, never a synthesized half-answer or a
        // placeholder.
        #[derive(Debug)]
        struct AbsentLookup;
        impl crate::server::tool_lookup::ToolLookup for AbsentLookup {
            fn kind(&self) -> &str {
                "dag"
            }

            fn lookup(
                &self,
                _query: &str,
                _ctx: &crate::server::tool_lookup::LookupContext<'_>,
            ) -> Result<Option<crate::server::tool_lookup::LookupResult>, crate::server::tool_lookup::LookupError>
            {
                Ok(None)
            }
        }
        let mut lookups = empty_lookups();
        lookups.register(Arc::new(AbsentLookup));
        let plan = ToolPlan {
            steps: vec![lookup_step("lookup_dag", "dag"), compose_step()],
            max_rounds: Some(3),
        };
        let mock = mock_canned("plain answer");
        let (outcome, lines) = crate::test_support::capture_logs(|| {
            block(async {
                run_tool_plan(
                    &plan,
                    &plan_target(),
                    &test_request("hello"),
                    "test-model",
                    "hello",
                    Some(&mock),
                    false,
                    &deps(),
                    None,
                    None,
                    3,
                    &lookups,
                )
                .await
                .expect("infallible")
            })
        });

        assert!(outcome.response.status().is_success());
        let answer = outcome.answer_text.unwrap_or_default();
        assert!(
            !answer.contains("[lookup:"),
            "an absent lookup must never leave a [lookup: placeholder, got: {answer}"
        );
        assert!(
            !answer.contains("resolved"),
            "an absent lookup must not be synthesized, got: {answer}"
        );

        let joined = lines.join("\n");
        assert!(
            joined.contains("tool_plan_lookup") && joined.contains("\"present\":false"),
            "an absent lookup must be audited as absent: {joined}"
        );
    }

    #[test]
    fn e2e_plan_with_real_knowledge_graph_lookup_composes_real_node_text() {
        // End-to-end through a real `ContentNodeStore` (M5): a plan
        // `dispatch -> knowledge_graph lookup -> compose` produces an answer
        // that includes the lookup's real node LOD0 text and nothing
        // fabricated — no `[lookup:` placeholder, no synthesized summary.
        use crate::ledger::ContentNodeLedger;
        use crate::node_store::new_node;
        use crate::server::tool_lookup::{KnowledgeGraphLookup, ToolLookupRegistry};
        use crate::test_stubs::HashEmbedder;
        use fluent_llm::EmbeddingProvider;
        use fluent_types::NodeId;

        let ledger = Arc::new(ContentNodeLedger::open_in_memory().unwrap());
        let embedder = Arc::new(HashEmbedder::new(256));
        let mut node = new_node(
            NodeId::from_int(7001),
            "sess-kg",
            "r1",
            "assistant",
            "The capital of France is Paris.",
            Some(true),
        );
        // The node's embedding must live in the same vector space as the query
        // embedding (the HashEmbedder's), so the cosine KNN is
        // dimension-consistent.
        node.embedding = embedder.embed("The capital of France is Paris.").ok();
        ledger.record_content_node(&node).unwrap();

        let mut lookups = ToolLookupRegistry::new();
        lookups.register(Arc::new(KnowledgeGraphLookup::new(embedder)));

        let plan = ToolPlan {
            steps: vec![
                dispatch_step("identify_subject"),
                lookup_step("lookup_graph", "knowledge_graph"),
                compose_step(),
            ],
            max_rounds: Some(3),
        };
        // The mock must match the exact user_text (MockDispatchContext matches
        // on the literal user message).
        let mock = Arc::new(crate::testing::MockDispatchContext::new(
            vec![crate::testing::MockTranscriptEntry {
                user_message: "capital of France".into(),
                classifier_response: String::new(),
                dispatch_response: Some("subject identified".into()),
                ..Default::default()
            }],
            vec![],
        ));
        let (outcome, lines) = crate::test_support::capture_logs(|| {
            block(async {
                run_tool_plan(
                    &plan,
                    &plan_target(),
                    &test_request("capital of France"),
                    "test-model",
                    "capital of France",
                    Some(&mock),
                    false,
                    &deps(),
                    Some(&ledger),
                    None,
                    3,
                    &lookups,
                )
                .await
                .expect("run_tool_plan is infallible")
            })
        });

        assert!(outcome.response.status().is_success());
        let answer = outcome.answer_text.unwrap_or_default();
        assert!(
            answer.contains("The capital of France is Paris."),
            "the lookup's real node text must be composed, got: {answer}"
        );
        assert!(
            !answer.contains("[lookup:"),
            "no lookup placeholder may ever be composed, got: {answer}"
        );

        let joined = lines.join("\n");
        assert!(
            joined.contains("tool_plan_lookup") && joined.contains("\"present\":true"),
            "a resolved lookup must be audited as present: {joined}"
        );
        assert!(
            joined.contains("\"result_count\":1"),
            "the audit must count the resolved node: {joined}"
        );
        assert!(
            joined.contains("tool_plan_composed"),
            "the composition must be audited: {joined}"
        );
    }

    #[test]
    fn failed_lookup_step_degrades_to_plain_dispatch() {
        // A `Lookup` step that returns `Err` degrades to plain group dispatch —
        // never a composed half-answer.
        #[derive(Debug)]
        struct FailingLookup;
        impl crate::server::tool_lookup::ToolLookup for FailingLookup {
            fn kind(&self) -> &str {
                "dag"
            }

            fn lookup(
                &self,
                _query: &str,
                _ctx: &crate::server::tool_lookup::LookupContext<'_>,
            ) -> Result<Option<crate::server::tool_lookup::LookupResult>, crate::server::tool_lookup::LookupError>
            {
                Err(crate::server::tool_lookup::LookupError::Failed(
                    "dag".into(),
                    "backing store unreachable".into(),
                ))
            }
        }
        let mut lookups = empty_lookups();
        lookups.register(Arc::new(FailingLookup));
        let plan = ToolPlan {
            steps: vec![lookup_step("lookup_dag", "dag"), compose_step()],
            max_rounds: Some(3),
        };
        let mock = mock_canned("plain answer");
        let (outcome, lines) = crate::test_support::capture_logs(|| {
            block(async {
                run_tool_plan(
                    &plan,
                    &plan_target(),
                    &test_request("hello"),
                    "test-model",
                    "hello",
                    Some(&mock),
                    false,
                    &deps(),
                    None,
                    None,
                    3,
                    &lookups,
                )
                .await
                .expect("infallible")
            })
        });

        assert!(outcome.response.status().is_success());
        let answer = outcome.answer_text.unwrap_or_default();
        assert_eq!(answer, "plain answer", "a failed lookup falls back to plain dispatch");
        assert!(
            !answer.contains("[lookup:") && !answer.contains("lookup_dag"),
            "a failed lookup must not be composed: {answer}"
        );

        let joined = lines.join("\n");
        assert!(
            joined.contains("tool_plan_fallback") && joined.contains("lookup_failed"),
            "a failed lookup must be audited as a fallback: {joined}"
        );
    }
}
