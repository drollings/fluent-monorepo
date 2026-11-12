//! Watermark repair: classify, fetch, fill, report.
//!
//! The repair loop closes the gap between what the tape holds and what a
//! reader needs. [`classify`](crate::coverage::classify) says whether a tape
//! needs repair; this module decides what to fetch, fetches it through a
//! [`HistoryProvider`](crate::history::HistoryProvider), stores it under
//! [`WritePolicy::InsertIfAbsent`](crate::store::WritePolicy), and reports
//! what happened per symbol. Every step composes an existing primitive:
//!
//! | Requirement | Primitive |
//! |---|---|
//! | One repair pass at a time | an `AtomicBool` compare-and-swap in `run_once` |
//! | Bounded concurrent repairs | `ResultPool`, which also returns the report |
//! | Re-sorted, deterministic results | `sort_by_key` on `SymbolId` |
//! | Capped exponential backoff | `common_core :: retry :: capped_exp_backoff_ms` |
//! | Cancellation | `StreamAbort`, raced by the pool's submit |
//! | Capability gating | `StoreStorage` via the task-local set |
//!
//! Per-symbol exclusion uses idempotence rather than a lock: a second
//! concurrent repair of the same span re-appends the same bars, observes
//! `inserted == 0`, and reports [`RepairStatus::AlreadyComplete`]. The
//! store itself is synchronous, so no lock is held across any `.await`.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use common_core::retry::capped_exp_backoff_ms;
use fluent_concurrency::pool::ResultPool;
use fluent_concurrency::scope::scoped;
use fluent_concurrency::stream::StreamAbort;
use fluent_wvr::prelude::*;
use fluent_wvr::CURRENT_CAPS;

use crate::capability::{check_binding, check_present, StoreStorage};
use crate::coverage::{Coverage, FreshnessPolicy, GapClass};
use crate::error::StoreError;
use crate::history::{FetchRequest, HistoryError, HistoryProvider};
use crate::key::{SymbolId, TapeKey};
use crate::range::TimeRange;
use crate::store::{CandleStore, WritePolicy};
use crate::tape::MarketTape;

/// Why a repair pass fills. Mirrors the classifier's repair verdicts: the
/// worker consumes the verdict and never re-derives which spans to fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
/// Why a repair pass fills. Mirrors the classifier's repair verdicts: the
/// worker consumes the verdict and never re-derives which spans to fill.
pub enum RepairReason {
    /// The store holds no rows at all.
    Empty,
    /// The tail trails the clock past the lag bound.
    Stale,
    /// The ends are current but the middle is torn.
    Torn,
}

impl RepairReason {
    /// The verdict's reason, or `None` for [`GapClass::Fresh`] — a fresh
    /// tape carries no repair verdict, and the coordinator skips it.
    #[must_use]
    pub fn of_gap(class: &GapClass) -> Option<Self> {
        match class {
            GapClass::Empty => Some(Self::Empty),
            GapClass::Stale { .. } => Some(Self::Stale),
            GapClass::Torn { .. } => Some(Self::Torn),
            GapClass::Fresh => None,
        }
    }
}

/// What one symbol repair did. `Unavailable` is terminal — the provider has
/// no data for the span — while `Deferred` is retryable under backoff. One
/// un-backfillable symbol therefore reports and releases the pass instead
/// of pinning it in a long sleep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
/// What one symbol repair did. `Unavailable` is terminal — the provider has
/// no data for the span — while `Deferred` is retryable under backoff. One
/// un-backfillable symbol therefore reports and releases the pass instead
/// of pinning it in a long sleep.
pub enum RepairStatus {
    /// Bars were fetched and stored.
    Repaired,
    /// Nothing was stored because the span was already covered — including
    /// the concurrent-duplicate case, which idempotence resolves without a
    /// lock.
    AlreadyComplete,
    /// Terminal: the provider has no data for this span, or served bytes it
    /// cannot stand behind. Never retried.
    Unavailable,
    /// Retryable: the store was locked, the fetch timed out, the provider
    /// was unreachable. Back off and try again.
    Deferred,
}

/// One symbol's repair: what was wanted, what was attempted, what happened.
///
/// `wanted` is the window the caller needed; `spans` is what the pass
/// actually attempted — the servable subset after horizon and width bounds.
/// The two diverge exactly when a bound refused part of the want, so a
/// partial fill is visible here rather than silently truncated. `detail`
/// names the reason in words; statuses alone never carry the explanation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
/// One symbol's repair: what was wanted, what was attempted, what happened.
/// 
/// `wanted` is the window the caller needed; `spans` is what the pass
/// actually attempted — the servable subset after horizon and width bounds.
/// The two diverge exactly when a bound refused part of the want, so a
/// partial fill is visible here rather than silently truncated. `detail`
/// names the reason in words; statuses alone never carry the explanation.
pub struct SymbolRepair {
    /// The tape under repair, at its stored spacing.
    pub key: TapeKey,
    /// The window the caller needed.
    pub wanted: TimeRange,
    /// The spans this pass attempted, ascending.
    pub spans: Vec<TimeRange>,
    /// Why the pass filled.
    pub reason: RepairReason,
    /// What the pass did.
    pub status: RepairStatus,
    /// Repair attempts made by the pass. Zero means the pass never attempted
    /// — a duplicate held off by single-flight, a preset terminal entry, or
    /// a cancellation before the first attempt.
    pub attempts: u32,
    /// Rows newly stored by the pass.
    pub inserted: usize,
    /// Rows that collided with stored rows.
    pub conflicts: usize,
    /// Why this status, in words. Every refusal and every terminal verdict
    /// names its reason here.
    pub detail: String,
}

/// One pass's work: the symbol repairs to attempt, in caller order. Results
/// come back re-sorted by `SymbolId` regardless of completion order.
#[derive(Debug, Clone, serde::Serialize)]
/// One pass's work: the symbol repairs to attempt, in caller order. Results
/// come back re-sorted by `SymbolId` regardless of completion order.
pub struct RecoveryPlan {
    /// The symbol repairs to attempt.
    pub repairs: Vec<SymbolRepair>,
}

impl RecoveryPlan {
    /// Whether the plan holds no work.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.repairs.is_empty()
    }

    /// How many symbol repairs the plan holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.repairs.len()
    }
}

/// Bounds a repair pass is held to. Schema-configurable through the derived
/// `FieldAccess + Describable`, fail-closed through `Validate`, and
/// schedulable as a supervised unit through `WorkUnit`, like every other
/// config in this crate.
#[derive(Debug, Clone, bon::Builder, FieldAccess, Describable)]
#[builder(start_fn = new)]
/// Bounds a repair pass is held to. Schema-configurable through the derived
/// `FieldAccess + Describable`, fail-closed through `Validate`, and
/// schedulable as a supervised unit through `WorkUnit`, like every other
/// config in this crate.
pub struct RepairConfig {
    /// Widest span one repair attempt fills, in milliseconds. A wider span
    /// is refused with its width named, never clipped into a servable
    /// prefix: clipping would present a partial fill as a complete one.
    #[field(
        desc = "Widest span one repair attempt fills, in milliseconds",
        min = 1
    )]
    #[builder(default = 604_800_000)]
    pub max_repair_span_ms: u64,
    /// Concurrent symbol repairs inside one pass.
    #[field(desc = "Concurrent symbol repairs inside one pass", min = 1, max = 64)]
    #[builder(default = 4)]
    pub repair_concurrency: u32,
    /// Backoff base between retry attempts, in milliseconds: the first
    /// failure waits exactly this long.
    #[field(desc = "Backoff base between retry attempts, in milliseconds", min = 1)]
    #[builder(default = 5_000)]
    pub backoff_base_ms: u64,
    /// How many doublings the backoff takes before holding steady.
    #[field(desc = "Backoff doublings before holding steady", min = 0)]
    #[builder(default = 6)]
    pub backoff_max_shift: u32,
    /// Ceiling for any single backoff sleep, in milliseconds. The schedule
    /// saturates here instead of growing past what an operator would wait
    /// for without intervening.
    #[field(
        desc = "Ceiling for any single backoff sleep, in milliseconds",
        min = 1
    )]
    #[builder(default = 300_000)]
    pub backoff_ceiling_ms: u64,
}

impl fluent_wvr::validate::Validate for RepairConfig {
    type Err = String;

    fn validate(&self) -> Result<(), Self::Err> {
        if self.max_repair_span_ms < 1 {
            return Err("max_repair_span_ms must be at least 1".to_owned());
        }
        if self.repair_concurrency < 1 {
            return Err("repair_concurrency must be at least 1".to_owned());
        }
        if self.backoff_base_ms < 1 {
            return Err("backoff_base_ms must be at least 1".to_owned());
        }
        if self.backoff_ceiling_ms < self.backoff_base_ms {
            return Err("backoff_ceiling_ms must cover backoff_base_ms".to_owned());
        }
        Ok(())
    }
}

impl WorkUnit for RepairConfig {
    fn name(&self) -> &str {
        "repair_config"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        fluent_wvr::validate::check_for_execute(self, "repair config invalid")?;
        Ok(fluent_wvr::validate::valid_output("repair config valid"))
    }
}

impl_component!(RepairConfig);

/// Map a provider failure to its repair verdict. Terminal shapes stay
/// terminal: unusable provider bytes never heal across a backoff, so they
/// report instead of retrying.
#[must_use]
/// Map a provider failure to its repair verdict. Terminal shapes stay
/// terminal: unusable provider bytes never heal across a backoff, so they
/// report instead of retrying.
pub fn classify_history_error(err: &HistoryError) -> RepairStatus {
    if err.is_retryable() {
        RepairStatus::Deferred
    } else {
        RepairStatus::Unavailable
    }
}

/// Map a store failure to its repair verdict through the store's own retry
/// taxonomy: only contention is transient, everything else is a fact about
/// the world that a retry reproduces exactly.
#[must_use]
/// Map a store failure to its repair verdict through the store's own retry
/// taxonomy: only contention is transient, everything else is a fact about
/// the world that a retry reproduces exactly.
pub fn classify_store_error(err: &StoreError) -> RepairStatus {
    if err.is_retryable() {
        RepairStatus::Deferred
    } else {
        RepairStatus::Unavailable
    }
}

/// The backoff sleep after `failures` consecutive failures: the shared
/// capped exponential schedule, saturated at the configured ceiling.
/// `failures` is 0-based — the first failure waits exactly the base —
/// matching the schedule's own convention.
#[must_use]
/// The backoff sleep after `failures` consecutive failures: the shared
/// capped exponential schedule, saturated at the configured ceiling.
/// `failures` is 0-based — the first failure waits exactly the base —
/// matching the schedule's own convention.
pub fn retry_delay_ms(config: &RepairConfig, failures: u32) -> u64 {
    capped_exp_backoff_ms(config.backoff_base_ms, failures, config.backoff_max_shift)
        .min(config.backoff_ceiling_ms)
}

/// The spans of `want` a repair pass should fill, plus the verdict that
/// asked for them. `Fresh` carries no verdict — the pass skips the item —
/// but still names a leading headroom span when the wanted window starts
/// before the tape does: seeding headroom the tape never held is a fill,
/// not a repair verdict, and the on-demand path serves it without one.
#[must_use]
/// The spans of `want` a repair pass should fill, plus the verdict that
/// asked for them. `Fresh` carries no verdict — the pass skips the item —
/// but still names a leading headroom span when the wanted window starts
/// before the tape does: seeding headroom the tape never held is a fill,
/// not a repair verdict, and the on-demand path serves it without one.
pub fn spans_for_want(
    class: &GapClass,
    coverage: Option<&Coverage>,
    want: &TimeRange,
) -> (Option<RepairReason>, Vec<TimeRange>) {
    match class {
        GapClass::Empty => (Some(RepairReason::Empty), vec![*want]),
        GapClass::Stale { .. } | GapClass::Torn { .. } => {
            let reason = RepairReason::of_gap(class);
            let spans: Vec<TimeRange> = class
                .repair_spans()
                .iter()
                .filter_map(|span| span.intersect(want))
                .filter(|span| match (span.start_ms, span.end_ms) {
                    (Some(start), Some(end)) => start < end,
                    _ => false,
                })
                .collect();
            (reason, spans)
        }
        GapClass::Fresh => {
            let leading = match (coverage, want.start_ms) {
                (Some(cov), Some(start)) if start < cov.first.ts_ms => {
                    let end = want
                        .end_ms
                        .map_or(cov.first.ts_ms, |want_end| want_end.min(cov.first.ts_ms));
                    (start < end).then_some(TimeRange {
                        start_ms: Some(start),
                        end_ms: Some(end),
                    })
                }
                _ => None,
            };
            (None, leading.into_iter().collect())
        }
    }
}

/// Bound one repair span by the width bound and the provider horizon. A span
/// wider than the bound is refused with its width named; a span no horizon
/// covers is refused naming the horizon. A span partially covered serves its
/// servable subset — visible as `spans` diverging from `wanted` on the
/// report, caught loudly downstream by the seed rule when the fill comes up
/// short.
fn bound_span(
    span: TimeRange,
    max_span_ms: u64,
    horizon: Option<TimeRange>,
) -> Result<TimeRange, String> {
    let (Some(start), Some(end)) = (span.start_ms, span.end_ms) else {
        return Err(format!(
            "unbounded repair span {span:?}: refusing rather than filling without bounds"
        ));
    };
    if start >= end {
        return Err(format!("empty repair span {span:?}: nothing to fill"));
    }
    let width = u64::try_from(end - start).unwrap_or(u64::MAX);
    if width > max_span_ms {
        return Err(format!(
            "repair span [{start}, {end}) is {width}ms wide, past max_repair_span_ms {max_span_ms}: refusing, never truncating"
        ));
    }
    match horizon {
        None => Ok(span),
        Some(edge) => edge.intersect(&span).filter(|sub| match (sub.start_ms, sub.end_ms) {
            (Some(sub_start), Some(sub_end)) => sub_start < sub_end,
            _ => false,
        }).ok_or_else(|| {
            format!("repair span [{start}, {end}) sits outside the provider horizon {edge:?}: refusing")
        }),
    }
}

/// What one fetch-and-append did.
struct FillOutcome {
    /// Bars the provider served for the span.
    fetched: usize,
    /// Rows newly stored.
    inserted: usize,
    /// Rows that collided with stored rows.
    conflicts: usize,
    /// Rows the price scale refused.
    rejected: usize,
}

/// Why a fetch-and-append failed, by layer.
#[derive(Debug)]
/// Why a fetch-and-append failed, by layer.
enum FillError {
    /// The provider failed.
    History(HistoryError),
    /// The store failed.
    Store(StoreError),
}

/// Fetch every page of `span` and append the bars under `InsertIfAbsent`.
/// Page following stops at the first provider refusal — a page that will not
/// serve will not serve its remainder either — and an empty span serves an
/// empty outcome rather than an error, so the caller can tell "no data" from
/// "failed".
async fn fetch_and_append(
    store: &MarketTape,
    provider: &dyn HistoryProvider,
    key: TapeKey,
    span: TimeRange,
) -> Result<FillOutcome, FillError> {
    let mut fetched: Vec<protopyne_types::Bar> = Vec::new();
    let mut cursor = Some(span);
    while let Some(window) = cursor {
        let page = provider
            .fetch(FetchRequest { key, span: &window })
            .await
            .map_err(FillError::History)?;
        fetched.extend(page.bars);
        cursor = page.next;
    }
    if fetched.is_empty() {
        return Ok(FillOutcome {
            fetched: 0,
            inserted: 0,
            conflicts: 0,
            rejected: 0,
        });
    }
    let outcome = store
        .append(key, &fetched, WritePolicy::InsertIfAbsent)
        .map_err(FillError::Store)?;
    Ok(FillOutcome {
        fetched: fetched.len(),
        inserted: outcome.inserted,
        conflicts: outcome.conflicts,
        rejected: outcome.rejected,
    })
}

/// Fill headroom spans the tape never held, without a repair verdict: the
/// on-demand path's seeding fetch. Each span is bounded exactly like a
/// verdict span — width and horizon refusals name their reason — and any
/// provider or store failure reports as [`StoreError`], since a single-shot
/// fill never retries. Returns `(inserted, conflicts)`.
pub(crate) async fn fill_headroom(
    store: &MarketTape,
    provider: &dyn HistoryProvider,
    key: TapeKey,
    spans: &[TimeRange],
    max_span_ms: u64,
) -> Result<(usize, usize), StoreError> {
    let mut inserted_total = 0usize;
    let mut conflicts_total = 0usize;
    for span in spans {
        let bounded =
            bound_span(*span, max_span_ms, provider.horizon()).map_err(StoreError::History)?;
        let outcome = fetch_and_append(store, provider, key, bounded)
            .await
            .map_err(|err| match err {
                FillError::History(HistoryError::Io(io)) => StoreError::Io(io),
                FillError::History(other) => StoreError::History(other.to_string()),
                FillError::Store(store) => store,
            })?;
        if outcome.rejected > 0 {
            return Err(StoreError::History(format!(
                "provider served {} row(s) the market scale refuses: data or scale misconfigured",
                outcome.rejected
            )));
        }
        inserted_total += outcome.inserted;
        conflicts_total += outcome.conflicts;
    }
    Ok((inserted_total, conflicts_total))
}

/// Repair `spans` toward `wanted`: bound each span, fetch-and-append it, and
/// report. Rejected rows fail the span terminally — unrepresentable provider
/// data never heals, so it reports instead of looping. A span that fetched
/// nothing reports `Unavailable`: the provider has no data there. Otherwise
/// stored rows read `Repaired` and pure collisions read `AlreadyComplete`,
/// which is how a concurrent duplicate resolves without a lock.
pub(crate) async fn repair_spans(
    store: &MarketTape,
    provider: &dyn HistoryProvider,
    key: TapeKey,
    wanted: TimeRange,
    spans: &[TimeRange],
    reason: RepairReason,
    max_span_ms: u64,
) -> SymbolRepair {
    let report =
        |status: RepairStatus, detail: String, inserted: usize, conflicts: usize| SymbolRepair {
            key,
            wanted,
            spans: spans.to_vec(),
            reason,
            status,
            attempts: 1,
            inserted,
            conflicts,
            detail,
        };
    let mut fetched_total = 0usize;
    let mut inserted_total = 0usize;
    let mut conflicts_total = 0usize;
    if spans.is_empty() {
        return report(
            RepairStatus::AlreadyComplete,
            "no spans to fill: the wanted window is already covered".to_owned(),
            0,
            0,
        );
    }
    for span in spans {
        let bounded = match bound_span(*span, max_span_ms, provider.horizon()) {
            Ok(bounded) => bounded,
            Err(detail) => return report(RepairStatus::Unavailable, detail, 0, 0),
        };
        match fetch_and_append(store, provider, key, bounded).await {
            Err(FillError::History(err)) => {
                return report(
                    classify_history_error(&err),
                    err.to_string(),
                    inserted_total,
                    conflicts_total,
                );
            }
            Err(FillError::Store(err)) => {
                return report(
                    classify_store_error(&err),
                    err.to_string(),
                    inserted_total,
                    conflicts_total,
                );
            }
            Ok(outcome) => {
                if outcome.rejected > 0 {
                    return report(
                        RepairStatus::Unavailable,
                        format!(
                            "provider served {} row(s) the market scale refuses: data or scale misconfigured, refusing to loop on it",
                            outcome.rejected
                        ),
                        inserted_total,
                        conflicts_total,
                    );
                }
                fetched_total += outcome.fetched;
                inserted_total += outcome.inserted;
                conflicts_total += outcome.conflicts;
            }
        }
    }
    if fetched_total == 0 {
        return report(
            RepairStatus::Unavailable,
            "provider served no bars for the span: nothing to store".to_owned(),
            0,
            0,
        );
    }
    if inserted_total > 0 {
        report(
            RepairStatus::Repaired,
            format!("stored {inserted_total} bar(s), {conflicts_total} already held"),
            inserted_total,
            conflicts_total,
        )
    } else {
        report(
            RepairStatus::AlreadyComplete,
            format!("span already covered: {conflicts_total} colliding row(s), none stored"),
            0,
            conflicts_total,
        )
    }
}

/// One symbol's repair as a supervised unit: synchronous `execute`, one
/// attempt, never inline sleeps.
///
/// `execute` validates the repair bounds through `check_for_execute`,
/// gates on the root-bound permit before consulting the task-local set,
/// and then blocks on one fetch-and-append: the store is synchronous and
/// in-memory, so there is no blocking I/O to offload and no pool between
/// the worker and the tape. Supervision (timeout, retry, cancellation)
/// applies through the `WorkUnit` boundary unchanged. The retry loop lives
/// in the coordinator, where sleeping belongs.
#[derive(Debug, Clone)]
/// One symbol's repair as a supervised unit: synchronous `execute`, one
/// attempt, never inline sleeps.
/// 
/// `execute` validates the repair bounds through `check_for_execute`,
/// gates on the root-bound permit before consulting the task-local set,
/// and then blocks on one fetch-and-append: the store is synchronous and
/// in-memory, so there is no blocking I/O to offload and no pool between
/// the worker and the tape. Supervision (timeout, retry, cancellation)
/// applies through the `WorkUnit` boundary unchanged. The retry loop lives
/// in the coordinator, where sleeping belongs.
pub struct RepairWorker {
    store: Arc<MarketTape>,
    provider: Arc<dyn HistoryProvider>,
    permit: StoreStorage,
    key: TapeKey,
    wanted: TimeRange,
    spans: Vec<TimeRange>,
    reason: RepairReason,
    config: RepairConfig,
    name: String,
}

impl RepairWorker {
    /// Repair `spans` toward `wanted` for one tape. Zero-width spans are
    /// dropped at construction: there is nothing to fill, and carrying them
    /// would read as work.
    #[must_use]
    pub fn new(
        store: Arc<MarketTape>,
        provider: Arc<dyn HistoryProvider>,
        permit: StoreStorage,
        key: TapeKey,
        wanted: TimeRange,
        spans: Vec<TimeRange>,
        reason: RepairReason,
        config: RepairConfig,
    ) -> Self {
        let spans: Vec<TimeRange> = spans
            .into_iter()
            .filter(|span| match (span.start_ms, span.end_ms) {
                (Some(start), Some(end)) => start < end,
                _ => false,
            })
            .collect();
        let name = format!(
            "repair.{}.{}.{}spans",
            key.symbol_id.0,
            key.timeframe.code(),
            spans.len()
        );
        Self {
            store,
            provider,
            permit,
            key,
            wanted,
            spans,
            reason,
            config,
            name,
        }
    }

    /// The tape under repair.
    #[must_use]
    pub fn key(&self) -> TapeKey {
        self.key
    }

    /// Perform one repair attempt: no retry, no sleep. Shared by the
    /// supervised `execute` path and the coordinator's retry loop, so the
    /// two cannot disagree about what an attempt does.
    pub async fn repair_attempt(&self) -> SymbolRepair {
        repair_spans(
            &self.store,
            self.provider.as_ref(),
            self.key,
            self.wanted,
            &self.spans,
            self.reason,
            self.config.max_repair_span_ms,
        )
        .await
    }
}

impl WorkUnit for RepairWorker {
    fn name(&self) -> &str {
        &self.name
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        fluent_wvr::validate::check_for_execute(&self.config, "repair config invalid")?;
        check_binding(&self.permit, self.store.root()).map_err(WorkError::from)?;
        check_present(self.store.permit()).map_err(WorkError::from)?;
        let repair = common_core::runtime::block_on(self.repair_attempt());
        WorkOutput::typed("repair attempt complete", &repair)
    }
}

impl_fieldless!(RepairWorker);

impl Describable for RepairWorker {
    fn describe(&self) -> serde_json::Value {
        describe_work_unit(self.name(), self.depends(), self.provides(), None)
    }
}

impl_component!(RepairWorker);

/// Owns repair passes: planning, bounded fan-out, retry, and cancellation.
///
/// The coordinator holds the store, one provider per symbol, and the
/// repair bounds. A pass plans coverage into spans, repairs symbols through
/// a bounded pool, retries `Deferred` outcomes under capped backoff with a
/// connectivity re-check at the top of every iteration, and re-sorts results
/// by `SymbolId` before returning. `Unavailable` terminates its symbol
/// without backing off; [`cancel`](Self::cancel) releases the whole pass.
#[derive(Clone)]
/// Owns repair passes: planning, bounded fan-out, retry, and cancellation.
/// 
/// The coordinator holds the store, one provider per symbol, and the
/// repair bounds. A pass plans coverage into spans, repairs symbols through
/// a bounded pool, retries `Deferred` outcomes under capped backoff with a
/// connectivity re-check at the top of every iteration, and re-sorts results
/// by `SymbolId` before returning. `Unavailable` terminates its symbol
/// without backing off; [`cancel`](Self::cancel) releases the whole pass.
pub struct RecoveryCoordinator {
    store: Arc<MarketTape>,
    providers: HashMap<SymbolId, Arc<dyn HistoryProvider>>,
    config: RepairConfig,
    runtime: Arc<dyn fluent_wvr::Runtime>,
    abort: StreamAbort,
    in_flight: Arc<AtomicBool>,
}

impl std::fmt::Debug for RecoveryCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecoveryCoordinator")
            .field("providers", &self.providers.len())
            .field("config", &self.config)
            .field("cancelled", &self.abort.is_cancelled())
            .finish_non_exhaustive()
    }
}

impl RecoveryCoordinator {
    /// Coordinate repairs over `store` with one provider per symbol id. The
    /// configuration is validated fail-closed: an incoherent bound refuses
    /// here, not mid-pass.
    pub fn new(
        store: Arc<MarketTape>,
        providers: Vec<(SymbolId, Arc<dyn HistoryProvider>)>,
        config: RepairConfig,
    ) -> Result<Self, StoreError> {
        config.validate().map_err(StoreError::Config)?;
        Ok(Self {
            store,
            providers: providers.into_iter().collect(),
            config,
            runtime: fluent_concurrency::tokio_runtime(),
            abort: StreamAbort::new(),
            in_flight: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Drive pool workers on `runtime` instead of the default Tokio one.
    #[must_use]
    pub fn with_runtime(mut self, runtime: Arc<dyn fluent_wvr::Runtime>) -> Self {
        self.runtime = runtime;
        self
    }

    /// Cancel through a shared abort signal — typically one the caller also
    /// hands to the providers, so a single fire releases both sides.
    #[must_use]
    pub fn with_abort(mut self, abort: StreamAbort) -> Self {
        self.abort = abort;
        self
    }

    /// Release the current pass, if any. In-flight attempts observe it at
    /// the top of their next iteration and at the pool submit boundary, so
    /// cancellation never strands an attempt mid-append: appends are atomic
    /// batches either way.
    pub fn cancel(&self) {
        self.abort.cancel();
    }

    /// Whether cancellation has fired.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.abort.is_cancelled()
    }

    /// Plan one pass: read each tape's coverage, classify it, and name the
    /// spans to fill. A fresh tape contributes nothing — there is no verdict
    /// to act on. An empty tape with an unbounded want contributes a preset
    /// terminal entry rather than failing the whole plan: one unplannable
    /// symbol must not pin the pass. A store that cannot be reached fails the
    /// plan loudly instead of planning against imagined coverage.
    pub fn plan(
        &self,
        permit: &StoreStorage,
        items: &[(TapeKey, TimeRange)],
        now_ms: i64,
        freshness: FreshnessPolicy,
    ) -> Result<RecoveryPlan, StoreError> {
        check_binding(permit, self.store.root())?;
        let mut repairs = Vec::with_capacity(items.len());
        for (key, want) in items {
            let coverage = self.store.coverage(*key)?;
            let class =
                crate::coverage::classify(coverage.clone(), now_ms, key.timeframe, freshness);
            let (reason, spans) = spans_for_want(&class, coverage.as_ref(), want);
            let Some(reason) = reason else { continue };
            if spans.is_empty() && matches!(class, GapClass::Empty) && !want.is_bounded() {
                repairs.push(SymbolRepair {
                    key: *key,
                    wanted: *want,
                    spans: Vec::new(),
                    reason,
                    status: RepairStatus::Unavailable,
                    attempts: 0,
                    inserted: 0,
                    conflicts: 0,
                    detail: "empty tape with an unbounded want: refusing rather than filling without bounds".to_owned(),
                });
                continue;
            }
            let detail = match class {
                GapClass::Empty => "empty tape: filling the wanted window".to_owned(),
                GapClass::Stale { .. } => "stale tail: filling the behind span".to_owned(),
                GapClass::Torn { .. } => "torn tape: filling interior holes".to_owned(),
                GapClass::Fresh => "fresh tape: nothing to fill".to_owned(),
            };
            repairs.push(SymbolRepair {
                key: *key,
                wanted: *want,
                spans,
                reason,
                status: RepairStatus::Deferred,
                attempts: 0,
                inserted: 0,
                conflicts: 0,
                detail,
            });
        }
        Ok(RecoveryPlan { repairs })
    }

    /// Run one pass over `plan`, returning one report per symbol re-sorted by
    /// `SymbolId`. Single-flight: a pass already in flight owns the spans, so
    /// a duplicate reports `AlreadyComplete` without re-fetching — the next
    /// scheduled pass reconciles whatever the in-flight one reports. Terminal
    /// entries pass through untouched: plans are point-in-time decisions,
    /// and re-planning — not re-running — re-evaluates them.
    pub async fn run_once(&self, permit: &StoreStorage, plan: &RecoveryPlan) -> Vec<SymbolRepair> {
        if plan.is_empty() {
            return Vec::new();
        }
        if let Err(err) = check_binding(permit, self.store.root()) {
            let detail = err.to_string();
            return plan
                .repairs
                .iter()
                .map(|repair| SymbolRepair {
                    status: RepairStatus::Unavailable,
                    attempts: 0,
                    inserted: 0,
                    conflicts: 0,
                    detail: detail.clone(),
                    ..repair.clone()
                })
                .collect();
        }
        if self.in_flight.swap(true, Ordering::AcqRel) {
            return plan
                .repairs
                .iter()
                .map(|repair| SymbolRepair {
                    status: RepairStatus::AlreadyComplete,
                    attempts: 0,
                    inserted: 0,
                    conflicts: 0,
                    detail: "a repair pass is already in flight: duplicate reports no work"
                        .to_owned(),
                    ..repair.clone()
                })
                .collect();
        }
        let _guard = FlightGuard(Arc::clone(&self.in_flight));
        // Pool workers are spawned bare, so snapshot the caller's capability
        // set and install it into every handler: store effects stay gated
        // exactly as if the caller had run them inline. No set installed
        // reads as empty, and every effect is denied rather than defaulted.
        let caps = CURRENT_CAPS.try_with(Clone::clone).unwrap_or_default();
        let pool = ResultPool::new(
            Arc::clone(&self.runtime),
            self.config.repair_concurrency.max(1) as usize,
            plan.repairs.len().max(1),
            {
                let this = self.clone();
                let permit = permit.clone();
                move |job: (usize, SymbolRepair)| {
                    let this = this.clone();
                    let permit = permit.clone();
                    let caps = caps.clone();
                    async move {
                        scoped(
                            caps,
                            async move { this.repair_with_retry(&permit, job).await },
                        )
                        .await
                    }
                }
            },
        );
        let jobs: Vec<(usize, SymbolRepair)> = plan.repairs.iter().cloned().enumerate().collect();
        let mut reports = Vec::with_capacity(jobs.len());
        for (position, outcome) in pool
            .submit_all(jobs, Some(self.abort.clone()))
            .await
            .into_iter()
            .enumerate()
        {
            match outcome {
                Ok((_, report)) => reports.push(report),
                Err(err) => reports.push(SymbolRepair {
                    status: RepairStatus::Deferred,
                    attempts: 0,
                    inserted: 0,
                    conflicts: 0,
                    detail: format!("repair submit failed: {err}"),
                    ..plan.repairs[position].clone()
                }),
            }
        }
        pool.shutdown().await;
        reports.sort_by_key(|report| report.key.symbol_id.0);
        reports
    }

    /// Repair one symbol, retrying `Deferred` under capped backoff. The
    /// connectivity re-check opens every iteration — mirroring the supervised
    /// reconnect shape — so a long sleep never commits to a doomed fetch,
    /// and cancellation is observed before every attempt as well as at the
    /// pool boundary. Attempts are counted; sleeps use the shared schedule
    /// saturated at the configured ceiling.
    async fn repair_with_retry(
        &self,
        permit: &StoreStorage,
        job: (usize, SymbolRepair),
    ) -> Result<(usize, SymbolRepair), Infallible> {
        let (index, planned) = job;
        if planned.status == RepairStatus::Unavailable {
            return Ok((index, planned));
        }
        let Some(provider) = self.providers.get(&planned.key.symbol_id) else {
            return Ok((
                index,
                SymbolRepair {
                    status: RepairStatus::Unavailable,
                    attempts: 0,
                    inserted: 0,
                    conflicts: 0,
                    detail: format!(
                        "no provider registered for symbol_id {}: refusing",
                        planned.key.symbol_id.0
                    ),
                    ..planned
                },
            ));
        };
        let worker = RepairWorker::new(
            Arc::clone(&self.store),
            Arc::clone(provider),
            permit.clone(),
            planned.key,
            planned.wanted,
            planned.spans.clone(),
            planned.reason,
            self.config.clone(),
        );
        let mut failures = 0u32;
        let mut attempts = 0u32;
        loop {
            if self.abort.is_cancelled() {
                return Ok((
                    index,
                    SymbolRepair {
                        status: RepairStatus::Deferred,
                        attempts,
                        detail: "repair cancelled".to_owned(),
                        ..planned
                    },
                ));
            }
            if !provider.is_reachable() {
                let delay = retry_delay_ms(&self.config, failures);
                failures += 1;
                self.runtime.sleep(Duration::from_millis(delay)).await;
                continue;
            }
            let mut report = worker.repair_attempt().await;
            attempts += 1;
            report.attempts = attempts;
            match report.status {
                RepairStatus::Repaired
                | RepairStatus::AlreadyComplete
                | RepairStatus::Unavailable => return Ok((index, report)),
                RepairStatus::Deferred => {
                    let delay = retry_delay_ms(&self.config, failures);
                    failures += 1;
                    self.runtime.sleep(Duration::from_millis(delay)).await;
                }
            }
        }
    }
}

/// Resets the single-flight flag when the pass scope exits, including on
/// panic: a dead pass must never bar the next one forever.
struct FlightGuard(Arc<AtomicBool>);

impl Drop for FlightGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}