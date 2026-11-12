//! Persisted work queue plus fixpoint loop driver.
//!
//! Every item is in exactly one VISION state (queued, in progress,
//! verified, failed, escalated). States persist in a
//! `CheckpointedStepGraph`, so a crash or interruption resumes rather
//! than restarts and finished work is never redone. Each pass handles
//! one bounded unit at the cheapest sufficient tier, verifies with the
//! caller's gate, records the outcome, and repeats until the fixpoint:
//! nothing queued and nothing blocked without an owner.
//!
//! One maintenance loop per workspace at a time: acquiring a second
//! driver while one is held is a typed error, never a second driver.
//! Execution composes the shared `SupervisedBatch` (failure containment)
//! with `StreamAbort` (pass-boundary cancellation).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use fluent_concurrency::batch::SupervisedBatch;
use fluent_concurrency::runtime::tokio::TokioRuntime;
use fluent_concurrency::stream::StreamAbort;
use fluent_dag::checkpointed::CheckpointedStepGraph;
use fluent_wvr::{
    impl_component, impl_fieldless, Describable, WorkContext, WorkError, WorkOutput, WorkUnit,
};

use crate::offload::{ApplyReport, EscalationPacket, ReviewSummary};
use crate::policy::MaintenancePolicy;

/// Shared act handle: the caller's gate verdict per unit and tier.
/// One `Arc` crosses the batch boundary; the loop never re-wraps it.
pub type ActFn = Arc<Mutex<dyn FnMut(&str, Tier) -> ActOutcome + Send>>;

/// VISION work-item states. Serializable so a run can resume from states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// VISION work-item states. Serializable so a run can resume from states.
pub enum WorkState {
    /// Waiting for dependencies, then for a worker pass.
    Queued,
    /// Executing in the current pass.
    InProgress,
    /// Gate green, finished, never redone.
    Verified,
    /// Gate red with attempts left.
    Failed,
    /// Handed up with an escalation packet; owned, never blocking.
    Escalated,
}

/// One unit of maintenance work.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
/// One unit of maintenance work.
pub struct WorkItem {
    /// Stable id (also the graph key).
    pub id: String,
    /// Component scope the narrowest gate covers.
    pub scope: String,
    /// Fix class (`rustfmt`, `clippy-fix`, …).
    pub kind: String,
    /// Dependency ids, in enqueue order.
    pub deps: Vec<String>,
    /// Current VISION state.
    pub state: WorkState,
    /// Act executions performed.
    pub attempts: u32,
    /// Last failure reason, when failed.
    pub last_error: Option<String>,
}

/// Queue error: duplicate ids and the single-loop guard.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
/// Queue error: duplicate ids and the single-loop guard.
pub enum QueueError {
    /// A second driver was acquired while one is held.
    #[error("maintenance loop already running")]
    AlreadyRunning,
    /// Duplicate work-item id.
    #[error("duplicate work item: {0}")]
    Duplicate(String),
}

/// Action tier for one pass. Tiers advance only on recorded failure:
/// nothing reaches `Escalate` without exhausting its attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Action tier for one pass. Tiers advance only on recorded failure:
/// nothing reaches `Escalate` without exhausting its attempts.
pub enum Tier {
    /// Deterministic fixers. Zero model calls.
    Deterministic,
    /// Small decision model triages what tools could not fix.
    Triage,
    /// Generative draft for policy-marked mechanical classes only.
    Draft,
    /// Hand up with an escalation packet. Never an act call.
    Escalate,
}

/// Tier for an item's next attempt: evidence-gated by construction.
#[must_use]
/// Tier for an item's next attempt: evidence-gated by construction.
pub fn tier_for(attempts: u32) -> Tier {
    match attempts {
        0 => Tier::Deterministic,
        1 => Tier::Triage,
        2 => Tier::Draft,
        _ => Tier::Escalate,
    }
}

/// One act execution outcome: the caller's gate verdict.
pub enum ActOutcome {
    /// Gate green with its reviewable report.
    Verified(ApplyReport),
    /// Gate red, attempts remain.
    Retryable(String),
    /// Judgment or policy refusal: escalate at once.
    Terminal(String),
    /// Backend busy or unreachable: wait without consuming an attempt.
    /// The item returns to queued and the report lists it under
    /// `model: unavailable`; the next run retries the same tier.
    Unavailable(String),
}

/// Exclusive driver handle: one loop per workspace. Held for the whole
/// run; dropping releases. A second acquire fails closed.
pub struct LoopGuard {
    _private: (),
}

static LOOP_HELD: AtomicBool = AtomicBool::new(false);

/// Serialize loop-driver tests: the single-loop guard is process-wide,
/// so tests that acquire it hold this mutex for their whole run.
#[cfg(test)]
/// Serialize loop-driver tests: the single-loop guard is process-wide,
/// so tests that acquire it hold this mutex for their whole run.
pub(crate) fn serial_loop_tests() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::OnceLock;
    static SERIAL: OnceLock<Mutex<()>> = OnceLock::new();
    common_core::sync::lock(SERIAL.get_or_init(|| Mutex::new(())))
}

impl LoopGuard {
    /// Acquire the single driver slot.
    pub fn acquire() -> Result<Self, QueueError> {
        LOOP_HELD
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self { _private: () })
            .map_err(|_| QueueError::AlreadyRunning)
    }
}

impl Drop for LoopGuard {
    fn drop(&mut self) {
        LOOP_HELD.store(false, Ordering::Release);
    }
}

/// Persisted queue: VISION states in the step graph, item bodies beside
/// it. Snapshots restore after a crash without redoing verified work.
pub struct MaintenanceQueue {
    graph: CheckpointedStepGraph<String, WorkState>,
    items: HashMap<String, WorkItem>,
    snapshot_path: Option<PathBuf>,
    last_snapshot_count: usize,
}

impl MaintenanceQueue {
    /// Empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph: CheckpointedStepGraph::new(),
            items: HashMap::new(),
            snapshot_path: None,
            last_snapshot_count: 0,
        }
    }

    /// Enqueue one unit with its dependencies. Duplicate ids are an error.
    pub fn enqueue(
        &mut self,
        id: &str,
        scope: &str,
        kind: &str,
        deps: &[&str],
    ) -> Result<(), QueueError> {
        if self.items.contains_key(id) {
            return Err(QueueError::Duplicate(id.to_string()));
        }
        let owned_deps: Vec<String> = deps.iter().map(ToString::to_string).collect();
        let graph_deps: Vec<String> = owned_deps.clone();
        self.graph
            .add_step(id.to_string(), &graph_deps, WorkState::Queued)
            .map_err(|_| QueueError::Duplicate(id.to_string()))?;
        self.items.insert(
            id.to_string(),
            WorkItem {
                id: id.to_string(),
                scope: scope.to_string(),
                kind: kind.to_string(),
                deps: owned_deps,
                state: WorkState::Queued,
                attempts: 0,
                last_error: None,
            },
        );
        Ok(())
    }

    /// Actionable items whose dependencies are verified, dependency-first
    /// with ascending blast radius (leaves before dependents): first runs
    /// converge small reviewable entries instead of one giant diff.
    /// `Failed` items keep their attempts and rerun while the tier ladder
    /// still grants them an act call; the `Escalate` tier never acts.
    #[must_use]
    pub fn ready(&self) -> Vec<String> {
        let ready: Vec<String> = self
            .graph
            .ready_steps()
            .into_iter()
            .filter(|id| {
                self.items
                    .get(id)
                    .is_some_and(|item| matches!(item.state, WorkState::Queued | WorkState::Failed))
            })
            .collect();
        // The graph already guarantees deps-complete; this only orders the
        // actionable set through the shared pipeline sorter (no second
        // ordering implementation). Unknown dep ids are absent from the
        // queue and count as no edge for ordering only.
        let paths: Vec<PathBuf> = ready.iter().map(PathBuf::from).collect();
        let ordered = crate::pipeline::order_for_waves(
            &paths,
            &|path| self.dependency_paths(path),
            &|path| self.dependent_paths(path),
        );
        let by_path: HashMap<PathBuf, Vec<String>> = {
            let mut map: HashMap<PathBuf, Vec<String>> = HashMap::new();
            for id in ready {
                map.entry(PathBuf::from(&id)).or_default().push(id);
            }
            map
        };
        let mut out = Vec::with_capacity(by_path.len());
        for path in ordered {
            if let Some(ids) = by_path.get(&path) {
                out.extend(ids.iter().cloned());
            }
        }
        out
    }

    /// Dependency ids of one queue item as paths (unknown ids skipped:
    /// absent from the queue, no edge for ordering).
    fn dependency_paths(&self, id_path: &Path) -> Vec<PathBuf> {
        let id = id_path.to_string_lossy();
        self.items
            .get(id.as_ref())
            .map(|item| {
                item.deps
                    .iter()
                    .filter(|dep| self.items.contains_key(*dep))
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Queue ids depending on one item, as paths (reverse of above).
    fn dependent_paths(&self, id_path: &Path) -> Vec<PathBuf> {
        let id = id_path.to_string_lossy();
        self.items
            .values()
            .filter(|item| item.deps.iter().any(|dep| dep == id.as_ref()))
            .map(|item| PathBuf::from(&item.id))
            .collect()
    }

    /// Whether the fixpoint holds: nothing queued, in progress, or failed.
    /// Verified and escalated items are terminal; failed items always own
    /// attempts while the ladder grants them, so nothing actionable hides.
    #[must_use]
    pub fn is_fixpoint(&self) -> bool {
        !self.items.values().any(|item| {
            matches!(
                item.state,
                WorkState::Queued | WorkState::InProgress | WorkState::Failed
            )
        })
    }

    /// Borrow one item's state.
    #[must_use]
    pub fn state(&self, id: &str) -> Option<WorkState> {
        self.items.get(id).map(|item| item.state)
    }

    /// Record an outcome decided outside the driver (the `run` command
    /// marks files verified or escalated after the wave loop certifies
    /// them). Attempts are untouched: this records, never retries.
    pub fn set_state(&mut self, id: &str, state: WorkState) {
        if let Some(item) = self.items.get_mut(id) {
            item.state = state;
        }
        if let Some(slot) = self.graph.state_mut(&id.to_string()) {
            *slot = state;
        }
        if matches!(state, WorkState::Verified | WorkState::Escalated) {
            self.graph.complete(&id.to_string());
        }
    }

    /// Set the crash-resume path: state-changing passes persist the
    /// queue here, best-effort (sidecars remain the source of truth).
    pub fn set_snapshot_path(&mut self, path: &std::path::Path) {
        self.snapshot_path = Some(path.to_path_buf());
        self.last_snapshot_count = usize::MAX;
    }

    /// Persist the queue snapshot now. Fails on I/O; the driver treats
    /// periodic writes as best-effort and keeps working.
    pub fn save_snapshot(&self, path: &std::path::Path) -> std::io::Result<()> {
        let raw = serde_json::to_string_pretty(&self.snapshot()).map_err(std::io::Error::other)?;
        common_core::io::write_atomic(path, raw.as_bytes()).map_err(std::io::Error::other)
    }

    /// Load a snapshot written by `save_snapshot`. A missing file is a
    /// fresh start, never an error; an unparseable file is. Load into
    /// an empty queue: duplicate ids error instead of merging.
    pub fn load_snapshot(&mut self, path: &std::path::Path) -> std::io::Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let raw = common_core::io::read_to_string_err(path).map_err(std::io::Error::other)?;
        let items: Vec<WorkItem> = serde_json::from_str(&raw).map_err(std::io::Error::other)?;
        self.restore(&items).map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
        })
    }

    /// Best-effort snapshot after state-changing passes: verified and
    /// escalated items are terminal, so only their growth writes.
    /// No path set means no I/O at all.
    fn maybe_snapshot(&mut self) {
        let Some(path) = self.snapshot_path.clone() else {
            return;
        };
        let progress = self
            .items
            .values()
            .filter(|item| matches!(item.state, WorkState::Verified | WorkState::Escalated))
            .count();
        if progress == self.last_snapshot_count {
            return;
        }
        if self.save_snapshot(&path).is_ok() {
            self.last_snapshot_count = progress;
        }
    }

    /// Snapshot states for crash-resume: verified work is never redone.
    #[must_use]
    pub fn snapshot(&self) -> Vec<WorkItem> {
        let mut items: Vec<WorkItem> = self.items.values().cloned().collect();
        items.sort_by(|left, right| left.id.cmp(&right.id));
        items
    }

    /// Restore from a snapshot, preserving states (resume, not restart).
    pub fn restore(&mut self, snapshot: &[WorkItem]) -> Result<(), QueueError> {
        for item in snapshot {
            let deps: Vec<&str> = item.deps.iter().map(String::as_str).collect();
            self.enqueue(&item.id, &item.scope, &item.kind, &deps)?;
            if let Some(stored) = self.items.get_mut(&item.id) {
                stored.state = item.state;
                stored.attempts = item.attempts;
                stored.last_error.clone_from(&item.last_error);
            }
            if matches!(item.state, WorkState::Verified | WorkState::Escalated) {
                self.graph.complete(&item.id);
            }
            if let Some(state) = self.graph.state_mut(&item.id) {
                *state = item.state;
            }
        }
        Ok(())
    }
}

impl Default for MaintenanceQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// Breaker policy for a loop run. Defaults are permissive: the structural
/// guarantees (attempt bound, tier-evidence escalation, max passes) carry
/// ordinary runs, and the breaker only fires when configured strict.
/// Calibrate strictly against a control set before trusting it in
/// production: an uncalibrated breaker stops converging runs.
#[derive(Debug, Clone)]
/// Breaker policy for a loop run. Defaults are permissive: the structural
/// guarantees (attempt bound, tier-evidence escalation, max passes) carry
/// ordinary runs, and the breaker only fires when configured strict.
/// Calibrate strictly against a control set before trusting it in
/// production: an uncalibrated breaker stops converging runs.
pub struct BreakerConfig {
    /// Consecutive non-certified passes that stop the loop.
    pub max_consecutive_failures: usize,
    /// Failure share above which the loop stops (0.0-1.0; above 1.0 disables).
    pub max_failure_rate: f64,
    /// Passes before the rate gate arms.
    pub rate_min_passes: usize,
    /// Passes without a verified outcome that stop the loop.
    pub max_stagnant_passes: usize,
    /// Distinct files terminally failing with one cause signature that
    /// stop the loop (`usize::MAX` disables the systemic wire).
    pub max_systemic_items: usize,
}

impl Default for BreakerConfig {
    fn default() -> Self {
        Self {
            max_consecutive_failures: usize::MAX,
            max_failure_rate: 2.0,
            rate_min_passes: usize::MAX,
            max_stagnant_passes: usize::MAX,
            max_systemic_items: usize::MAX,
        }
    }
}

impl BreakerConfig {
    /// Strict calibration for red-team control sets.
    #[must_use]
    pub fn strict() -> Self {
        Self {
            max_consecutive_failures: 3,
            max_failure_rate: 0.5,
            rate_min_passes: 4,
            max_stagnant_passes: 6,
            max_systemic_items: 3,
        }
    }
}

/// Loop circuit breaker: bounded attempts per item are enforced by the
/// tier ladder; this stops runs where the same failure repeats, the
/// revert share climbs, passes stop certifying anything, or one cause
/// terminally fails item after item (systemic). Trip reasons name the
/// wire; `systemic fingerprint` carries the cause signature.
#[derive(Debug)]
/// Loop circuit breaker: bounded attempts per item are enforced by the
/// tier ladder; this stops runs where the same failure repeats, the
/// revert share climbs, passes stop certifying anything, or one cause
/// terminally fails item after item (systemic). Trip reasons name the
/// wire; `systemic fingerprint` carries the cause signature.
pub struct LoopBreaker {
    config: BreakerConfig,
    consecutive_failures: usize,
    certified: usize,
    failed: usize,
    stagnant: usize,
    /// Cause signature to distinct terminally-failed files. Only
    /// attributed failures count: unattributed entries never trip.
    systemic: HashMap<String, HashSet<String>>,
}

impl LoopBreaker {
    /// New breaker over an explicit policy.
    #[must_use]
    pub fn new(config: BreakerConfig) -> Self {
        Self {
            config,
            consecutive_failures: 0,
            certified: 0,
            failed: 0,
            stagnant: 0,
            systemic: HashMap::new(),
        }
    }

    /// Observe one pass outcome: `true` when the pass certified an item.
    /// Returns the stop reason when a trip wire fires.
    pub fn observe(&mut self, certified: bool) -> Option<String> {
        self.observe_with_failing(certified, &[])
    }

    /// Observe one pass outcome plus this pass's terminal failures as
    /// `(file, cause-signature)` pairs. Empty signatures are unattributed
    /// and never trip: only one cause failing distinct items halts the
    /// run. Liveness signal only — it never judges decision quality.
    pub fn observe_with_failing(
        &mut self,
        certified: bool,
        failing: &[(String, String)],
    ) -> Option<String> {
        for (file, signature) in failing {
            if signature.is_empty() {
                continue;
            }
            self.systemic
                .entry(signature.clone())
                .or_default()
                .insert(file.clone());
        }
        if let Some(hit) = self
            .systemic
            .iter()
            .find(|(_, files)| files.len() >= self.config.max_systemic_items)
        {
            return Some(format!("systemic fingerprint {}", hit.0));
        }
        if certified {
            self.certified += 1;
            self.consecutive_failures = 0;
            self.stagnant = 0;
            return None;
        }
        self.failed += 1;
        self.consecutive_failures += 1;
        self.stagnant += 1;
        if self.consecutive_failures >= self.config.max_consecutive_failures {
            return Some("repeat failure".to_string());
        }
        let total = self.certified + self.failed;
        if total >= self.config.rate_min_passes
            && self.failed as f64 / total as f64 > self.config.max_failure_rate
        {
            return Some("revert rate".to_string());
        }
        if self.stagnant > self.config.max_stagnant_passes {
            return Some("no progress".to_string());
        }
        None
    }
}

/// Whether a stop reason is a breaker trip (resumable halt) rather
/// than an ordinary stop. Only the breaker's own named reasons count;
/// anything else (pass budgets, aborts, owner blocks) is work
/// remaining, never a halt.
#[must_use]
/// Whether a stop reason is a breaker trip (resumable halt) rather
/// than an ordinary stop. Only the breaker's own named reasons count;
/// anything else (pass budgets, aborts, owner blocks) is work
/// remaining, never a halt.
pub fn is_breaker_trip(reason: &str) -> bool {
    matches!(reason, "repeat failure" | "revert rate" | "no progress")
        || reason.starts_with("systemic fingerprint")
}

/// Fixpoint run report: what was verified, what was escalated, and why
/// the loop stopped. Serializable so the reviewable ledger persists
/// across runs — every applied change is listed, never implied.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
/// Fixpoint run report: what was verified, what was escalated, and why
/// the loop stopped. Serializable so the reviewable ledger persists
/// across runs — every applied change is listed, never implied.
pub struct LoopReport {
    /// Passes executed (one bounded unit each).
    pub passes: usize,
    /// Verified ids, in verification order.
    pub verified: Vec<String>,
    /// Escalated ids with reasons, in escalation order.
    pub escalated: Vec<(String, String)>,
    /// Reviewable ledger of verified writes.
    pub summary: ReviewSummary,
    /// Escalation packets, one per escalated item.
    pub packets: Vec<EscalationPacket>,
    /// Stop reason: `None` means the fixpoint.
    pub stopped: Option<String>,
    /// Clean files reprocessed without an invalidation cause (target: zero).
    /// Filled from index passes; defaulted so old ledgers still load.
    #[serde(default)]
    pub reprocessed_clean: u64,
    /// Model calls avoided by deterministic routing (rule wins plus
    /// cache hits). Evidence-routing measure, recorded apart from
    /// authority modes below.
    #[serde(default)]
    pub model_calls_avoided: u64,
    /// Decision-point authority modes (`point=mode`), recorded apart
    /// from evidence routing so legitimacy is never read as correctness.
    #[serde(default)]
    pub decision_modes: Vec<String>,
    /// Items that waited on an unavailable model (id plus backend
    /// reason, in wait order). Waiting consumes no attempts and never
    /// escalates: the next run retries the same tier. Defaulted so old
    /// ledgers still load.
    #[serde(default)]
    pub model_unavailable: Vec<(String, String)>,
}

impl LoopReport {
    /// Contract exit for the run: breaker trips halt (resumable, with
    /// verified work kept and model tiers stopped while deterministic
    /// tiers continue); escalations strand as stuck; any other stop is
    /// work remaining; a quiet loop is done.
    #[must_use]
    pub fn exit_code(&self) -> crate::seal::ExitCode {
        if let Some(reason) = &self.stopped {
            if is_breaker_trip(reason) {
                return crate::seal::ExitCode::Halted;
            }
            return crate::seal::ExitCode::WorkRemaining;
        }
        if self.escalated.is_empty() {
            crate::seal::ExitCode::Done
        } else {
            crate::seal::ExitCode::Stuck
        }
    }

    /// Compact human rendering: fixed, reverted, escalated, and why.
    #[must_use]
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut out = self.summary.render();
        let _ = writeln!(out, "passes: {}", self.passes);
        let _ = writeln!(out, "reprocessed clean: {}", self.reprocessed_clean);
        let _ = writeln!(out, "model calls avoided: {}", self.model_calls_avoided);
        if self.decision_modes.is_empty() {
            let _ = writeln!(out, "decision modes: unmeasured");
        } else {
            let _ = writeln!(out, "decision modes: {}", self.decision_modes.join(", "));
        }
        for (id, reason) in &self.escalated {
            let _ = writeln!(out, "escalated: {id} ({reason})");
        }
        for (id, reason) in &self.model_unavailable {
            let _ = writeln!(out, "model: unavailable: {id} ({reason})");
        }
        match &self.stopped {
            Some(reason) => {
                let _ = writeln!(out, "stopped: {reason}");
            }
            None => {
                let _ = writeln!(out, "fixpoint: clean");
            }
        }
        out
    }

    /// Fold one index pass's counters into the run report. Additive:
    /// callers merge each wave's stats as they land.
    pub fn merge_index_stats(&mut self, stats: &crate::index_pipeline::IndexStats) {
        self.reprocessed_clean += stats.reprocessed_clean as u64;
    }

    /// Fold one decision snapshot's routing counters into the run
    /// report: avoided calls are rule wins plus cache hits (never model
    /// answers). Authority modes arrive separately via `record_modes`.
    pub fn merge_decision_snapshot(
        &mut self,
        snapshot: &crate::query::decision_points::DecisionMetricsSnapshot,
    ) {
        self.model_calls_avoided += snapshot.rule_wins + snapshot.cache_hits;
    }

    /// Record decision-point authority modes (`point=mode` strings).
    /// Stored verbatim and rendered apart from evidence routing.
    pub fn record_modes(&mut self, modes: &[String]) {
        self.decision_modes.extend(modes.iter().cloned());
    }

    /// Persist the reviewable ledger: every applied change is listed, so
    /// a later run or a human audit replays exactly what happened.
    pub fn save_ledger(&self, path: &std::path::Path) -> std::io::Result<()> {
        let raw = serde_json::to_string_pretty(self)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        common_core::io::write_atomic(path, raw.as_bytes()).map_err(std::io::Error::other)
    }

    /// Reload a persisted ledger.
    pub fn load_ledger(path: &std::path::Path) -> std::io::Result<Self> {
        let raw = common_core::io::read_to_string_err(path).map_err(std::io::Error::other)?;
        serde_json::from_str(&raw)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    }
}

/// One pass's unit behind the uniform control-plane handle. The domain
/// verdict travels through the shared outcome slot; `execute` itself
/// only reports that the act ran.
struct StepUnit {
    name: String,
    tier: Tier,
    act: Arc<Mutex<dyn FnMut(Tier) -> ActOutcome + Send>>,
    outcome: Arc<Mutex<Option<ActOutcome>>>,
}

impl WorkUnit for StepUnit {
    fn name(&self) -> &str {
        &self.name
    }

    fn depends(&self) -> &[fluent_wvr::ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[fluent_wvr::ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        let verdict = self
            .act
            .lock()
            .map(|mut act| act(self.tier))
            .map_err(|_| WorkError::Execution("step act poisoned".to_string()))?;
        self.outcome
            .lock()
            .map(|mut slot| *slot = Some(verdict))
            .map_err(|_| WorkError::Execution("step outcome poisoned".to_string()))?;
        Ok(WorkOutput::ok(format!("{} done", self.name)))
    }
}

impl_fieldless!(StepUnit);

impl Describable for StepUnit {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({ "step": self.name })
    }
}

impl_component!(StepUnit);

/// First-run ratchet for a dirty tree: record the ledger baseline,
/// then converge leaf-first in capped waves. The baseline lets a later
/// run prove it introduced no giant diff — every applied edit lands
/// as its own small reviewable entry. Strict breaker, caller-sized
/// cap, same fixpoint driver as every other run.
pub fn run_first_run(
    guard: &LoopGuard,
    queue: &mut MaintenanceQueue,
    policy: &MaintenancePolicy,
    max_passes: usize,
    abort: &StreamAbort,
    act: &ActFn,
    baseline_path: &std::path::Path,
) -> std::io::Result<LoopReport> {
    let baseline = LoopReport {
        passes: 0,
        verified: Vec::new(),
        escalated: Vec::new(),
        summary: ReviewSummary::new(),
        packets: Vec::new(),
        stopped: None,
        reprocessed_clean: 0,
        model_calls_avoided: 0,
        decision_modes: Vec::new(),
        model_unavailable: Vec::new(),
    };
    baseline.save_ledger(baseline_path)?;
    let mut breaker = LoopBreaker::new(BreakerConfig::strict());
    Ok(run_to_fixpoint_with_breaker(
        guard,
        queue,
        policy,
        max_passes,
        abort,
        Some(&mut breaker),
        act,
    ))
}

/// Drive the queue to the fixpoint: observe what is stale (ready set),
/// act on one unit at the cheapest sufficient tier, verify with the
/// caller's gate, record, repeat. Stops with a reason when the loop
/// cannot converge instead of spinning. The breaker is permissive by
/// default; pass an explicit [`LoopBreaker`] to arm trip wires.
pub fn run_to_fixpoint(
    guard: &LoopGuard,
    queue: &mut MaintenanceQueue,
    policy: &MaintenancePolicy,
    max_passes: usize,
    abort: &StreamAbort,
    act: &ActFn,
) -> LoopReport {
    run_to_fixpoint_with_breaker(guard, queue, policy, max_passes, abort, None, act)
}

/// [`run_to_fixpoint`] with an explicit circuit breaker: repeat failure,
/// revert rate, stagnation, and systemic cause signatures stop the run
/// with a named reason instead of burning passes that certify nothing.
pub fn run_to_fixpoint_with_breaker(
    _guard: &LoopGuard,
    queue: &mut MaintenanceQueue,
    policy: &MaintenancePolicy,
    max_passes: usize,
    abort: &StreamAbort,
    mut breaker: Option<&mut LoopBreaker>,
    act: &ActFn,
) -> LoopReport {
    let mut report = LoopReport {
        passes: 0,
        verified: Vec::new(),
        escalated: Vec::new(),
        summary: ReviewSummary::new(),
        packets: Vec::new(),
        stopped: None,
        reprocessed_clean: 0,
        model_calls_avoided: 0,
        decision_modes: Vec::new(),
        model_unavailable: Vec::new(),
    };
    // Termination is structural, not hoped for: every pass strictly
    // reduces remaining work — attempts climb toward forced escalation
    // while verified and escalated sets grow monotonically. The loop
    // can only exit at the fixpoint or with a named stop below.
    loop {
        if abort.is_cancelled() {
            report.stopped = Some("aborted".to_string());
            break;
        }
        if report.passes >= max_passes {
            report.stopped = Some("max passes reached".to_string());
            break;
        }
        let ready = queue.ready();
        if ready.is_empty() {
            if !queue.is_fixpoint() {
                report.stopped = Some("blocked without owner".to_string());
            }
            break;
        }
        let id = ready[0].clone();
        let attempts = queue.items.get(&id).map_or(0, |item| item.attempts);
        let tier = tier_for(attempts);
        if tier == Tier::Escalate {
            escalate(queue, &mut report, &id, "attempts exhausted");
            queue.maybe_snapshot();
            report.passes += 1;
            if let Some(stop) = breaker.as_mut().and_then(|breaker| breaker.observe(false)) {
                report.stopped = Some(stop);
                break;
            }
            continue;
        }
        if let Some(item) = queue.items.get_mut(&id) {
            item.state = WorkState::InProgress;
        }
        if let Some(state) = queue.graph.state_mut(&id) {
            *state = WorkState::InProgress;
        }
        let outcome = execute_step(&id, tier, act);
        let certified = matches!(outcome, Some(ActOutcome::Verified(_)));
        match outcome {
            Some(ActOutcome::Verified(write)) => {
                if let Some(item) = queue.items.get_mut(&id) {
                    item.state = WorkState::Verified;
                    item.last_error = None;
                }
                if let Some(state) = queue.graph.state_mut(&id) {
                    *state = WorkState::Verified;
                }
                queue.graph.complete(&id);
                report.summary.push_report(&write);
                report.verified.push(id);
            }
            Some(ActOutcome::Retryable(reason)) => {
                record_retry(queue, &mut report, policy, &id, reason);
            }
            Some(ActOutcome::Terminal(reason)) => {
                escalate(queue, &mut report, &id, &reason);
            }
            Some(ActOutcome::Unavailable(reason)) => {
                // Waiting is not failing: no attempt consumed, no retry
                // recorded, no breaker signal — the item returns to queued
                // and the report names where it waited. Skips the shared
                // observe below so a busy backend never trips the failure
                // breaker; the pass bound still ends the run.
                if let Some(item) = queue.items.get_mut(&id) {
                    item.state = WorkState::Queued;
                    item.last_error = Some(reason.clone());
                }
                if let Some(state) = queue.graph.state_mut(&id) {
                    *state = WorkState::Queued;
                }
                report.model_unavailable.push((id.clone(), reason));
                report.passes += 1;
                continue;
            }
            None => {
                record_retry(
                    queue,
                    &mut report,
                    policy,
                    &id,
                    "step produced no verdict".to_string(),
                );
            }
        }
        // Crash-resume write: verified and escalated sets only grow, so
        // unchanged passes skip the write inside `maybe_snapshot`.
        queue.maybe_snapshot();
        if let Some(stop) = breaker
            .as_mut()
            .and_then(|breaker| breaker.observe(certified))
        {
            report.stopped = Some(stop);
            report.passes += 1;
            break;
        }
        report.passes += 1;
    }
    report
}

/// Execute one unit through the supervised batch: panic containment and
/// per-attempt timeout ride the shared supervisor, never inline code.
/// Returns `None` when the step produced no verdict (panic, drop).
fn execute_step(id: &str, tier: Tier, act: &ActFn) -> Option<ActOutcome> {
    let outcome: Arc<Mutex<Option<ActOutcome>>> = Arc::new(Mutex::new(None));
    let id_owned = id.to_string();
    let shared = Arc::clone(act);
    // Shared lock helper is `Sized`-bound; this mutex holds a
    // trait object, so explicit poison recovery stays here.
    let runner = move |tier: Tier| {
        shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)(&id_owned, tier)
    };
    let unit = StepUnit {
        name: format!("step-{id}"),
        tier,
        act: Arc::new(Mutex::new(runner)),
        outcome: Arc::clone(&outcome),
    };
    let summary = common_core::runtime::block_on_shared(async {
        let mut batch =
            SupervisedBatch::new(Arc::new(TokioRuntime), fluent_wvr::CapabilitySet::new());
        if batch.register(Arc::new(unit)).is_err() {
            return None;
        }
        Some(batch.await)
    })?;
    let _ = summary;
    let mut slot = common_core::sync::lock(&outcome);
    slot.take()
}

/// Record a retryable outcome: attempts climb the tier ladder, and the
/// item escalates instead of acting once the policy limit is reached.
fn record_retry(
    queue: &mut MaintenanceQueue,
    report: &mut LoopReport,
    policy: &MaintenancePolicy,
    id: &str,
    reason: String,
) {
    let attempts = queue.items.get(id).map_or(0, |item| item.attempts) + 1;
    if attempts >= policy.attempt_limit {
        escalate(queue, report, id, &reason);
        return;
    }
    if let Some(item) = queue.items.get_mut(id) {
        item.state = WorkState::Failed;
        item.attempts = attempts;
        item.last_error = Some(reason);
    }
    if let Some(state) = queue.graph.state_mut(&id.to_string()) {
        *state = WorkState::Failed;
    }
}

/// Escalate one item and everything blocked on it: nothing stays blocked
/// without an owner.
fn escalate(queue: &mut MaintenanceQueue, report: &mut LoopReport, id: &str, reason: &str) {
    let kind = queue
        .items
        .get(id)
        .map(|item| item.kind.clone())
        .unwrap_or_default();
    let dependents = queue.graph.cancel_dependents(&id.to_string());
    let mut escalated = vec![id.to_string()];
    escalated.extend(dependents);
    escalated.sort();
    escalated.dedup();
    for (index, member) in escalated.iter().enumerate() {
        if let Some(item) = queue.items.get_mut(member) {
            item.state = WorkState::Escalated;
            item.last_error = Some(reason.to_string());
        }
        if let Some(state) = queue.graph.state_mut(member) {
            *state = WorkState::Escalated;
        }
        queue.graph.complete(member);
        let mut packet = EscalationPacket::from_policy(member, "maintenance", &kind, reason);
        packet.remaining = escalated
            .iter()
            .enumerate()
            .filter(|(other, _)| *other != index)
            .map(|(_, id)| id.clone())
            .collect();
        report.packets.push(packet);
        report.escalated.push((member.clone(), reason.to_string()));
    }
}

#[cfg(test)]
#[path = "../tests/loop_safety.rs"]
mod loop_safety_tests;

#[cfg(test)]
#[path = "../tests/authorization_gates.rs"]
mod authorization_gates_tests;

#[cfg(test)]
#[path = "../tests/work_queue.rs"]
mod work_queue_tests;

#[cfg(test)]
#[path = "../tests/fixpoint.rs"]
mod fixpoint_tests;

#[cfg(test)]
#[path = "../tests/m0_hygiene.rs"]
mod m0_hygiene_tests;

#[cfg(test)]
#[path = "../tests/systemic_breaker.rs"]
mod systemic_breaker_tests;

#[cfg(test)]
#[path = "../tests/redteam_hardening.rs"]
mod hardening_tests;
