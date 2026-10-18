//! Reconnect backfill: a dropped window splices before signals resume.
//!
//! A stream drop can resume over a gapped series, which would evaluate
//! indicators on missing bars. This module closes that hole: reconnecting
//! bars that open a spacing gap suspend entries, the missing range is
//! fetched through the ordered source ladder
//! ([`first_accept_in_order_sync`](fluent_concurrency::ladder::first_accept_in_order_sync))
//! and spliced, and only a contiguous series resumes. A failed fetch keeps
//! signals suspended and flags health through [`HealthGate::Backfill`](crate::health::HealthGate)
//! (**correctness** — the decision's inputs may be wrong), never through the
//! producer-confidence breaker.
//!
//! Ownership mirrors reconciliation: [`BackfillUnit`] is the supervised
//! sensor the session prepends to every bar once attached, [`BackfillHandle`]
//! is the shared gate the submission path reads, and [`BackfillSource`] reads
//! are synchronous snapshots (in-memory or file rungs), so observing never
//! blocks.

use std::sync::{Arc, Mutex};

use fluent_concurrency::ladder::first_accept_in_order_sync;
use fluent_wvr::prelude::*;
use protopyne_types::{adjust_fetch_bounds, AdjustedBounds, Bar, BarGap, SeriesContext, Timeframe};

use crate::error::VenueError;

/// Asset the backfill sensor provides: units whose validity depends on a
/// contiguous series declare it in `depends`, so a hole cancels them
/// through the batch graph while exits and monitors stay independent.
pub const BACKFILL_ASSET: &str = "bars.backfilled";

/// Outcome of one resync pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackfillReport {
    /// Whether the series is contiguous again.
    pub complete: bool,
    /// Hole bars spliced by this pass (0 when already contiguous).
    pub filled: usize,
    /// Human-readable alert lines; empty when complete.
    pub alerts: Vec<String>,
}

/// One history rung the ladder walks: the missing closed-bar range in,
/// contiguous candidate bars out. Empty means "nothing here" (skipped, not
/// an error); `Err` walks on while retryable and stops the walk when
/// terminal.
pub trait BackfillSource: Send + Sync {
    /// Fetch closed bars covering `[start_ms, end_ms]`.
    fn fetch(&self, start_ms: i64, end_ms: i64) -> Result<Vec<Bar>, VenueError>;
}

/// Entry gate driven by backfill: a hole suspends entries while exits stay
/// permitted (managing existing positions never stops).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackfillGate {
    suspended: bool,
    reason: Option<String>,
    alerts: Vec<String>,
}

impl BackfillGate {
    /// Observe one resync report. A complete report clears the suspension;
    /// any incomplete one sets it and banks the alerts.
    pub fn observe(&mut self, report: &BackfillReport) {
        if report.complete {
            self.suspended = false;
            self.reason = None;
        } else {
            self.suspended = true;
            self.reason = report.alerts.first().cloned();
            self.alerts.extend(report.alerts.iter().cloned());
        }
    }

    /// Suspend entries with a reason (the reconnect path, before any fetch).
    pub fn suspend(&mut self, reason: String) {
        self.suspended = true;
        self.reason = Some(reason.clone());
        self.alerts.push(reason);
    }

    /// Whether new entries must wait for the hole.
    #[must_use]
    pub fn entries_suspended(&self) -> bool {
        self.suspended
    }

    /// Whether the series is contiguous (the health gate's backfill fact).
    #[must_use]
    pub fn backfilled(&self) -> bool {
        !self.suspended
    }

    /// Why entries wait, if they do.
    #[must_use]
    pub fn reason(&self) -> Option<String> {
        self.reason.clone()
    }

    /// Refuse entries while suspended. Call from the submission path.
    pub fn check_entries(&self) -> Result<(), VenueError> {
        if self.suspended {
            Err(VenueError::Health(format!(
                "entries suspended: {}",
                self.reason.as_deref().unwrap_or("backfill incomplete")
            )))
        } else {
            Ok(())
        }
    }

    /// Take the banked alerts for out-of-band delivery, leaving the
    /// suspension untouched. Delivery drains: alerts never accumulate
    /// without bound across reporting cycles.
    pub fn drain_alerts(&mut self) -> Vec<String> {
        std::mem::take(&mut self.alerts)
    }
}

/// Shared handle to the backfill entry gate: the sensor observes into it,
/// the submission path reads it. Locking never crosses an await (every
/// transition is synchronous), so a recovering mutex is the honest
/// primitive, never a wedged gate.
#[derive(Debug, Clone, Default)]
pub struct BackfillHandle {
    inner: Arc<Mutex<BackfillGate>>,
}

impl BackfillHandle {
    /// A handle over an open gate: nothing observed yet, entries allowed.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Observe one resync report.
    pub fn observe(&self, report: &BackfillReport) {
        common_core::sync::lock(&self.inner).observe(report);
    }

    /// Suspend entries with a reason.
    pub fn suspend(&self, reason: String) {
        common_core::sync::lock(&self.inner).suspend(reason);
    }

    /// Whether new entries must wait for the hole.
    #[must_use]
    pub fn entries_suspended(&self) -> bool {
        common_core::sync::lock(&self.inner).entries_suspended()
    }

    /// Whether the series is contiguous.
    #[must_use]
    pub fn backfilled(&self) -> bool {
        common_core::sync::lock(&self.inner).backfilled()
    }

    /// Refuse entries while suspended.
    pub fn check_entries(&self) -> Result<(), VenueError> {
        common_core::sync::lock(&self.inner).check_entries()
    }

    /// Why entries wait, if they do.
    #[must_use]
    pub fn reason(&self) -> Option<String> {
        common_core::sync::lock(&self.inner).reason()
    }

    /// Take the banked alerts for out-of-band delivery.
    pub fn drain_alerts(&self) -> Vec<String> {
        common_core::sync::lock(&self.inner).drain_alerts()
    }
}

/// The missing closed-bar range for one gap, widened by the indicator
/// warmup headroom. Fetching headroom the window already holds is harmless:
/// [`splice`] skips bars at or behind the tip, so the widened fetch stays
/// idempotent while guaranteeing post-resync evaluation keeps its headroom
/// even past a trimmed tail.
#[must_use]
pub fn missing_range(gap: &BarGap, timeframe: Timeframe, warmup: usize) -> AdjustedBounds {
    adjust_fetch_bounds(
        Some(gap.expected_ts.timestamp_millis()),
        usize::try_from(gap.missing_bars).ok(),
        timeframe,
        warmup,
    )
}

/// Splice fetched bars into a series in arrival order. Bars at or behind
/// the tip are already present and skipped, so a headroom-widened fetch is
/// idempotent; any other out-of-order or still-gapped bar refuses instead
/// of reordering or silently jumping the hole.
pub fn splice(ctx: &mut SeriesContext, fetched: Vec<Bar>) -> Result<usize, VenueError> {
    let mut pushed = 0;
    for bar in fetched {
        let tip = ctx.bars().last().map(|tip| tip.ts);
        if tip.is_some_and(|tip| bar.ts <= tip) {
            continue;
        }
        ctx.push(bar)
            .map_err(|e| VenueError::Invalid(format!("backfill splice refused: {e}")))?;
        pushed += 1;
    }
    Ok(pushed)
}

/// Fetch the missing range through the ordered source ladder: the first
/// rung holding bars wins; empty rungs skip; retryable errors walk on while
/// terminal ones stop the walk. Exhaustion with no bars anywhere is a
/// failure (the hole stays open), never a silent success.
pub fn fetch_through(
    sources: &[Arc<dyn BackfillSource>],
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<Bar>, VenueError> {
    let found = first_accept_in_order_sync(
        sources.iter().cloned(),
        |source: Arc<dyn BackfillSource>| {
            source
                .fetch(start_ms, end_ms)
                .map(|bars| if bars.is_empty() { None } else { Some(bars) })
        },
        |error: &VenueError| !error.is_retryable(),
    )?;
    found.ok_or_else(|| {
        VenueError::Health(format!(
            "backfill found no bars for [{start_ms}, {end_ms}]: the hole stays open"
        ))
    })
}

/// The resync sensor's mutable state: the evaluated series plus reconnect
/// bars waiting for their hole to close. One lock covers both, so a note
/// and a resync never interleave halfway through a splice.
#[derive(Debug)]
struct ResyncState {
    ctx: SeriesContext,
    stash: Vec<Bar>,
}

/// One resync pass as a supervised unit, owned by the session once
/// attached. Reconnecting bars arrive through [`note_bar`](Self::note_bar):
/// contiguous bars flow straight into the series while a bar that opens a
/// gap suspends entries and waits; [`resync`](Self::resync) fetches the
/// hole through the ladder, splices it, then drains the waiters in order.
/// Partial progress is kept (fetched bars stay spliced, unpushed waiters
/// stay stashed), so the next pass fetches only what is still missing.
pub struct BackfillUnit {
    state: Mutex<ResyncState>,
    sources: Vec<Arc<dyn BackfillSource>>,
    gate: BackfillHandle,
    warmup: usize,
    provides: Vec<ArcIntern<str>>,
}

impl BackfillUnit {
    /// Build the sensor over `ctx` (the pre-drop series), fetching through
    /// `sources` in order with indicator `warmup` headroom, observing into
    /// `gate`. Starts open: a contiguous series needs no fetch.
    #[must_use]
    pub fn new(
        ctx: SeriesContext,
        sources: Vec<Arc<dyn BackfillSource>>,
        gate: BackfillHandle,
        warmup: usize,
    ) -> Self {
        Self {
            state: Mutex::new(ResyncState {
                ctx,
                stash: Vec::new(),
            }),
            sources,
            gate,
            warmup,
            provides: vec![ArcIntern::from(BACKFILL_ASSET)],
        }
    }

    /// The asset this sensor provides for bar dependents.
    #[must_use]
    pub fn asset(&self) -> &'static str {
        BACKFILL_ASSET
    }

    /// Whether the unit advertises its asset (the sensor contract).
    #[must_use]
    pub fn provides_asset(&self) -> bool {
        self.provides.iter().any(|asset| &**asset == BACKFILL_ASSET)
    }

    /// Borrow the shared gate.
    #[must_use]
    pub fn gate(&self) -> &BackfillHandle {
        &self.gate
    }

    /// Reconnect bars still waiting for their hole, in arrival order.
    #[must_use]
    pub fn pending(&self) -> usize {
        common_core::sync::lock(&self.state).stash.len()
    }

    /// Feed one reconnected closed bar. Contiguous bars flow into the
    /// series; a bar behind the tip is stale and refused without touching
    /// state; a bar ahead opens a gap — entries suspend and the bar waits.
    pub fn note_bar(&self, bar: Bar) -> Result<(), VenueError> {
        let mut state = common_core::sync::lock(&self.state);
        let timeframe = state.ctx.timeframe();
        let step_secs = timeframe.total_seconds() as i64;
        let tip = state
            .stash
            .last()
            .map(|bar| bar.ts)
            .or_else(|| state.ctx.bars().last().map(|bar| bar.ts));
        match tip {
            None => {
                state
                    .ctx
                    .push(bar)
                    .map_err(|e| VenueError::Invalid(format!("backfill refused the bar: {e}")))?;
                Ok(())
            }
            Some(tip) if bar.ts <= tip => Err(VenueError::Invalid(format!(
                "backfill refused a stale bar: {ts} at or behind the tip {tip}",
                ts = bar.ts
            ))),
            Some(tip) if (bar.ts - tip).num_seconds() == step_secs => {
                if state.stash.is_empty() {
                    state.ctx.push(bar).map_err(|e| {
                        VenueError::Invalid(format!("backfill refused the bar: {e}"))
                    })?;
                } else {
                    state.stash.push(bar);
                }
                Ok(())
            }
            Some(tip) => {
                let missing = (bar.ts - tip)
                    .num_seconds()
                    .div_euclid(step_secs)
                    .saturating_sub(1);
                let reason = format!(
                    "reconnect gap: {missing} bars missing between {tip} and {ts}",
                    ts = bar.ts
                );
                state.stash.push(bar);
                drop(state);
                self.gate.suspend(reason.clone());
                Err(VenueError::Health(format!("entries suspended: {reason}")))
            }
        }
    }

    /// Run one resync pass: fetch the hole through the ladder, splice it,
    /// then drain the waiters in order. Observes the report into the gate
    /// either way, so entries resume exactly when the series is contiguous.
    pub fn resync(&self) -> BackfillReport {
        let mut state = common_core::sync::lock(&self.state);
        if state.stash.is_empty() {
            let report = BackfillReport {
                complete: true,
                filled: 0,
                alerts: Vec::new(),
            };
            drop(state);
            self.gate.observe(&report);
            return report;
        }
        if state.ctx.is_empty() {
            return Self::drain_waiters(&mut state, &self.gate, 0);
        }
        let timeframe = state.ctx.timeframe();
        let step_secs = timeframe.total_seconds() as i64;
        let step_ms = timeframe.duration_ms();
        let low = state
            .ctx
            .bars()
            .last()
            .expect("non-empty series has a tip")
            .ts;
        let high = state.stash.first().expect("non-empty stash has a head").ts;
        let missing = (high - low)
            .num_seconds()
            .div_euclid(step_secs)
            .saturating_sub(1)
            .max(0) as u64;
        let gap = BarGap::new()
            .at_index(state.ctx.len())
            .expected_ts(low + chrono::Duration::milliseconds(step_ms))
            .actual_ts(high)
            .missing_bars(missing)
            .build();
        let bounds = missing_range(&gap, timeframe, self.warmup);
        let start_ms = bounds
            .fetch_start_ms
            .unwrap_or_else(|| gap.expected_ts.timestamp_millis());
        let end_ms = gap.actual_ts.timestamp_millis() - step_ms;
        let fetched = match fetch_through(&self.sources, start_ms, end_ms) {
            Ok(fetched) => fetched,
            Err(error) => return Self::suspended(&self.gate, error.to_string()),
        };
        let filled = match splice(&mut state.ctx, fetched) {
            Ok(filled) => filled,
            Err(error) => return Self::suspended(&self.gate, error.to_string()),
        };
        Self::drain_waiters(&mut state, &self.gate, filled)
    }

    /// Push waiters into the series in order. Unpushable waiters (a short
    /// fetch left a second hole) stay stashed with entries suspended;
    /// partial progress is kept either way.
    fn drain_waiters(
        state: &mut ResyncState,
        gate: &BackfillHandle,
        filled: usize,
    ) -> BackfillReport {
        while !state.stash.is_empty() {
            let tip = state.ctx.bars().last().map(|bar| bar.ts);
            if tip.is_some_and(|tip| state.stash[0].ts <= tip) {
                state.stash.remove(0);
                continue;
            }
            let bar = state.stash[0].clone();
            match state.ctx.push(bar) {
                Ok(()) => {
                    state.stash.remove(0);
                }
                Err(error) => {
                    return Self::suspended(gate, format!("backfill short of the hole: {error}"));
                }
            }
        }
        let report = BackfillReport {
            complete: true,
            filled,
            alerts: Vec::new(),
        };
        gate.observe(&report);
        report
    }

    /// Suspend with reason and report the incomplete pass.
    fn suspended(gate: &BackfillHandle, reason: String) -> BackfillReport {
        let report = BackfillReport {
            complete: false,
            filled: 0,
            alerts: vec![reason.clone()],
        };
        gate.suspend(reason);
        report
    }
}

impl WorkUnit for BackfillUnit {
    fn name(&self) -> &str {
        "backfill.sensor"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &self.provides
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        let report = self.resync();
        if report.complete {
            Ok(WorkOutput::ok("series contiguous: backfill complete"))
        } else {
            Err(WorkError::Execution(report.alerts.join("; ")))
        }
    }
}

impl_fieldless!(BackfillUnit);

impl Describable for BackfillUnit {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "description": "Reconnect backfill sensor: fails while a series hole is unspliced",
        })
    }
}

impl_component!(BackfillUnit);

#[cfg(test)]
#[path = "../tests/backfill.rs"]
mod tests;
