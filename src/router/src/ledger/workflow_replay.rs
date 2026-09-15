//! Workflow replay loop (M14a): record successful runs as `Target` DAGs,
//! replay near-neighbor runs without frontier calls.
//!
//! The loop closes VISION's "Post-processing: audit + workflow extraction"
//! (§"The Escalation Ladder") and the plan route's "matched against a library
//! of prior workflows": a validated run decomposes into `Target` nodes
//! (`depends`/`provides`), stores as the query embedding + DAG in the shared
//! [`WorkflowStore`](super::workflow_store::WorkflowStore) and as `ContentNode`
//! steps in the ledger, and a future near-neighbor query replays the DAG
//! steps locally — skipping the frontier call when the same decomposition
//! structure applies.
//!
//! Explicitly behavior-changing, behind [`ReplayConfig::enabled`] (default
//! off — when off every entry point is a no-op returning `None`/empty and no
//! request path changes). The replay gate combines both axes separately,
//! never blended: **[task-value]** (embedding distance to the stored query,
//! validated-reuse count) with **[self-doubt]** (current plan confidence) —
//! a low-confidence plan never replays even a high-success workflow.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bitvec::vec::BitVec;
use fluent_dag::dep_graph::DependencyGraph;
use fluent_dag::target::{Target, TargetRegistry};
use fluent_types::{AnnotationClaim, ContentNode, ProvenanceTier};
use serde::{Deserialize, Serialize};

use super::workflow_store::{gated_insert_allowed, WorkflowEntry, WorkflowStore};

/// Calibration date for the replay-gate numbers below (M14-CAL): the
/// held-out synthetic traffic the threshold + reuse curves were measured on.
/// The gate tests pin these constants; re-calibration updates the date.
pub const CALIBRATION_DATE: &str = "2026-09-15";

/// Minimum cosine similarity (stored query ↔ incoming query) for replay.
/// Measured knee with safety margin (calibration report in the milestone
/// checklist): paraphrase band sits at ~0.997, the adversarial
/// near-neighbor band at ~0.933 — 0.96 clears the adversarial band with
/// headroom while keeping paraphrases.
pub const CALIBRATED_DISTANCE_THRESHOLD: f64 = 0.96;

/// Minimum validated reuses before a workflow may replay. One success could
/// be luck; two validated reuses plus the verified write gate is the bar.
pub const CALIBRATED_MIN_REUSES: u32 = 2;

/// Minimum current-plan confidence for replay (self-doubt axis). Blocks the
/// 0.10 low-confidence control with wide margin while passing confident
/// traffic; replay itself is cheap and deterministic, so the bar stays modest.
pub const CALIBRATED_MIN_CONFIDENCE: f64 = 0.5;

/// Replay configuration. Everything defaults off/closed: enabling is an
/// explicit operator act, and thresholds arrive pre-set from calibration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayConfig {
    /// Master flag. `false` (default) disables record and replay entirely.
    #[serde(default)]
    pub enabled: bool,
    /// Minimum cosine similarity for a near-neighbor hit.
    #[serde(default = "default_distance_threshold")]
    pub distance_threshold: f64,
    /// Minimum validated reuses before a workflow may replay.
    #[serde(default = "default_min_reuses")]
    pub min_reuses: u32,
    /// Minimum current-plan confidence (self-doubt axis).
    #[serde(default = "default_min_confidence")]
    pub min_confidence: f64,
}

fn default_distance_threshold() -> f64 {
    CALIBRATED_DISTANCE_THRESHOLD
}
fn default_min_reuses() -> u32 {
    CALIBRATED_MIN_REUSES
}
fn default_min_confidence() -> f64 {
    CALIBRATED_MIN_CONFIDENCE
}

impl Default for ReplayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            distance_threshold: default_distance_threshold(),
            min_reuses: default_min_reuses(),
            min_confidence: default_min_confidence(),
        }
    }
}

impl ReplayConfig {
    /// The calibrated gate: flag on, thresholds at the measured numbers.
    /// Unit + calibration tests pin this against the dated constants above.
    #[must_use]
    pub fn calibrated() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }
}

/// One recorded step of a solved run: the LOD0 prompt that was sent, the
/// LOD0 response, and its data-flow edges by asset name.
#[derive(Debug, Clone)]
pub struct RecordedStep {
    pub step_id: String,
    pub prompt_lod0: String,
    pub response_lod0: String,
    pub depends_on: Vec<String>,
    pub provides: Vec<String>,
    pub outcome_ok: bool,
}

/// Outcome of [`WorkflowReplay::record_workflow`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordOutcome {
    Stored,
    /// The steps do not form a DAG (dependency cycle) — never stored.
    RejectedCycle,
    /// The write gate refused (confidence / novelty / verified) — never stored.
    RejectedGate,
}

/// One replayed step: LOD0 text plus its LOD0 content hash, so replay inputs
/// reference full text by hash, never a summary.
#[derive(Debug, Clone)]
pub struct ReplayedStep {
    pub step_id: String,
    pub prompt_lod0: String,
    pub response_lod0: String,
    pub depends_on: Vec<String>,
    pub provides: Vec<String>,
    /// `fnv1a64` of the step's LOD0 text (same function as the node store's
    /// content hash, `0` for empty) — the LOD0 reference key.
    pub lod0_hash: u64,
}

/// A replay decision: the stored decomposition, ready to materialize.
#[derive(Debug, Clone)]
pub struct ReplayedWorkflow {
    pub audit_id: String,
    pub score: f64,
    pub reuses: u32,
    pub steps: Vec<ReplayedStep>,
    /// Provenance ceiling for replayed claims. Always `LocalModel`: replayed
    /// steps are local re-execution of a stored decomposition, never frontier
    /// output and never human review — enforced by construction (no setter,
    /// no parameter).
    pub provenance_cap: ProvenanceTier,
}

/// Resolution of a query against the replay library: either a replay hit or
/// the fresh-plan walk's outcome.
#[derive(Debug)]
pub enum Resolution<R> {
    Replayed(ReplayedWorkflow),
    Fresh(Option<R>),
}

/// The replay loop: a [`WorkflowStore`] plus per-workflow validated-reuse
/// counters (the N gate). All state is lock-guarded; no IO happens here —
/// embeddings and content nodes arrive from the caller, keeping tests
/// hermetic and the module transport-free.
pub struct WorkflowReplay {
    store: Arc<dyn WorkflowStore>,
    /// Validated reuses per `audit_id`: +1 per confirmed-good replay,
    /// saturating −1 per confirmed-bad one. Replay needs `>= min_reuses`.
    reuses: Mutex<HashMap<String, u32>>,
}

impl WorkflowReplay {
    pub fn new(store: Arc<dyn WorkflowStore>) -> Self {
        Self {
            store,
            reuses: Mutex::new(HashMap::new()),
        }
    }

    /// Record a solved run: validate the DAG, pass the write gate, store the
    /// embedding-keyed `Target` DAG, and (optionally) persist one `ContentNode`
    /// per step referencing the query's LOD0 content hash. No-op `Stored`
    /// requires `cfg.enabled` — recording is part of the flagged loop.
    #[allow(clippy::too_many_arguments)]
    pub fn record_workflow(
        &self,
        store: &dyn WorkflowStore,
        node_store: Option<&crate::node_store::ContentNodeStore>,
        query_embedding: &[f32],
        audit_id: &str,
        steps: &[RecordedStep],
        assembler_confidence: f64,
        verified: bool,
        session_id: &str,
        query_content_hash: u64,
    ) -> RecordOutcome {
        if steps.is_empty() {
            return RecordOutcome::RejectedGate;
        }
        let Some(dag) = build_target_dag(steps) else {
            return RecordOutcome::RejectedCycle;
        };
        let distance_to_nearest = nearest_distance(store, query_embedding);
        if !gated_insert_allowed(assembler_confidence, distance_to_nearest, verified) {
            return RecordOutcome::RejectedGate;
        }
        let entry = WorkflowEntry {
            query_embedding: query_embedding.to_vec(),
            dag,
            audit_id: audit_id.to_string(),
            verified,
        };
        if store.insert(entry).is_err() {
            return RecordOutcome::RejectedGate;
        }
        if let Some(nodes) = node_store {
            for step in steps {
                let node = replay_step_node(step, session_id, audit_id, query_content_hash);
                let _ = nodes.record_content_node(&node);
            }
        }
        crate::audit::emit(
            "workflow_record",
            serde_json::json!({ "audit_id": audit_id, "steps": steps.len() }),
        );
        RecordOutcome::Stored
    }

    /// Attempt a replay: flag off → `None`; PII/flagged query → `None`;
    /// nearest verified neighbor below threshold → `None`; reuses below N →
    /// `None`; current confidence below floor → `None`. Otherwise the stored
    /// decomposition with its reuse count. Steps come back in dependency
    /// order (the stored DAG order is topological by construction).
    pub fn try_replay(
        &self,
        query_text: &str,
        query_embedding: &[f32],
        current_confidence: f64,
        cfg: &ReplayConfig,
    ) -> Option<ReplayedWorkflow> {
        if !cfg.enabled {
            return None;
        }
        // Must-NOT-replay control: flagged (PII-bearing etc.) queries never
        // replay, however near the neighbor.
        if crate::ledger_guard::scrub_for_ledger(query_text).flagged {
            return None;
        }
        if current_confidence < cfg.min_confidence {
            return None;
        }
        let (entry, score) = self
            .store
            .nearest_verified(query_embedding, 3, cfg.distance_threshold)
            .into_iter()
            .next()?;
        let reuses = self.reuse_count(&entry.audit_id);
        if reuses < cfg.min_reuses {
            return None;
        }
        let steps = entry
            .dag
            .iter()
            .map(|t| {
                let envelope = split_step_envelope(t);
                ReplayedStep {
                    step_id: t.name.to_string(),
                    prompt_lod0: envelope.prompt,
                    response_lod0: envelope.response,
                    depends_on: envelope.depends_on,
                    provides: envelope.provides,
                    lod0_hash: lod0_hash_of(&t.command),
                }
            })
            .collect();
        crate::audit::emit(
            "workflow_replay",
            serde_json::json!({
                "audit_id": entry.audit_id,
                "score": score,
                "reuses": reuses,
            }),
        );
        Some(ReplayedWorkflow {
            audit_id: entry.audit_id,
            score,
            reuses,
            steps,
            provenance_cap: ProvenanceTier::LocalModel,
        })
    }

    /// Feedback: a confirmed-good replay banks one validated reuse; a
    /// confirmed-bad one demotes (saturating), so a decaying workflow falls
    /// back under N and stops replaying.
    pub fn note_reuse_outcome(&self, audit_id: &str, ok: bool) {
        let mut guard = common_core::sync::lock(&self.reuses);
        let counter = guard.entry(audit_id.to_string()).or_insert(0);
        if ok {
            *counter = counter.saturating_add(1);
        } else {
            *counter = counter.saturating_sub(1);
        }
    }

    fn reuse_count(&self, audit_id: &str) -> u32 {
        common_core::sync::lock(&self.reuses).get(audit_id).copied().unwrap_or(0)
    }

    /// Resolve a query: replay hit short-circuits (replay is deterministic
    /// reuse — cheaper than any fresh rung, tried before any LLM call, per
    /// the deterministic-first invariant); on a miss the fresh-plan walk runs
    /// through the canonical [`first_accept_in_order`](fluent_concurrency::ladder::first_accept_in_order)
    /// combinator — never a hand-rolled fallback loop.
    pub async fn resolve<F, R, E, Fut, Stop>(
        &self,
        query_text: &str,
        query_embedding: &[f32],
        current_confidence: f64,
        cfg: &ReplayConfig,
        fresh_rungs: impl IntoIterator<Item = F>,
        run: impl FnMut(F) -> Fut,
        stop: Stop,
    ) -> Result<Resolution<R>, E>
    where
        Fut: std::future::Future<Output = Result<Option<R>, E>> + Send,
        Stop: FnMut(&E) -> bool,
    {
        if let Some(replayed) = self.try_replay(query_text, query_embedding, current_confidence, cfg) {
            return Ok(Resolution::Replayed(replayed));
        }
        let fresh =
            fluent_concurrency::ladder::first_accept_in_order(fresh_rungs, run, stop).await?;
        Ok(Resolution::Fresh(fresh))
    }

    /// Materialize a replay decision into fresh `ContentNode`s: LOD0 carries
    /// the stored step text, metadata references the query LOD0 content hash
    /// (never a summary), and every claim is stamped at most `LocalModel`
    /// with producer `workflow_replay` — replayed steps can never outrank
    /// local execution, let alone frontier or human review.
    pub fn materialize(
        &self,
        replayed: &ReplayedWorkflow,
        session_id: &str,
        query_content_hash: u64,
    ) -> Vec<ContentNode> {
        let now = common_core::time::now_secs();
        replayed
            .steps
            .iter()
            .map(|step| {
                let mut node = crate::node_store::new_node(
                    fluent_types::NodeId::from_int(0),
                    session_id,
                    &format!("{}-{}", replayed.audit_id, step.step_id),
                    "assistant",
                    &format!("{}\n{}", step.prompt_lod0, step.response_lod0),
                    Some(true),
                );
                node.id = None;
                node.step_id = Some(step.step_id.clone());
                let claims = vec![
                    AnnotationClaim::confirmed(
                        format!("replay:{}", step.step_id),
                        ProvenanceTier::LocalModel,
                        serde_json::json!({
                            "audit_id": replayed.audit_id,
                            "score": replayed.score,
                            "reuses": replayed.reuses,
                            "lod0_hash": step.lod0_hash,
                        }),
                        "workflow_replay",
                        now,
                    ),
                ];
                node.metadata = Some(serde_json::json!({
                    "origin": "workflow_replay",
                    "kind": "workflow_replay_step",
                    "audit_id": replayed.audit_id,
                    "step_id": step.step_id,
                    "depends_on": step.depends_on,
                    "provides": step.provides,
                    "query_content_hash": query_content_hash,
                    "step_lod0_hash": step.lod0_hash,
                    "replay_claims": claims,
                }));
                node
            })
            .collect()
    }
}

/// Cosine distance (`1 − cosine`) to the nearest stored entry; `f64::INFINITY`
/// (always novel) when the library is empty.
fn nearest_distance(store: &dyn WorkflowStore, query_embedding: &[f32]) -> f64 {
    if query_embedding.is_empty() {
        return f64::INFINITY;
    }
    store
        .nearest(query_embedding, 1)
        .into_iter()
        .next()
        .map_or(f64::INFINITY, |(_, score)| 1.0 - score)
}

/// Build the `Target` DAG from recorded steps: the asset universe is the
/// ordered union of `provides` names; each step's `depends`/`provides` are
/// bitsets over it. `depends_on` names no step provides are external inputs
/// (re-provided at replay, recorded in metadata, not bit-indexed). A step's
/// own provides never satisfy its own depends (no self-edges). Returns `None`
/// on a dependency cycle or an empty asset universe — a replayable workflow
/// must be a non-trivial DAG.
fn build_target_dag(steps: &[RecordedStep]) -> Option<Vec<Target>> {
    if steps.is_empty() {
        return None;
    }
    let mut assets: Vec<&str> = Vec::new();
    for step in steps {
        for name in &step.provides {
            if !assets.contains(&name.as_str()) {
                assets.push(name);
            }
        }
    }
    if assets.is_empty() {
        return None;
    }
    let bit = |names: &[String], own: &[String]| {
        let mut bv = BitVec::repeat(false, assets.len());
        for name in names {
            if own.iter().any(|o| o == name) {
                continue;
            }
            if let Some(i) = assets.iter().position(|a| a == name) {
                bv.set(i, true);
            }
        }
        bv
    };
    // Provider map (first-wins): asset → step index.
    let mut provider: HashMap<&str, usize> = HashMap::new();
    for (i, step) in steps.iter().enumerate() {
        for name in &step.provides {
            provider.entry(name.as_str()).or_insert(i);
        }
    }
    // Cycle check over step-level edges via the shared DAG primitive (no
    // hand-rolled graph algorithm): an edge step → provider of each known,
    // non-self asset it consumes.
    let mut graph = DependencyGraph::<String>::new();
    for (i, step) in steps.iter().enumerate() {
        let mut deps: Vec<String> = Vec::new();
        for name in &step.depends_on {
            if step.provides.iter().any(|o| o == name) {
                continue;
            }
            if let Some(&p) = provider.get(name.as_str()) {
                if p != i && !deps.contains(&steps[p].step_id) {
                    deps.push(steps[p].step_id.clone());
                }
            }
        }
        if graph
            .register(&step.step_id, &deps, std::slice::from_ref(&step.step_id))
            .is_err()
        {
            return None;
        }
    }
    if graph.topo_sort().is_err() {
        return None;
    }
    let order = graph.topo_sort().ok()?;
    let by_id: HashMap<&str, &RecordedStep> =
        steps.iter().map(|s| (s.step_id.as_str(), s)).collect();
    let mut registry = TargetRegistry::new();
    let mut dag: Vec<Target> = Vec::with_capacity(steps.len());
    for (i, id) in order.iter().enumerate() {
        let step = by_id.get(id.as_str())?;
        let target = Target::new()
            .id(i as i64)
            .name(step.step_id.as_str().into())
            .target_type(fluent_types::TargetType::Phony)
            .executor(fluent_types::ExecutorKind::Native)
            .depends(bit(&step.depends_on, &step.provides))
            .provides(bit(&step.provides, &[]))
            .command(join_step_lod0(step))
            .essential(true)
            .build();
        if registry.register(target.clone()).is_err() {
            return None;
        }
        dag.push(target);
    }
    Some(dag)
}

/// Join a step's LOD0 pair plus its edge names into the `Target` command
/// payload: a JSON envelope (`prompt`/`response`/`depends_on`/`provides`).
/// The envelope keeps the entry self-describing — `WorkflowEntry` needs no
/// new field, so existing constructors are untouched, and replay recovers
/// step names without a side table. (Phony replay steps never execute
/// `command` as a shell; the payload is data by definition here.)
fn join_step_lod0(step: &RecordedStep) -> String {
    serde_json::json!({
        "prompt": step.prompt_lod0,
        "response": step.response_lod0,
        "depends_on": step.depends_on,
        "provides": step.provides,
    })
    .to_string()
}

/// Decoded step envelope; unknown shapes degrade to empty fields (a corrupt
/// payload replays nothing rather than erroring the request path).
#[derive(Default)]
struct StepEnvelope {
    prompt: String,
    response: String,
    depends_on: Vec<String>,
    provides: Vec<String>,
}

/// Split the command payload back into its envelope.
fn split_step_envelope(target: &Target) -> StepEnvelope {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&target.command) else {
        return StepEnvelope::default();
    };
    let strings = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|e| e.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    StepEnvelope {
        prompt: value.get("prompt").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        response: value.get("response").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        depends_on: strings("depends_on"),
        provides: strings("provides"),
    }
}

/// The `fnv1a64` LOD0 hash (same function + empty→0 rule as the node store's
/// content hash) — cite, don't fork.
fn lod0_hash_of(text: &str) -> u64 {
    if text.is_empty() {
        0
    } else {
        common_core::hash::fnv1a64(text.as_bytes())
    }
}

/// Build the ledger `ContentNode` for one recorded step: LOD0 is the step's
/// prompt + response, metadata links the query LOD0 hash and the data-flow
/// edges so replay resolves inputs against full text.
fn replay_step_node(
    step: &RecordedStep,
    session_id: &str,
    audit_id: &str,
    query_content_hash: u64,
) -> ContentNode {
    let mut node = crate::node_store::new_node(
        fluent_types::NodeId::from_int(0),
        session_id,
        &format!("{audit_id}-{}", step.step_id),
        "assistant",
        &format!("{}\n{}", step.prompt_lod0, step.response_lod0),
        Some(step.outcome_ok),
    );
    node.id = None;
    node.step_id = Some(step.step_id.clone());
    node.embedding = None;
    node.metadata = Some(serde_json::json!({
        "origin": "workflow_replay",
        "kind": "workflow_step",
        "audit_id": audit_id,
        "step_id": step.step_id,
        "depends_on": step.depends_on,
        "provides": step.provides,
        "query_content_hash": query_content_hash,
        "step_lod0_hash": lod0_hash_of(&step.prompt_lod0),
    }));
    node
}

#[cfg(test)]
#[path = "../../tests/workflow_replay.rs"]
mod tests;

#[cfg(test)]
#[path = "../../tests/workflow_replay_cal.rs"]
mod calibration_tests;
