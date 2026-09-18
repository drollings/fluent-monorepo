//! Classifier-driven target matching — the in-role complexity ladder.
//!
//! VISION §"Target selection within a role": after the classifier resolves a
//! route to a role, each candidate target self-assesses the prompt's
//! complexity and defers to the next, more-intelligent member when the
//! assessed complexity exceeds its configured `intelligence`. The first target
//! whose `intelligence` meets or exceeds the assessed complexity — or the last
//! member of the role — actually answers, and its answer is recorded in the
//! session ledger.
//!
//! This module owns the *matching* concern only (§4.1 of the roadmap): the
//! pure selection core (`start_index` / `is_match`), the self-assessment
//! prompt + tolerant parse, and the I/O-bound `TargetMatcher` that runs the
//! climb. It deliberately knows nothing about HTTP dispatch, escalation, or
//! the ledger — those stay in `server/` and `dispatch/`.
//!
//! Both the flat classifier path (`stages::classifier`) and the
//! classification-tree engine (`stages::tree`) resolve through the *same*
//! `TargetMatcher` (DRY — one climbing implementation).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use common_core::registry::ConcurrentRegistry;
use fluent_concurrency::pool::{Limiter, ResultPool};
use fluent_llm::client::ChatBackend;
use fluent_llm::ChatMessage;
use fluent_wvr::prelude::*;

use crate::config::RoleMember;
use crate::config::RoutingConfig;
use crate::pipeline::RoutingTarget;
use crate::stages::common::{coerce_string, coerce_u8};

/// A single target's self-assessment of the request's complexity.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SelfAssessment {
    #[serde(default = "default_five")]
    pub complexity: u8,
    #[serde(default)]
    pub reason: String,
}

fn default_five() -> u8 {
    5
}

/// The fixed self-assessment prompt — NOT auto-constructed from children (the
/// self-assessment is not a tree classifier node). §4.2 verbatim.
///
/// M6.3: canonical home is `fluent_llm::prompt::build_self_assessment_prompt`;
/// re-exported here so existing `target_match::…` paths keep working.
pub use fluent_llm::prompt::build_self_assessment_prompt;

/// Tolerant parse of a target's self-assessment response. Mirrors
/// `parse_tree_verdict` (`stages/tree.rs`) through the shared
/// `fluent_llm::parse_typed` codec: direct deserialize fast path, then the
/// shared fence-strip → parse → extract pipeline, then the shared field
/// coercion so string-valued numbers ("7") survive.
pub fn parse_self_assessment(response: &str) -> Result<SelfAssessment, WorkError> {
    fluent_llm::parse_typed::<SelfAssessment>(response, &serde_json::Value::Null, |v| {
        if let Some(obj) = v.as_object_mut() {
            coerce_u8(obj, "complexity", 5);
            coerce_string(obj, "reason", "");
        }
    })
    .map_err(|e| WorkError::Execution(format!("self-assessment verdict parse error: {e}")))
}

/// One ordered member of a role the matcher climbs over.
///
/// `model_key` is the config key (`models`/role `models`), `model_name` the
/// resolved display name (`ModelEntry.name` or the key when absent).
#[derive(Debug, Clone)]
pub struct TargetCandidate {
    pub model_key: String,
    pub model_name: String,
    pub intelligence: u8,
    /// `cost_input + cost_output` — informational, used to keep the ordered
    /// role's cost ordering visible to callers and tests.
    pub cost: f64,
}

/// Build the ordered `TargetCandidate` list for a role — the DRY
/// shape both the flat classifier path and the classification-tree engine feed
/// the matcher. Role members fan out to their candidate keys first; members
/// with no `models` entry are skipped (defense-in-depth — the caller falls
/// back to static resolution when the resulting list is empty or
/// single-member).
pub fn candidates_for_role(routing: &RoutingConfig, role: &str) -> Vec<TargetCandidate> {
    candidates_for_keys(routing, &routing.role_dispatch_members(role))
}

/// The shared member-keys → candidates mapping: literal keys resolve through
/// their `models` entry, anything else (unknown keys, and — when called with
/// raw members — unexpanded sentinels) is skipped.
fn candidates_for_keys(routing: &RoutingConfig, keys: &[String]) -> Vec<TargetCandidate> {
    keys.iter()
        .filter_map(|key| {
            routing.entry_for_key(key).map(|entry| TargetCandidate {
                model_key: key.clone(),
                model_name: entry
                    .name
                    .clone()
                    .unwrap_or_else(|| fluent_types::model_key::split_model_key(key).0.to_string()),
                intelligence: entry.intelligence,
                cost: entry.cost_input + entry.cost_output,
            })
        })
        .collect()
}

/// Per-(session, role) most-recently-successful dispatch: the wire `model`
/// id of the target that last served the role *in that session*. Written by
/// the dispatch path on every `Ok` outcome (failed targets never record),
/// read by sentinel expansion at match time. A plain mutex map beside the
/// server stats — not a subsystem.
///
/// Sessions are unbounded over a server lifetime, so the map evicts the
/// oldest-stamped entry past [`RECENCY_CAPACITY`] (scanned on insert only
/// when full). Recency is a monotonic sequence, not wall-clock time: two
/// dispatches in the same wall-clock second previously tied, and the
/// eviction scan could remove the entry that was just re-recorded. Sequence
/// stamps are totally ordered, so the least-recently-written entry always
/// leaves. A failed `Last` target falls through to the next rung via the
/// existing fallback combinator.
/// Session → role → (serving model, sequence stamp).
type RecencyMap = HashMap<ArcIntern<str>, HashMap<ArcIntern<str>, (ArcIntern<str>, u64)>>;

#[derive(Debug, Default)]
pub struct RoleRecency {
    inner: std::sync::Mutex<RecencyMap>,
    seq: std::sync::atomic::AtomicU64,
}

/// Maximum `(session, role)` recency entries (see [`RoleRecency`]).
pub const RECENCY_CAPACITY: usize = 4096;

impl RoleRecency {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Mutex::new(HashMap::new()),
            seq: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Record a successful dispatch of `role` in `session`, served by the
    /// target whose wire `model` id is `model_wire`. Lock-serialized with
    /// reads — concurrent requests in one session observe one winner.
    pub fn record(&self, session: &str, role: &str, model_wire: &str) {
        let stamped = self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut inner = common_core::sync::lock(&self.inner);
        let session_key: ArcIntern<str> = ArcIntern::from(session);
        let role_key: ArcIntern<str> = ArcIntern::from(role);
        let model: ArcIntern<str> = ArcIntern::from(model_wire);
        inner
            .entry(session_key)
            .or_default()
            .insert(role_key, (model, stamped));
        if Self::len_locked(&inner) > RECENCY_CAPACITY {
            let mut oldest: Option<(ArcIntern<str>, ArcIntern<str>, u64)> = None;
            for (session_key, roles) in &*inner {
                for (role_key, (_, stamped)) in roles {
                    let replace = oldest.as_ref().is_none_or(|(_, _, min)| *stamped < *min);
                    if replace {
                        oldest = Some((session_key.clone(), role_key.clone(), *stamped));
                    }
                }
            }
            if let Some((session_key, role_key, _)) = oldest {
                if let Some(roles) = inner.get_mut(&session_key) {
                    roles.remove(&role_key);
                    if roles.is_empty() {
                        inner.remove(&session_key);
                    }
                }
            }
        }
    }

    /// The wire `model` id of the role's last successful dispatch in
    /// `session`, if any. An unknown session resolves to nothing — the
    /// caller falls back to the role head.
    pub fn last_for(&self, session: &str, role: &str) -> Option<String> {
        let inner = common_core::sync::lock(&self.inner);
        let session_key: ArcIntern<str> = ArcIntern::from(session);
        let role_key: ArcIntern<str> = ArcIntern::from(role);
        inner
            .get(&session_key)
            .and_then(|roles| roles.get(&role_key))
            .map(|(model_wire, _)| model_wire.to_string())
    }

    /// Record `model_wire` as the globally last-answered model, regardless
    /// of session or role. Backs `last` sentinels in sentinel-only roles
    /// and the `model: "last"` classifier duty — whichever LLM actually
    /// answered last, with no per-session scoping.
    pub fn record_any(&self, model_wire: &str) {
        let stamped = self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut inner = common_core::sync::lock(&self.inner);
        let session_key: ArcIntern<str> = ArcIntern::from("");
        let group_key: ArcIntern<str> = ArcIntern::from("");
        let model: ArcIntern<str> = ArcIntern::from(model_wire);
        inner
            .entry(session_key)
            .or_default()
            .insert(group_key, (model, stamped));
    }

    /// The globally last-answered model wire id, if any dispatch has been
    /// recorded.
    pub fn last_any(&self) -> Option<String> {
        self.last_for("", "")
    }

    /// Entry count (bounded by [`RECENCY_CAPACITY`]).
    pub fn len(&self) -> usize {
        let inner = common_core::sync::lock(&self.inner);
        Self::len_locked(&inner)
    }

    /// Whether no dispatch has been recorded yet.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn len_locked(inner: &RecencyMap) -> usize {
        inner.values().map(HashMap::len).sum()
    }
}

/// The supervisor liveness view a request carries: which managed models are
/// currently running. `None` (mock mode, no managed fleet) probes as
/// all-down, so `Any` degrades to config order.
#[derive(Clone, Default)]
pub struct LivenessProbe {
    supervisor: Option<Arc<crate::supervisor::LlamaServerSupervisor>>,
}

impl LivenessProbe {
    pub fn new(supervisor: Option<Arc<crate::supervisor::LlamaServerSupervisor>>) -> Self {
        Self { supervisor }
    }

    /// Whether the managed model behind a base key is currently running.
    /// `None` when the model is not managed.
    pub fn is_running(&self, base_key: &str) -> Option<bool> {
        self.supervisor
            .as_ref()
            .and_then(|s| s.is_running(base_key))
    }
}

/// Request-scoped availability view for sentinel expansion, assembled from
/// the per-request context channels the handler populates. Absent channels
/// (unit tests, paths that bypass the handler) degrade gracefully: no recency,
/// no session, and an all-down fleet, so expansion is identity for
/// sentinel-free roles.
#[derive(Clone, Default)]
pub struct RoleExpansion {
    recency: Option<Arc<RoleRecency>>,
    liveness: LivenessProbe,
    session: Option<String>,
}

/// The per-request context channels [`RoleExpansion::from_ctx`] reads.
pub const RECENCY_CTX_KEY: &str = "role_recency";
pub const LIVENESS_CTX_KEY: &str = "role_liveness";

impl RoleExpansion {
    pub fn from_ctx(ctx: &WorkContext) -> Self {
        Self {
            recency: ctx.get::<Arc<RoleRecency>>(RECENCY_CTX_KEY).cloned(),
            liveness: ctx
                .get::<LivenessProbe>(LIVENESS_CTX_KEY)
                .cloned()
                .unwrap_or_default(),
            // The handler stamps the effective session id into the request
            // before the pipeline runs, so the structured request always
            // carries it on served paths; `None` off those paths means
            // "unknown session" (Last falls back to the head). Read through
            // the typed-request cache (M14) — no per-call re-parse.
            session: crate::stages::common::typed_request(ctx).and_then(|r| r.session_id.clone()),
        }
    }

    pub fn recency(&self) -> Option<&RoleRecency> {
        self.recency.as_deref()
    }

    /// Effective session id for `last`-sentinel expansion, if the request
    /// carried one.
    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    /// Supervisor liveness for a base model key. Onnx roles never reach this
    /// probe — expansion treats them as loaded via the existing registry
    /// readiness, with no new probe.
    pub fn supervisor_loaded(&self, base_key: &str) -> bool {
        self.liveness.is_running(base_key) == Some(true)
    }
}

/// The wire `model` id a literal role member dispatches as: the built
/// target's `model` for `models` entries, the raw member string for in-process
/// onnx roles (served by the onnx backend, with no `models` entry). `None`
/// when the member resolves to nothing.
fn member_wire_id(routing: &RoutingConfig, member: &str) -> Option<String> {
    let (base, _) = fluent_types::model_key::split_model_key(member);
    if routing.onnx_keys.contains(base) {
        return Some(member.to_string());
    }
    routing.target_for_key(member).map(|rt| rt.model)
}

/// Whether a pool-managed base key is fittable for a cheap duty without a
/// cold load (task-value, not confidence): remote endpoints and onnx roles
/// are always servable — no load to trigger; pool-managed models
/// (`weights`/`hf_repo`) must already be running, because duties never
/// cold-load (dispatch loads on demand through its own readiness path).
/// Unknown keys are unfittable — nothing resolves them.
fn duty_fittable(routing: &RoutingConfig, base: &str, loaded: bool) -> bool {
    if loaded {
        return true;
    }
    if routing.onnx_keys.contains(base) {
        return true;
    }
    match routing.models.get(base) {
        None => false,
        Some(entry) => entry.weights.is_none() && entry.hf_repo.is_none(),
    }
}

/// The models key behind a recorded wire id (`<base>` for `base` and
/// `base:point` wires served from a `models` entry or onnx role). `None`
/// when the wire resolves to nothing — a removed model, never a candidate.
fn key_for_wire(routing: &RoutingConfig, wire: &str) -> Option<String> {
    let (base, _) = fluent_types::model_key::split_model_key(wire);
    if routing.models.contains_key(base) || routing.onnx_keys.contains(base) {
        return Some(base.to_string());
    }
    routing.target_for_key(wire).map(|_| wire.to_string())
}

/// Whether a recency rung may serve: the member resolves to a real target
/// and passes the capability bar. Onnx/registry members carry no
/// `intelligence` rating and pass fail-open here (candidate construction
/// downstream still owns unknown-key filtering).
fn rung_viable(routing: &RoutingConfig, member: &str, min_complexity: Option<u8>) -> bool {
    if member_wire_id(routing, member).is_none() {
        return false;
    }
    let (base, _) = fluent_types::model_key::split_model_key(member);
    match routing.models.get(base) {
        Some(entry) => entry.intelligence >= min_complexity.unwrap_or(0),
        None => true,
    }
}

/// The classifier-duty `last`/`any` resolution, shared by the tree engine's
/// per-request duty-key path: the globally last-answered model when it is
/// fittable without a cold load, else the fleet-viable member
/// (intelligence-passing, fittable-first, cheapest — the M1 rule with a
/// managed-aware residency probe). Recency is injected per request, never
/// stored: no boot-frozen duty key.
///
/// No complexity scope applies here by construction — the complexity estimate
/// is produced BY the classifier, so vetting the duty on it would be
/// circular. Capability for the duty is resolvability + fittability; the
/// dispatch ladder owns capability fit. `None` only on an empty fleet.
pub fn viable_last_key(
    routing: &RoutingConfig,
    recency: Option<&RoleRecency>,
    is_running: &dyn Fn(&str) -> bool,
) -> Option<String> {
    if let Some(wire) = recency.and_then(RoleRecency::last_any) {
        if let Some(key) = key_for_wire(routing, &wire) {
            let (base, _) = fluent_types::model_key::split_model_key(&key);
            if duty_fittable(routing, base, is_running(base)) {
                return Some(key);
            }
        }
    }
    let picked = crate::config::select_viable_member(
        "classifier",
        &["last".to_string()],
        &routing.models,
        None,
        Some(&|base| duty_fittable(routing, base, is_running(base))),
    )?;
    // Duties never cold-load: when the cheapest viable pick is a managed
    // model whose server is down (no remote/onnx/running candidate won),
    // fail closed instead of materializing weights for a duty. The caller
    // falls back to the literal sentinel (served by the boot default, whose
    // own call fails loudly when unreachable), and dispatch-time ensure
    // remains the only path that loads models.
    let (base, _) = fluent_types::model_key::split_model_key(&picked);
    if duty_fittable(routing, base, is_running(base)) {
        Some(picked)
    } else {
        tracing::warn!(
            target: "router.target_match",
            model = %picked,
            "classifier duty vetoed cold managed pick - no running model; failing closed",
        );
        None
    }
}

/// Expand a role's effective members (literal keys plus the `last`/`any`
/// availability sentinels) into ordered literal keys. Role-naming members
/// fan out to their candidate keys first; without sentinels the expansion is
/// identity, so sentinel-free configs resolve byte-identically.
///
/// `Last` resolves preference-ordered, each rung vetted (resolvability +
/// intelligence-pass; loaded-ness is deliberately NOT a veto — unloaded
/// members load on demand through the unchanged readiness path, while `Any`
/// keeps the loaded-first preference):
/// session-last → global-last → viable-first (the M1 rule over the role,
/// or the fleet for sentinel-only roles). A rung that fails vetting falls
/// to the next; when no recency rung serves, one `last-empty-fallback`
/// audit note is emitted (recency coverage signal, not the outcome — the
/// viable rung may still serve). No expiry heuristic: a failed target falls
/// to the next rung via the combinator.
/// - `Any` expands, in config order, to the currently-loaded members first
///   (supervisor `is_running`, plus onnx-registry members via the existing
///   readiness), then the remaining members in config order (which load on
///   demand through the unchanged readiness path).
/// - `is_running` is the supervisor probe over *base* model keys
///   (`is_running == Some(true)` counts as loaded).
/// - Sentinels only order candidates; the vetted climb underneath keeps
///   capability fit, so recency never outranks capability.
pub fn expand_role_keys(
    routing: &RoutingConfig,
    role: &str,
    recency: Option<&RoleRecency>,
    session: Option<&str>,
    is_running: &dyn Fn(&str) -> bool,
    min_complexity: Option<u8>,
) -> Vec<String> {
    use crate::config::RoleMember;
    if !routing.roles.contains_key(role) {
        return Vec::new();
    }
    let raw = routing.role_dispatch_members(role);
    let literals: Vec<&String> = raw
        .iter()
        .filter(|m| matches!(RoleMember::parse(m), RoleMember::Key(_)))
        .collect();
    // Memoized liveness: the viable rung and the `Any` partition share one
    // probe per base key per expansion (fewer supervisor locks; deterministic
    // probe sequences for the calibration corpus). Single-expansion scope —
    // no cross-request staleness.
    let probe_cache = std::cell::RefCell::new(HashMap::new());
    let memo_running = |base: &str| {
        if let Some(hit) = probe_cache.borrow().get(base) {
            return *hit;
        }
        let live = is_running(base);
        probe_cache.borrow_mut().insert(base.to_string(), live);
        live
    };
    let is_loaded = |member: &str| {
        let (base, _) = fluent_types::model_key::split_model_key(member);
        if routing.onnx_keys.contains(base) {
            return true;
        }
        memo_running(base)
    };
    // Viable-`last` pick, resolved once per expansion: session-last →
    // global-last → viable-first, each rung vetted (resolvable +
    // intelligence-pass). The viable rung reuses the M1 rule over the raw
    // members (sentinel-only roles resolve over the fleet); resident-first
    // ordering comes from the same loaded probe as `Any`. `bool` tracks
    // whether a recency rung served (drives the coverage audit below).
    let find_literal = |wire: &str| {
        literals
            .iter()
            .find(|m| member_wire_id(routing, m).as_deref() == Some(wire))
            .map(|m| m.as_str())
    };
    // Inline at each `and_then` site below (a `let`-bound closure would not
    // infer higher-ranked lifetimes for the combinator chain).
    fn vetted<'a>(
        routing: &RoutingConfig,
        member: &'a str,
        min_complexity: Option<u8>,
    ) -> Option<&'a str> {
        rung_viable(routing, member, min_complexity).then_some(member)
    }
    let wants_last = raw
        .iter()
        .any(|m| matches!(RoleMember::parse(m), RoleMember::Last));
    let (last_pick, session_hit, global_hit): (Option<String>, bool, bool) = if !wants_last {
        (None, true, true)
    } else if let Some(member) = recency
        .and_then(|r| session.and_then(|s| r.last_for(s, role)))
        .and_then(|wire| find_literal(&wire))
        .and_then(|m| vetted(routing, m, min_complexity))
    {
        (Some(member.to_string()), true, false)
    } else if let Some(member) = recency
        .and_then(RoleRecency::last_any)
        .and_then(|wire| find_literal(&wire))
        .and_then(|m| vetted(routing, m, min_complexity))
    {
        (Some(member.to_string()), false, true)
    } else {
        // Viable-first: "work with whatever is loaded rather than load
        // another", ordered by capability-fit, then loaded-ness, then cost.
        // For sentinel-only roles this resolves over the fleet.
        let viable = crate::config::select_viable_member(
            role,
            &raw,
            &routing.models,
            min_complexity,
            Some(&|base| is_loaded(base)),
        );
        (viable, false, false)
    };
    let mut out: Vec<String> = Vec::with_capacity(raw.len() + usize::from(last_pick.is_some()));
    let mut seen: HashSet<String> = HashSet::new();
    let push = |key: &str, out: &mut Vec<String>, seen: &mut HashSet<String>| {
        if seen.insert(key.to_string()) {
            out.push(key.to_string());
        }
    };
    // The coverage audit: the session rung missed (unknown session, stale
    // wire id, or no session channel at all) — or, for sentinel-only
    // roles with no session rung, the global rung missed. Emitted once
    // per expansion even when a later rung serves: it measures recency
    // coverage, not the outcome.
    let recency_missed = if literals.is_empty() {
        !global_hit
    } else {
        session.is_none_or(|_| !session_hit)
    };
    if wants_last && recency_missed {
        crate::audit::emit(
            "target_match",
            serde_json::json!({
                "stage": "expansion",
                "role": role,
                "reason": "last-empty-fallback",
            }),
        );
    }
    for member in &raw {
        match RoleMember::parse(member) {
            RoleMember::Key(key) => push(&key, &mut out, &mut seen),
            RoleMember::Last => {
                if let Some(pick) = &last_pick {
                    push(pick, &mut out, &mut seen);
                }
            }
            RoleMember::Any => {
                // Single partition pass: each literal is probed once, loaded
                // members keep config order ahead of the unloaded remainder.
                let (loaded, unloaded): (Vec<&&String>, Vec<&&String>) =
                    literals.iter().partition(|m| is_loaded(m));
                for m in loaded.into_iter().chain(unloaded) {
                    push(m, &mut out, &mut seen);
                }
            }
        }
    }
    out
}

/// The expanded-candidate variant of [`candidates_for_role`]: sentinel
/// members resolve to ordered literal keys first (session-scoped `last`
/// included), then the same entry-resolution mapping runs (unknown keys
/// skipped, onnx roles excluded — they carry no `models` entry and are
/// served outside the climb, as today).
pub fn expanded_candidates_for_role(
    routing: &RoutingConfig,
    role: &str,
    recency: Option<&RoleRecency>,
    session: Option<&str>,
    is_running: &dyn Fn(&str) -> bool,
    min_complexity: Option<u8>,
) -> Vec<TargetCandidate> {
    let keys = expand_role_keys(routing, role, recency, session, is_running, min_complexity);
    candidates_for_keys(routing, &keys)
}

/// One assessed step of the climb — the audit/observability payload.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AssessmentRecord {
    pub model_key: String,
    pub model_name: String,
    /// `None` when the self-assessment call failed or was unparseable
    /// (conservative: escalate to the next candidate).
    pub assessed: Option<u8>,
    pub reason: String,
    pub error: Option<String>,
    pub matched: bool,
}

/// The outcome of a ladder run: the matched primary target plus the full walk.
#[derive(Debug)]
pub struct TargetMatch {
    pub primary: RoutingTarget,
    pub assessments: Vec<AssessmentRecord>,
}

/// Step 1 of §4.1: the cheapest index whose `intelligence` meets the
/// classifier's complexity estimate.
///
/// Candidates are in config order (ascending cost and intelligence). Returns
/// `0` when `classifier_complexity` is absent or no member qualifies — the
/// cheapest candidate self-assesses first; the climb never skips a candidate
/// the classifier already ruled out as too weak when a stronger one is cheaper
/// (cost ordering governs within the group).
pub fn start_index(candidates: &[TargetCandidate], classifier_complexity: Option<u8>) -> usize {
    let Some(c) = classifier_complexity else {
        return 0;
    };
    candidates
        .iter()
        .position(|cand| cand.intelligence >= c)
        .unwrap_or(0)
}

/// The match rule for a single assessed candidate (§4.1 step 2c): a candidate
/// matches when its assessed complexity does not exceed its configured
/// `intelligence`. The "last member always matches" escape is enforced by the
/// matcher loop, not here — this stays a pure per-candidate rule.
pub fn is_match(candidate: &TargetCandidate, assessed: u8) -> bool {
    assessed <= candidate.intelligence
}

/// The ladder's per-candidate backend set, built once by `config::builder`
/// (the single `LlmClient` factory — DIP) and shared by every pipeline.
///
/// `by_key` holds one `ChatBackend` per model key referenced by any
/// role `models` member. `default` serves any candidate key absent from
/// `by_key`: the injected mock/transcript backend in mock runs (covering every
/// key), or a defense-in-depth real client in real mode — mirroring the
/// classification-tree engine's `default_client` fallback.
#[derive(Clone)]
pub struct TargetBackends {
    by_key: ConcurrentRegistry<String, Arc<dyn ChatBackend>>,
    default: Arc<dyn ChatBackend>,
}

impl TargetBackends {
    pub fn new(
        by_key: HashMap<String, Arc<dyn ChatBackend>>,
        default: Arc<dyn ChatBackend>,
    ) -> Self {
        let reg = ConcurrentRegistry::new();
        for (key, backend) in by_key {
            reg.insert(key, backend);
        }
        Self {
            by_key: reg,
            default,
        }
    }

    /// The backend for `model_key`, falling back to `default` when the key has
    /// no dedicated backend (mock/transcript injection, or an unbuilt model
    /// key). This is the single lookup rule — the matcher and any caller use
    /// exactly this one.
    pub fn get(&self, model_key: &str) -> Arc<dyn ChatBackend> {
        self.by_key
            .get(&model_key.to_string())
            .map_or_else(|| Arc::clone(&self.default), |b| b.as_ref().clone())
    }

    /// The number of dedicated per-key backends.
    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    /// Whether no dedicated per-key backends exist (mock/transcript mode, or
    /// a config with no role `models` members).
    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }
}

/// The I/O-bound half of the ladder: holds the per-candidate backend set (one
/// `ChatBackend` per candidate model key plus the `default` fallback — the
/// injected mock/transcript backend or a real client) and runs the climb under
/// a shared `Limiter`, exactly like the classifier stage's sync LLM call.
///
/// `Clone` is cheap (three `Arc`-ish fields + a `u64`), so the same matcher
/// can be shared between the flat classifier stage and the classification-tree
/// engine without re-provisioning backends.
#[derive(Clone)]
pub struct TargetMatcher {
    /// Per-model-key backends + the default for keys absent from the map
    /// (real mode: one `LlmClient` per group member, built by `config::builder`
    /// — the single DIP construction site).
    backends: TargetBackends,
    /// Bounds concurrent self-assessment calls (same primitive as the
    /// classifier stage).
    limiter: Arc<Limiter>,
    /// Per-assessment wall-clock budget.
    timeout_ms: u64,
}

impl TargetMatcher {
    pub fn new(backends: TargetBackends, limiter: Arc<Limiter>, timeout_ms: u64) -> Self {
        Self {
            backends,
            limiter,
            timeout_ms,
        }
    }

    /// Run the §4.1 ladder: start index → per-candidate self-assessment →
    /// match rule → tail fallbacks.
    ///
    /// NOTE (M8 exemption, M4.4 cited): this loop deliberately does NOT compose
    /// `fluent_concurrency::ladder::first_accept_in_order`
    /// (`fluent-concurrency/SKILL.md` §3.11 — start-index-skip +
    /// last-always-wins + per-rung audit is a documented exception, not a
    /// candidate for unification). It differs in
    /// three load-bearing ways: (1) start-index skip — iteration begins at
    /// `start_index(complexity)`, not rung 0; (2) last-always-wins — the
    /// final candidate matches unconditionally, so exhaustion yields a
    /// match rather than `Ok(None)`; (3) per-rung audit — every
    /// assessment (including non-matching ones) emits an `AssessmentRecord`
    /// plus an audit event, not just the winner.
    ///
    /// Concurrency shape: the reachable suffix (`candidates[start..]`) is
    /// assessed concurrently through a `ResultPool` (the same primitive
    /// team mode uses for fan-out), bounded by the existing `Limiter` — no
    /// new concurrency knob. The completed assessments are then walked
    /// **in candidate order**, so match decisions, the `assessments`
    /// vector, and audit content + order are byte-identical to the old
    /// sequential walk.
    ///
    /// Accepted tradeoff: every reachable candidate is assessed even when
    /// an early one matches — the sequential walk short-circuited the
    /// later LLM calls, the fan-out spends them. With typical group sizes
    /// (2–4 members) the wasted calls are bounded and the latency win
    /// (slowest assessment, not the sum) dominates. Later results are
    /// computed but discarded, never recorded or audited.
    ///
    /// Signal status: the assessed `complexity` is the candidate model's
    /// self-reported doubt about the prompt — producer self-doubt, not a
    /// measurement of answer correctness. Failure (call error or
    /// unparseable reply) escalates conservatively and is never cached or
    /// promoted into any persistent index as a correctness label.
    ///
    /// `route` is the resolved route name (the primary's `target_name`),
    /// `role` its dispatch role. `candidates` is the role's ordered member
    /// list. `routing` supplies the `ModelEntry`s for fallback construction and
    /// the cross-role `all_dispatch_targets` resilience list.
    pub fn match_target(
        &self,
        route: &str,
        role: &str,
        routing: &RoutingConfig,
        candidates: &[TargetCandidate],
        classifier_complexity: Option<u8>,
        user_text: &str,
    ) -> Option<TargetMatch> {
        if candidates.is_empty() {
            tracing::warn!(
                target: "router.pipeline.stage2.target_match",
                route = %route,
                role = %role,
                "target-matching ladder: empty candidate role",
            );
            return None;
        }

        let start = start_index(candidates, classifier_complexity);
        let suffix = &candidates[start..];
        // Positional: `results[i]` is the assessment of `suffix[i]`. The
        // fan-out only reorders *completion*; this vector restores candidate
        // order before anything observable happens.
        let results = self.assess_suffix(suffix, user_text);
        let mut assessments: Vec<AssessmentRecord> = Vec::with_capacity(candidates.len());
        let mut matched_index = candidates.len() - 1;

        for (offset, candidate) in suffix.iter().enumerate() {
            let i = start + offset;
            let assessed = &results[offset];
            let reason = assessed
                .as_ref()
                .ok()
                .map(|s| s.reason.clone())
                .unwrap_or_default();

            // Match rule: assessed <= intelligence, OR this is the last member
            // (the ladder always terminates — the last member always matches).
            let matched = i == candidates.len() - 1
                || assessed
                    .as_ref()
                    .is_ok_and(|s| is_match(candidate, s.complexity));

            let record = AssessmentRecord {
                model_key: candidate.model_key.clone(),
                model_name: candidate.model_name.clone(),
                assessed: assessed.as_ref().ok().map(|s| s.complexity),
                reason,
                // Self-assessment failure → conservative escalate.
                error: assessed.as_ref().err().map(ToString::to_string),
                matched,
            };
            crate::audit::emit(
                "target_match",
                serde_json::json!({
                    "stage": "assessment",
                    "route": route,
                    "role": role,
                    "model_key": candidate.model_key,
                    "model_name": candidate.model_name,
                    "intelligence": candidate.intelligence,
                    "assessed": record.assessed,
                    "reason": record.reason,
                    "error": record.error,
                    "matched": record.matched,
                }),
            );
            assessments.push(record);

            if matched {
                matched_index = i;
                break;
            }
        }

        let matched_candidate = &candidates[matched_index];
        let primary = Self::build_primary(matched_candidate, route, role, routing);
        let fallbacks = Self::build_fallbacks(
            routing,
            route,
            classifier_complexity,
            &primary,
            candidates,
            matched_index,
        );

        let mut match_target = primary;
        match_target.fallbacks = fallbacks;

        crate::audit::emit(
            "target_match",
            serde_json::json!({
                "stage": "match",
                "route": route,
                "role": role,
                "matched_model_key": matched_candidate.model_key,
                "matched_model_name": matched_candidate.model_name,
                "assessed": assessments[matched_index - start].assessed,
                "fallback_count": match_target.fallbacks.len(),
            }),
        );

        Some(TargetMatch {
            primary: match_target,
            assessments,
        })
    }

    /// Assess the reachable suffix concurrently, returning one outcome per
    /// candidate in suffix order. Single-candidate suffixes take the direct
    /// path (exactly one backend call, no runtime needed); longer suffixes
    /// fan out over a `ResultPool` whose workers run the same `assess`
    /// (same limiter, same timeout, same conservative-error mapping).
    /// Pool-level failures (queue full/closed, cancelled submit) degrade
    /// to per-candidate assessment errors — the walk escalates past them
    /// exactly as it does a failed LLM call.
    fn assess_suffix(
        &self,
        suffix: &[TargetCandidate],
        user_text: &str,
    ) -> Vec<Result<SelfAssessment, WorkError>> {
        if suffix.len() <= 1 {
            return suffix
                .iter()
                .map(|candidate| self.assess(candidate, user_text))
                .collect();
        }
        let jobs: Vec<(TargetCandidate, String)> = suffix
            .iter()
            .map(|candidate| (candidate.clone(), user_text.to_owned()))
            .collect();
        let fanout = Self::fanout_assessments(self.clone(), jobs);
        match tokio::runtime::Handle::try_current() {
            Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(|| handle.block_on(fanout))
            }
            // No ambient runtime (unit tests, CLI paths): a throwaway
            // multi-thread runtime drives the same fan-out. Sized to the
            // suffix — groups are 2–4 members, never a thread farm. A
            // failed build degrades to the sequential walk below (timing
            // only — decisions, records, and audit order are identical).
            Err(_) => match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(suffix.len())
                .enable_all()
                .build()
            {
                Ok(rt) => rt.block_on(fanout),
                Err(_) => suffix
                    .iter()
                    .map(|candidate| self.assess(candidate, user_text))
                    .collect(),
            },
            // Current-thread runtime: `block_on` cannot run here (and the
            // pool workers' own `run_sync` bridge needs multi-thread to
            // re-enter), so fall back to the sequential walk. Timing only —
            // decisions, records, and audit order are identical.
            Ok(_) => suffix
                .iter()
                .map(|candidate| self.assess(candidate, user_text))
                .collect(),
        }
    }

    /// Drive one `ResultPool` over the suffix jobs and collect outcomes in
    /// job order. One submitter task per job (so all submits overlap), one
    /// worker per job (so no submit ever blocks on pool capacity), then an
    /// in-order join — completion order never leaks into the results.
    async fn fanout_assessments(
        matcher: TargetMatcher,
        jobs: Vec<(TargetCandidate, String)>,
    ) -> Vec<Result<SelfAssessment, WorkError>> {
        let pool = Arc::new(ResultPool::new(
            fluent_concurrency::tokio_runtime(),
            jobs.len(),
            jobs.len(),
            move |job: (TargetCandidate, String)| {
                let matcher = matcher.clone();
                async move {
                    Ok::<Result<SelfAssessment, WorkError>, std::convert::Infallible>(
                        matcher.assess(&job.0, &job.1),
                    )
                }
            },
        ));
        let mut submitters = Vec::with_capacity(jobs.len());
        for job in jobs {
            let pool = Arc::clone(&pool);
            submitters.push(tokio::spawn(async move { pool.submit(job).await }));
        }
        let mut results = Vec::with_capacity(submitters.len());
        for submitter in submitters {
            let assessed = match submitter.await {
                Ok(Ok(assessed)) => assessed,
                Ok(Err(_)) | Err(_) => Err(WorkError::Execution(
                    "self-assessment fan-out submit failed — escalating".into(),
                )),
            };
            results.push(assessed);
        }
        if let Ok(pool) = Arc::try_unwrap(pool) {
            pool.shutdown().await;
        }
        results
    }

    /// One self-assessment call: build the fixed prompt, run through the
    /// shared limiter (mirroring `stages::classifier`'s sync call), bounded by
    /// `timeout_ms`. A failed/unparseable call is `Err` — the caller escalates
    /// conservatively.
    fn assess(
        &self,
        candidate: &TargetCandidate,
        user_text: &str,
    ) -> Result<SelfAssessment, WorkError> {
        let messages = vec![ChatMessage {
            role: "system".into(),
            content: build_self_assessment_prompt(user_text),
        }];
        let client = self.backends.get(&candidate.model_key);

        tracing::info!(
            target: "router.pipeline.stage2.target_match",
            model_key = %candidate.model_key,
            model_name = %candidate.model_name,
            input_len = user_text.len(),
            "target self-assessment request",
        );

        let call_start = std::time::Instant::now();
        let response = self.limiter.run_sync(|| async {
            let call = client.chat_complete(&messages);
            if self.timeout_ms > 0 {
                let timed =
                    tokio::time::timeout(Duration::from_millis(self.timeout_ms), async { call })
                        .await;
                if let Ok(r) = timed {
                    r
                } else {
                    tracing::warn!(
                        target: "router.pipeline.stage2.target_match",
                        model_key = %candidate.model_key,
                        timeout_ms = self.timeout_ms,
                        "target self-assessment timed out",
                    );
                    Err(fluent_llm::LlmError::Http(format!(
                        "self-assessment timed out after {}ms",
                        self.timeout_ms
                    )))
                }
            } else {
                call
            }
        });
        let latency_ms = call_start.elapsed().as_millis() as u64;

        let response = match response {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    target: "router.pipeline.stage2.target_match",
                    model_key = %candidate.model_key,
                    error = %e,
                    latency_ms = latency_ms,
                    "target self-assessment call failed — escalating to next candidate",
                );
                return Err(WorkError::Execution(e.to_string()));
            }
        };

        match parse_self_assessment(&response) {
            Ok(s) => {
                tracing::info!(
                    target: "router.pipeline.stage2.target_match",
                    model_key = %candidate.model_key,
                    complexity = s.complexity,
                    latency_ms = latency_ms,
                    "target self-assessment succeeded",
                );
                Ok(s)
            }
            Err(e) => {
                tracing::warn!(
                    target: "router.pipeline.stage2.target_match",
                    model_key = %candidate.model_key,
                    error = %e,
                    latency_ms = latency_ms,
                    "target self-assessment unparseable — escalating to next candidate",
                );
                Err(e)
            }
        }
    }

    /// Primary target for the matched candidate (§4.1 step 3): the matched
    /// model's `RoutingTarget` with `role = R` and `target_name = route`.
    fn build_primary(
        candidate: &TargetCandidate,
        route: &str,
        role: &str,
        routing: &RoutingConfig,
    ) -> RoutingTarget {
        let mut rt = match routing.target_for_key(&candidate.model_key) {
            Some(rt) => rt,
            None => RoutingTarget {
                url: String::new(),
                model: candidate.model_name.clone(),
                target_name: Some(candidate.model_key.clone()),
                role: None,
                params: None,
                instance: None,
                snapshot: None,
                id_slot: None,
                filter_thinking: false,
                retry_count: 0,
                retry_base_interval_s: 1,
                stream: true,
                idle_timeout_ms: fluent_llm::constants::DEFAULT_IDLE_TIMEOUT_MS,
                total_timeout_ms: fluent_llm::constants::DEFAULT_TOTAL_TIMEOUT_MS,
                api_key: None,
                fallbacks: vec![],
                is_onnx: false,
            },
        };
        rt.role = Some(role.to_string());
        rt.target_name = Some(route.to_string());
        rt
    }

    /// Fallback construction (§4.1 step 3): the matched index's more-intelligent
    /// role tail `R[i+1..=n]` as `RoutingTarget`s (mechanical-failure walk, in
    /// order), then any cross-role models from `all_dispatch_targets` not
    /// already included (dedup by model name, preserving today's cross-role
    /// resilience).
    fn build_fallbacks(
        routing: &RoutingConfig,
        route: &str,
        classifier_complexity: Option<u8>,
        primary: &RoutingTarget,
        candidates: &[TargetCandidate],
        matched_index: usize,
    ) -> Vec<RoutingTarget> {
        let mut result: Vec<RoutingTarget> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        seen.insert(primary.model.clone());

        for candidate in candidates.iter().skip(matched_index + 1) {
            let Some(rt) = routing.target_for_key(&candidate.model_key) else {
                continue;
            };
            if seen.insert(rt.model.clone()) {
                result.push(rt);
            }
        }

        for (name, _) in routing.all_dispatch_targets(route, classifier_complexity) {
            let Some(rt) = routing.target_for_key(&name) else {
                continue;
            };
            if seen.insert(rt.model.clone()) {
                result.push(rt);
            }
        }

        result
    }
}

/// Resolve a route name to a typed `RoutingTarget` through the target-matching
/// ladder when one is available, falling back to the static
/// cheapest-qualifying pick.
///
/// The ladder only runs for 2+ member roles (a single-member role has
/// nothing to climb — it resolves statically, byte-identical to the static
/// path, with no extra LLM call). The matcher never fails hard: on absence, an
/// unresolvable role, an empty candidate list, or an internal `None`, the
/// static `routing_target` path runs (defense in depth).
///
/// This is the one route→target resolution shared by the flat classifier, the
/// deterministic soft-redirect pre-filter, and the matrix-authoritative branch.
pub(crate) fn resolve_route_to_target(
    routing_config: &RoutingConfig,
    matcher: Option<&TargetMatcher>,
    route: &str,
    complexity: Option<u8>,
    user_text: &str,
    expansion: &RoleExpansion,
) -> Option<RoutingTarget> {
    if let Some(matcher) = matcher {
        if let Some(role) = routing_config.route_role(route) {
            let candidates = expanded_candidates_for_role(
                routing_config,
                role,
                expansion.recency(),
                expansion.session(),
                &|base| expansion.supervisor_loaded(base),
                complexity,
            );
            if candidates.len() >= 2 {
                if let Some(tm) = matcher.match_target(
                    route,
                    role,
                    routing_config,
                    &candidates,
                    complexity,
                    user_text,
                ) {
                    tracing::info!(
                        target: "router.pipeline.stage2",
                        route = %route,
                        role = %role,
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
    // Sentinel-bearing roles (the `last`/`any` availability vocabulary)
    // resolve through the runtime expansion — session/global recency and
    // fleet liveness — before the static fallback, so `last` serves
    // whichever LLM actually last answered rather than a fixed member.
    if let Some(role) = routing_config.route_role(route) {
        let declares_sentinel = routing_config.roles.get(role).is_some_and(|r| {
            r.effective_models()
                .iter()
                .any(|m| matches!(RoleMember::parse(m), RoleMember::Last | RoleMember::Any))
        });
        if declares_sentinel {
            let keys = expand_role_keys(
                routing_config,
                role,
                expansion.recency(),
                expansion.session(),
                &|base| expansion.supervisor_loaded(base),
                complexity,
            );
            if let Some(key) = keys.first() {
                if let Some(mut rt) = routing_config.target_for_key(key) {
                    rt.role = Some(role.to_string());
                    rt.target_name = Some(route.to_string());
                    return Some(rt);
                }
            }
        }
    }
    routing_config.routing_target(route, complexity)
}

#[cfg(test)]
#[path = "../tests/target_match.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/target_match_order.rs"]
mod target_match_order_tests;

#[cfg(test)]
#[path = "../tests/routing_sentinel_golden.rs"]
mod routing_sentinel_golden;
