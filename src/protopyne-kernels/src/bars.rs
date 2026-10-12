//! Native tick-to-bar ingestion: ring, accumulator, resampler, sessions.
//!
//! The shipped tick library stays byte-identical on disk as the executable
//! contract; this module is its pure-Rust mirror for the closed-bar path.
//! Everything here folds confirmed input only: ticks accumulate into the
//! current bucket, buckets finalize into the ring at grid crossings, and
//! only closed buckets ever emit. Open (still-forming) state is never
//! projected, never confirmed, never mixed into confirmed output.
//!
//! [`BarRing`] mirrors the library's ring buffer and accumulator exactly:
//! bounded capacity with oldest-eviction, chained opens, buy/sell splits,
//! gap accounting, drift-gated reconciliation against injected closed rows,
//! and the single-confirmation fence. Two deliberate divergences from the
//! Pine original, both documented at the site: eviction decrements the
//! running totals (the Pine sizing never overflows within one bar, so its
//! totals never face eviction), and fields the library declares but never
//! reads or writes are not ported.
//!
//! Tick input reuses the shared [`BarTick`](crate::indicator::BarTick) legs
//! (cumulative volume, bar-so-far high/low/close over a bar-open time);
//! raw trade prints fold through [`tick_resample`]. Grid math composes the
//! [`mtf`](crate::mtf) primitives — no second floor lives here. Session
//! breaks stay in the chart-model providers; [`TradingSession`] is the
//! daylight-aware regular-hours clock those providers do not cover.

use std::collections::{BTreeMap, HashMap, VecDeque};

use fluent_wvr::prelude::*;

use crate::error::KernelError;
use crate::indicator::BarTick;
use crate::value::PineValue;
use crate::mtf::{Lookahead, floor_to_grid, fully_formed};
use protopyne_types::{Bar, BarGap, BarWindow, Timeframe, validate_new_bar};

/// Minimum volume that counts as meaningful (reconstructed candles and
/// accumulator finalization below this are skipped, never stored).
/// Task-value axis: a data-validity floor for the job (downstream
/// continuity), not a statement about buffer trust.
pub const VOLUME_EPSILON: f64 = 0.0001;

/// Pine-declared drift tolerance, frozen at the shipped value. Verified
/// unreferenced in the Pine source: no Pine branch reads it, so no Rust
/// decision may read it either — wiring it in would diverge from the
/// oracle. Kept as a named constant so the exact Pine value stays pinned
/// for audit instead of hiding in a comment.
pub const DRIFT_TOLERANCE_PCT: f64 = 0.1;

/// Buy/sell split for directionless (flat) volume. Neither axis: a split
/// convention, not a trigger — it never fires, gates, or pulls.
pub const NEUTRAL_BUY_RATIO: f64 = 0.5;

/// Extra ring slots beyond one chart bar's worth of lower-timeframe candles.
/// Neither axis: a capacity constant, sized once and asserted by overflow
/// tests, never tuned as a heuristic.
pub const BUFFER_PADDING: usize = 5;

/// Percentage drift between measured and reference volume: zero reference
/// (or absent input) guards to zero instead of dividing.
#[must_use]
pub fn drift_pct(actual: f64, expected: f64) -> Option<f64> {
    if !actual.is_finite() || !expected.is_finite() {
        return None;
    }
    if expected <= 0.0 {
        return Some(0.0);
    }
    Some((actual - expected).abs() / expected * 100.0)
}

/// Split a volume delta into buy volume by price direction: up takes all,
/// down takes none, flat (or directionless input) splits neutral.
#[must_use]
pub fn estimate_buy_volume(total_volume: f64, price_delta: f64) -> f64 {
    if price_delta.is_nan() {
        return total_volume * NEUTRAL_BUY_RATIO;
    }
    if price_delta > 0.0 {
        total_volume
    } else if price_delta < 0.0 {
        0.0
    } else {
        total_volume * NEUTRAL_BUY_RATIO
    }
}

/// Whether a volume is meaningful (finite and above [`VOLUME_EPSILON`]).
#[must_use]
pub fn is_valid_volume(vol: f64) -> bool {
    vol.is_finite() && vol > VOLUME_EPSILON
}

/// Structural OHLCV guard behind delta estimation: every leg finite,
/// positive volume, and a non-degenerate range (relative floor keeps dust
/// wicks from inventing direction).
#[must_use]
pub fn validate_ohlcv(open: f64, high: f64, low: f64, close: f64, vol: f64) -> bool {
    open.is_finite()
        && high.is_finite()
        && low.is_finite()
        && close.is_finite()
        && vol.is_finite()
        && vol > 0.0
        && (high - low) > low * 0.000_001
}

/// Buy-volume estimate for one closed candle from its shape: a linear
/// directional component (body ratio, full range ±0.5) plus a close-position
/// bias (±0.15), clamped to [0.02, 0.98]. Degenerate candles fall back to
/// the neutral split. Same op order as the Pine original.
#[must_use]
pub fn estimate_delta_volume(open: f64, high: f64, low: f64, close: f64, vol: f64) -> f64 {
    if !validate_ohlcv(open, high, low, close, vol) {
        return vol * 0.5;
    }
    let range = high - low;
    let body = (close - open).abs();
    let direction = if close > open {
        1.0
    } else if close < open {
        -1.0
    } else {
        0.0
    };
    let body_ratio = body / range;
    let close_pos = (close - low) / range;
    let directional = direction * body_ratio * 0.5;
    let close_adjustment = (close_pos - 0.5) * 0.3;
    let buy_ratio = (0.5 + directional + close_adjustment).clamp(0.02, 0.98);
    vol * buy_ratio
}

/// Fast hyperbolic-tangent approximation (`x / (1 + |x|)`).
#[must_use]
pub fn fast_tanh(x: f64) -> f64 {
    x / (1.0 + x.abs())
}

/// One closed bar in float legs: the confirmation-side shape. Prices convert
/// exactly at market magnitudes; volumes are share counts below 2^53.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClosedBar {
    /// Bucket-open timestamp in milliseconds.
    pub ts_ms: i64,
    /// Open price.
    pub o: f64,
    /// High price.
    pub h: f64,
    /// Low price.
    pub l: f64,
    /// Close price.
    pub c: f64,
    /// Traded volume.
    pub volume: f64,
}

impl ClosedBar {
    /// Convert a confirmed prototype bar: timestamps to milliseconds,
    /// decimals to `f64` (exact at market magnitudes, `NaN` when
    /// unrepresentable rather than failing the window).
    #[must_use]
    pub fn from_proto_bar(bar: &protopyne_types::Bar) -> Self {
        use rust_decimal::prelude::ToPrimitive as _;
        let conv = |d: rust_decimal::Decimal| d.to_f64().unwrap_or(f64::NAN);
        Self {
            ts_ms: bar.ts.timestamp_millis(),
            o: conv(bar.o),
            h: conv(bar.h),
            l: conv(bar.l),
            c: conv(bar.c),
            volume: bar.volume as f64,
        }
    }
}

/// One raw trade print: timestamp, price, own volume. Unlike cumulative
/// engine legs, prints fold whole — every finite print enters its bucket.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawTick {
    /// Print timestamp in milliseconds.
    pub ts_ms: i64,
    /// Print price.
    pub price: f64,
    /// Own volume.
    pub volume: f64,
}

/// One reconstructed lower-timeframe candle: bucket-open stamp, chained
/// open, high/low/close, total and buy volumes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BarParts {
    /// Bucket-open timestamp in milliseconds.
    pub ts_ms: i64,
    /// Open (chained from the prior close at read time in the ring).
    pub o: f64,
    /// High.
    pub h: f64,
    /// Low.
    pub l: f64,
    /// Close.
    pub c: f64,
    /// Total volume.
    pub volume: f64,
    /// Buy volume.
    pub buy_volume: f64,
}

impl BarParts {
    /// Sell volume: total minus buy.
    #[must_use]
    pub fn sell_volume(self) -> f64 {
        self.volume - self.buy_volume
    }
}

/// One closed lower-timeframe row injected for reconciliation (the native
/// stand-in for the fetched security rows): timestamp plus OHLCV.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LtfRow {
    /// Bucket-open timestamp in milliseconds.
    pub ts_ms: i64,
    /// Open price.
    pub o: f64,
    /// High price.
    pub h: f64,
    /// Low price.
    pub l: f64,
    /// Close price.
    pub c: f64,
    /// Total volume.
    pub volume: f64,
}

/// One confirmed chart bar under reconciliation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConfirmedBar {
    /// Chart-bar open timestamp in milliseconds (the confirmation fence).
    pub bar_open_ms: i64,
    /// Open price.
    pub o: f64,
    /// High price.
    pub h: f64,
    /// Low price.
    pub l: f64,
    /// Close price.
    pub c: f64,
    /// Total volume.
    pub volume: f64,
}

/// Fetch-timeframe check at an LTF boundary: the reference volume, plus the
/// closed rows to rebuild from when drift trips the pull.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FetchCheck<'a> {
    /// Reference volume for the drift check.
    pub volume: f64,
    /// Closed rows for the rebuild (`None` records the pull without data).
    pub rows: Option<&'a [LtfRow]>,
}

/// How one tick ingested: seeding absorbs the first updates into tracking
/// (their volume never enters a delta), flat updates only move price
/// tracking, rollbacks reset the accumulator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TickIngest {
    /// First update of the bar: tracking initialized, nothing accumulated.
    Seeded,
    /// No positive volume change: only price tracking moved.
    Unchanged,
    /// Positive volume delta accumulated into the current bucket.
    Accumulated {
        /// Volume delta folded in.
        volume_delta: f64,
        /// Buy share of the delta.
        buy_volume: f64,
    },
    /// Cumulative volume decreased: accumulator reset to the bar legs.
    RolledBack,
}

/// Outcome of one LTF boundary evaluation at a tick stamp.
#[derive(Debug, Clone, PartialEq)]
pub struct LtfOutcome {
    /// Grid boundaries crossed since the accumulator started.
    pub boundaries_crossed: usize,
    /// Empty buckets skipped inside the crossing.
    pub gaps_added: usize,
    /// Candles finalized by the crossing, oldest first.
    pub finalized: Vec<BarParts>,
    /// A boundary pulsed on this tick.
    pub ready_boundary: bool,
    /// Meaningful volume was ready to distribute.
    pub ready_distribution: bool,
    /// Drift tripped with no rebuild rows available.
    pub pull_pending: bool,
}

/// Outcome of one chart-bar confirmation.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfirmOutcome {
    /// Whether this call confirmed (the fence refuses repeats).
    pub confirmed: bool,
    /// Tick-accumulated over reference volume.
    pub tick_accuracy: f64,
    /// Tick drift percentage at confirm time.
    pub volume_drift_pct: f64,
    /// Reconstructed drift under the threshold.
    pub healthy: bool,
    /// Remainder top-up candle, if the reference overshot the ring.
    pub remainder: Option<BarParts>,
    /// Drift tripped: the caller should reconcile against closed rows.
    pub pull_needed: bool,
}

/// Outcome of one rebuild from closed rows.
#[derive(Debug, Clone, PartialEq)]
pub struct ReconcileOutcome {
    /// Candles in the ring after the rebuild.
    pub rebuilt: usize,
    /// Remainder top-up candle, if the reference overshot the rows.
    pub remainder: Option<BarParts>,
}

/// Pull/no-pull verdict from a measured drift: the producer-confidence
/// axis. `Accept` keeps the ring (remainder path); `Pull` tells the caller
/// to reconcile against closed rows. Derived by the frozen strict rule
/// (`measured > threshold` pulls); unmeasurable drift never pulls, the same
/// way Pine `na` comparisons stay quiet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftDecision {
    /// Ring stands; remainder top-up may still apply.
    Accept,
    /// Ring distrusted; caller should reconcile against closed rows.
    Pull,
}

/// Separated drift report: the measured drift (task-value evidence about
/// the numbers) beside the pull/no-pull verdict (producer-confidence about
/// the buffer). Callers read the measurement to judge the answer and the
/// verdict to judge the ring — "I am unsure" never masquerades as "the
/// answer is wrong". Computed fresh on every call: nothing caches or
/// persists a verdict.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DriftReport {
    /// Measured drift percentage (`None` when unmeasurable: non-finite
    /// legs). Task-value evidence.
    pub measured_drift_pct: Option<f64>,
    /// Threshold the verdict applied. Producer-confidence parameter.
    pub threshold_pct: f64,
    /// Verdict derived by the frozen rule. Producer-confidence.
    pub decision: DriftDecision,
}

impl DriftReport {
    /// Assess one measurement under a threshold with the frozen rule.
    #[must_use]
    pub fn assess(measured_drift_pct: Option<f64>, threshold_pct: f64) -> Self {
        let decision = if measured_drift_pct.is_some_and(|drift| drift > threshold_pct) {
            DriftDecision::Pull
        } else {
            DriftDecision::Accept
        };
        Self { measured_drift_pct, threshold_pct, decision }
    }

    /// Whether the caller should reconcile against closed rows.
    #[must_use]
    pub fn pull_needed(self) -> bool {
        self.decision == DriftDecision::Pull
    }

    /// Whether the measurement reads healthy: inside the threshold and
    /// actually measured. Unmeasurable reads unhealthy, matching Pine `na`
    /// drift (too unsure to trust, too quiet to pull).
    #[must_use]
    pub fn healthy(self) -> bool {
        self.measured_drift_pct.is_some_and(|drift| drift < self.threshold_pct)
    }
}

/// Bar-state flags for one tick update in realtime emulation: `isnew` fires
/// on the first tick of a bar-open, `isconfirmed` on the last; tick runs
/// never report history. Four flags because the Pine `barstate` contract has
/// four members — bundled, never constructed piecemeal.
// Note: `struct_excessive_bools` would split the Pine contract across types;
// the four flags travel as one value by design.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarStateFlags {
    /// First tick of the bar.
    pub is_new: bool,
    /// Last tick of the bar.
    pub is_confirmed: bool,
    /// Always false in tick emulation.
    pub is_history: bool,
    /// Always true in tick emulation.
    pub is_realtime: bool,
}

/// Bar-state flags from a tick's position inside its bar-open group.
#[must_use]
pub fn tick_bar_state(first_in_bar: bool, last_in_bar: bool) -> BarStateFlags {
    BarStateFlags {
        is_new: first_in_bar,
        is_confirmed: last_in_bar,
        is_history: false,
        is_realtime: true,
    }
}

/// Which input a resample should fold: cheap tick prints, or full bars when
/// reusing them costs less. Ties reuse bars (no print stream to maintain).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResampleSource {
    /// Fold the tick prints.
    Ticks,
    /// Reuse the full bars.
    Bars,
}

/// Pick the cheaper resample input by count.
#[must_use]
pub fn choose_source(tick_count: usize, bar_count: usize) -> ResampleSource {
    if tick_count < bar_count {
        ResampleSource::Ticks
    } else {
        ResampleSource::Bars
    }
}

/// Cumulative volumes to per-tick deltas: the first value seeds tracking
/// (never a delta), every later difference is one tick's volume.
#[must_use]
pub fn volume_deltas(cumulative: &[f64]) -> Vec<f64> {
    cumulative
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .collect()
}

/// Ring configuration: the four timeframe spans plus the drift/spike
/// thresholds and the gap-fill policy. Plain construction; [`Validate`]
/// checks the spans before a ring builds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BarsConfig {
    /// Reconstructed-candle span in milliseconds.
    pub ltf_ms: i64,
    /// Fetch-validation span in milliseconds.
    pub fetch_ms: i64,
    /// Chart span in milliseconds.
    pub ctf_ms: i64,
    /// Higher-timeframe validation span in milliseconds.
    pub htf_ms: i64,
    /// Spike ratio over the expected slice.
    pub spike_threshold: f64,
    /// Drift percentage that trips a rebuild.
    pub drift_threshold: f64,
    /// Fill empty buckets with zero placeholders.
    pub fill_gaps: bool,
}

impl BarsConfig {
    /// Frozen spike-ratio default from the shipped library (task-value
    /// axis: actionability of the outcome). Pinned here so calibration
    /// tests trip on any widening.
    pub const DEFAULT_SPIKE_THRESHOLD: f64 = 2.0;
    /// Frozen drift-percentage default from the shipped library
    /// (producer-confidence axis: buffer self-doubt). Pinned here so
    /// calibration tests trip on any widening.
    pub const DEFAULT_DRIFT_THRESHOLD: f64 = 4.0;

    /// Record the four spans; thresholds take library defaults (spike 2.0,
    /// drift 4.0, sparse gaps).
    #[must_use]
    pub fn new(ltf_ms: i64, fetch_ms: i64, ctf_ms: i64, htf_ms: i64) -> Self {
        Self {
            ltf_ms,
            fetch_ms,
            ctf_ms,
            htf_ms,
            spike_threshold: Self::DEFAULT_SPIKE_THRESHOLD,
            drift_threshold: Self::DEFAULT_DRIFT_THRESHOLD,
            fill_gaps: false,
        }
    }

    /// Ring capacity: chart-over-LTF ratio plus padding (bounded memory).
    #[must_use]
    pub fn max_candles(self) -> usize {
        let ratio = self.ctf_ms.div_euclid(self.ltf_ms.max(1)).max(1);
        (ratio as usize).saturating_add(BUFFER_PADDING)
    }
}

impl protopyne_types::Validate for BarsConfig {
    type Err = KernelError;

    fn validate(&self) -> Result<(), Self::Err> {
        for (name, ms) in [
            ("ltf_ms", self.ltf_ms),
            ("fetch_ms", self.fetch_ms),
            ("ctf_ms", self.ctf_ms),
            ("htf_ms", self.htf_ms),
        ] {
            if ms <= 0 {
                return Err(KernelError::InvalidParam(format!(
                    "bars config {name} must be a positive span, got {ms}"
                )));
            }
        }
        for (name, value) in [
            ("spike_threshold", self.spike_threshold),
            ("drift_threshold", self.drift_threshold),
        ] {
            if !value.is_finite() {
                return Err(KernelError::InvalidParam(format!(
                    "bars config {name} must be finite, got {value}"
                )));
            }
        }
        Ok(())
    }
}

impl FieldAccess for BarsConfig {
    fn set_field(&mut self, name: &str, _value: &str) -> Result<(), FieldError> {
        Err(FieldError::NotFound(name.into()))
    }

    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        match name {
            "ltf_ms" => Ok(self.ltf_ms.to_string()),
            "fetch_ms" => Ok(self.fetch_ms.to_string()),
            "ctf_ms" => Ok(self.ctf_ms.to_string()),
            "htf_ms" => Ok(self.htf_ms.to_string()),
            "spike_threshold" => Ok(self.spike_threshold.to_string()),
            "drift_threshold" => Ok(self.drift_threshold.to_string()),
            "fill_gaps" => Ok(self.fill_gaps.to_string()),
            _ => Err(FieldError::NotFound(name.into())),
        }
    }

    fn field_names(&self) -> &'static [&'static str] {
        &[
            "ltf_ms",
            "fetch_ms",
            "ctf_ms",
            "htf_ms",
            "spike_threshold",
            "drift_threshold",
            "fill_gaps",
        ]
    }
}

impl Describable for BarsConfig {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "ltf_ms": { "type": "integer", "description": "Reconstructed-candle span in ms (read-only)" },
                "fetch_ms": { "type": "integer", "description": "Fetch-validation span in ms (read-only)" },
                "ctf_ms": { "type": "integer", "description": "Chart span in ms (read-only)" },
                "htf_ms": { "type": "integer", "description": "Higher-timeframe span in ms (read-only)" },
                "spike_threshold": { "type": "number", "description": "Spike ratio over expected (read-only)" },
                "drift_threshold": { "type": "number", "description": "Drift percentage tripping a rebuild (read-only)" },
                "fill_gaps": { "type": "boolean", "description": "Fill empty buckets with zero placeholders (read-only)" }
            },
            "required": ["ltf_ms", "fetch_ms", "ctf_ms", "htf_ms"]
        })
    }
}

impl WorkUnit for BarsConfig {
    fn name(&self) -> &str {
        "bars_config"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        protopyne_types::check_for_execute(self, "bars config invalid")?;
        Ok(WorkOutput::ok(format!(
            "bars config ready (ltf {}ms, ctf {}ms, capacity {})",
            self.ltf_ms,
            self.ctf_ms,
            self.max_candles()
        )))
    }
}

impl_component!(BarsConfig);

/// One retained reconstructed candle (open chains from the bar open or the
/// prior close at read time, like the library's `get`).
#[derive(Debug, Clone, Copy, PartialEq)]
struct StoredCandle {
    ts_ms: i64,
    h: f64,
    l: f64,
    c: f64,
    volume: f64,
    buy_volume: f64,
}

/// Tick-to-bar ring: bounded reconstructed-candle buffer plus the forming
/// accumulator, tick tracker, and reconciliation counters.
#[derive(Debug, Clone)]
pub struct BarRing {
    config: BarsConfig,
    candles: VecDeque<StoredCandle>,
    candle_open: Option<f64>,
    total_vol: f64,
    total_buy: f64,
    sum_h: f64,
    sum_l: f64,
    sum_c: f64,
    gap_count: usize,
    boundary_count: usize,
    pull_count: usize,
    fetch_check_count: usize,
    last_price: Option<f64>,
    last_volume: Option<f64>,
    last_time: Option<i64>,
    last_time_diff: Option<i64>,
    tick_count: usize,
    tick_volume_total: f64,
    tick_buy_volume: f64,
    acc_h: f64,
    acc_l: f64,
    acc_c: f64,
    acc_v: f64,
    acc_b: f64,
    acc_start: Option<i64>,
    last_fetch_time: Option<i64>,
    last_confirmed: Option<i64>,
    tick_accuracy: f64,
    volume_drift_pct: f64,
    last_bar_drift: f64,
    last_bar_healthy: bool,
    rollback_count: usize,
}

impl BarRing {
    /// Build an empty ring over `config` (validated first).
    pub fn new(config: &BarsConfig) -> Result<Self, KernelError> {
        use protopyne_types::Validate as _;
        config.validate()?;
        Ok(Self {
            config: *config,
            candles: VecDeque::with_capacity(config.max_candles()),
            candle_open: None,
            total_vol: 0.0,
            total_buy: 0.0,
            sum_h: 0.0,
            sum_l: 0.0,
            sum_c: 0.0,
            gap_count: 0,
            boundary_count: 0,
            pull_count: 0,
            fetch_check_count: 0,
            last_price: None,
            last_volume: None,
            last_time: None,
            last_time_diff: None,
            tick_count: 0,
            tick_volume_total: 0.0,
            tick_buy_volume: 0.0,
            acc_h: 0.0,
            acc_l: 0.0,
            acc_c: 0.0,
            acc_v: 0.0,
            acc_b: 0.0,
            acc_start: None,
            last_fetch_time: None,
            last_confirmed: None,
            tick_accuracy: 0.0,
            volume_drift_pct: 0.0,
            last_bar_drift: 0.0,
            last_bar_healthy: true,
            rollback_count: 0,
        })
    }

    /// Ring configuration.
    #[must_use]
    pub fn config(&self) -> &BarsConfig {
        &self.config
    }

    /// Number of retained candles.
    #[must_use]
    pub fn len(&self) -> usize {
        self.candles.len()
    }

    /// Whether no candles are retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.candles.is_empty()
    }

    /// Whether any reconstructed candles are available.
    #[must_use]
    pub fn has_volume_waiting(&self) -> bool {
        !self.candles.is_empty()
    }

    /// Total reconstructed volume.
    #[must_use]
    pub fn total_volume(&self) -> f64 {
        self.total_vol
    }

    /// Total reconstructed buy volume.
    #[must_use]
    pub fn total_buy_volume(&self) -> f64 {
        self.total_buy
    }

    /// Skipped empty buckets this bar.
    #[must_use]
    pub fn gap_count(&self) -> usize {
        self.gap_count
    }

    /// LTF boundaries crossed this bar.
    #[must_use]
    pub fn boundary_count(&self) -> usize {
        self.boundary_count
    }

    /// Rebuilds served this run.
    #[must_use]
    pub fn pull_count(&self) -> usize {
        self.pull_count
    }

    /// Fetch-boundary validations run.
    #[must_use]
    pub fn fetch_check_count(&self) -> usize {
        self.fetch_check_count
    }

    /// Ticks accumulated this bar.
    #[must_use]
    pub fn tick_count(&self) -> usize {
        self.tick_count
    }

    /// Tick-accumulated volume this bar.
    #[must_use]
    pub fn tick_volume_total(&self) -> f64 {
        self.tick_volume_total
    }

    /// Tick-accumulated buy volume this bar.
    #[must_use]
    pub fn tick_buy_volume(&self) -> f64 {
        self.tick_buy_volume
    }

    /// Rollbacks detected this run.
    #[must_use]
    pub fn rollback_count(&self) -> usize {
        self.rollback_count
    }

    /// Last bar's reconstructed drift percentage.
    #[must_use]
    pub fn last_bar_drift(&self) -> f64 {
        self.last_bar_drift
    }

    /// Read candle `index` (0 = oldest) with the open chained from the bar
    /// open or the prior close; out-of-range reads absent.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<BarParts> {
        let slot = self.candles.get(index)?;
        let open = if index == 0 {
            self.candle_open?
        } else {
            self.candles.get(index - 1).map(|prev| prev.c)?
        };
        Some(BarParts {
            ts_ms: slot.ts_ms,
            o: open,
            h: slot.h,
            l: slot.l,
            c: slot.c,
            volume: slot.volume,
            buy_volume: slot.buy_volume,
        })
    }

    /// Retained candles, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = BarParts> + '_ {
        (0..self.len()).filter_map(|i| self.get(i))
    }

    /// Retained candles, newest first.
    pub fn iter_rev(&self) -> impl Iterator<Item = BarParts> + '_ {
        (0..self.len()).rev().filter_map(|i| self.get(i))
    }

    /// Newest `n` candles, oldest first (saturates at the ring size).
    #[must_use]
    pub fn window(&self, n: usize) -> Vec<BarParts> {
        let start = self.len().saturating_sub(n);
        (start..self.len()).filter_map(|i| self.get(i)).collect()
    }

    /// Bars since the newest candle satisfying `cond` (0 when the newest
    /// does, absent when none ever did).
    #[must_use]
    pub fn bars_since(&self, cond: impl Fn(&BarParts) -> bool) -> Option<usize> {
        for (back, index) in (0..self.len()).rev().enumerate() {
            if self.get(index).is_some_and(|candle| cond(&candle)) {
                return Some(back);
            }
        }
        None
    }

    /// Buy-minus-neutral volume imbalance over the ring (positive reads buy
    /// pressure); falls back to the forming accumulator when empty.
    #[must_use]
    pub fn volume_imbalance(&self) -> f64 {
        if self.candles.is_empty() {
            self.acc_b - self.acc_v * NEUTRAL_BUY_RATIO
        } else {
            self.total_buy - self.total_vol * NEUTRAL_BUY_RATIO
        }
    }

    /// Whether the newest candle's volume spikes over its slice of the
    /// higher-timeframe reference (absent or non-positive references never
    /// spike).
    #[must_use]
    pub fn is_spike(&self, htf_vol_sma: f64) -> bool {
        if self.candles.is_empty() || !htf_vol_sma.is_finite() || htf_vol_sma <= 0.0 {
            return false;
        }
        let bar_vol = self.candles.back().map_or(0.0, |candle| candle.volume);
        if !bar_vol.is_finite() || bar_vol <= 0.0 {
            return false;
        }
        let expected =
            htf_vol_sma * self.config.ltf_ms as f64 / self.config.htf_ms as f64;
        if !expected.is_finite() || expected <= 0.0 {
            return false;
        }
        bar_vol / expected > 1.0 + self.config.spike_threshold
    }

    /// Adaptive alpha over the ring (or the accumulator when empty), with
    /// absent-input guards reading library defaults. Same op order as the
    /// Pine original; output stays in [0.01, 0.90].
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn alpha(
        &self,
        vol: f64,
        vol_sma: f64,
        atr_distance: f64,
        atr_short: f64,
        atr_long: f64,
        stddev_val: f64,
    ) -> f64 {
        let safe_vol_sma = nonzero_or(vol_sma, 1.0);
        let safe_atr_short = nonzero_or(atr_short, 1.0);
        let safe_atr_long = nonzero_or(atr_long, 1.0);
        let safe_stddev = nonzero_or(stddev_val, 0.1);
        let safe_atr_distance = if atr_distance.is_nan() { 0.0 } else { atr_distance };
        if safe_vol_sma <= 0.0 || safe_atr_long <= 0.0 {
            return 0.08;
        }
        let (total_vol, total_buy) = if self.candles.is_empty() {
            (self.acc_v, self.acc_b)
        } else {
            (self.total_vol, self.total_buy)
        };
        if total_vol <= 0.0 {
            return 0.08;
        }
        let total_sell = total_vol - total_buy;
        let buy_height = total_buy / total_vol;
        let sell_height = total_sell / total_vol;
        let bar_ratio = self.config.ltf_ms as f64 / self.config.ctf_ms as f64;
        let mut delta_alpha = buy_height - sell_height;
        let vol_alpha = (vol / safe_vol_sma)
            + (buy_height + sell_height)
                / (1.0f64.max(nonzero_or(
                    safe_vol_sma / 0.05f64.max(bar_ratio),
                    1.0,
                )));
        let volatility = 0.1f64.max(safe_atr_short / safe_atr_long);
        let squeeze = safe_stddev / safe_atr_long;
        if delta_alpha < 0.0 && safe_atr_distance > 0.0 {
            delta_alpha =
                safe_atr_distance * sell_height / (sell_height / 2.0).max(buy_height);
        } else if delta_alpha > 0.0 && safe_atr_distance < 0.0 {
            delta_alpha =
                (-safe_atr_distance) * buy_height / (buy_height / 2.0).max(sell_height);
        }
        fast_tanh(((6.0 * vol_alpha) + (2.0 * delta_alpha) + volatility + (2.0 * squeeze)) / 50.0)
    }

    /// Open a new chart bar: reset tick tracking, the accumulator, the ring,
    /// and the per-bar counters (the bar-so-far legs seed the state). Call
    /// with the bar's first update (the `isnew` branch): that update seeds
    /// tracking only and never accumulates, so ticks fold from the second
    /// update on — the same seeding the engine performs.
    pub fn on_bar_open(&mut self, open: f64, high: f64, low: f64, close: f64, bar_open_ms: i64) {
        self.last_price = Some(close);
        self.last_volume = None;
        self.last_time = Some(bar_open_ms);
        self.last_fetch_time = Some(bar_open_ms);
        self.tick_count = 0;
        self.last_time_diff = None;
        self.tick_volume_total = 0.0;
        self.tick_buy_volume = 0.0;
        self.acc_h = high;
        self.acc_l = low;
        self.acc_c = close;
        self.acc_v = 0.0;
        self.acc_b = 0.0;
        self.acc_start = Some(bar_open_ms);
        self.reset_ring();
        self.gap_count = 0;
        self.boundary_count = 0;
        self.candle_open = Some(open);
    }

    /// Fold one tick update: cumulative volume and bar-so-far legs arrive on
    /// the shared [`BarTick`] shape (`volume` is cumulative, `close` the
    /// tick price, `high`/`low` the bar-so-far extremes for rollback
    /// recovery); the tick stamp rides alongside.
    pub fn ingest_tick(&mut self, bar: BarTick, tick_ms: i64) -> TickIngest {
        let price = bar.close;
        let cum_volume = bar.volume;
        match self.last_volume {
            None => {
                self.last_volume = Some(cum_volume);
                self.last_price = Some(price);
                self.last_time = Some(tick_ms);
                TickIngest::Seeded
            }
            Some(last) if cum_volume < last => {
                self.rollback_count += 1;
                self.acc_v = 0.0;
                self.acc_b = 0.0;
                self.acc_h = bar.high;
                self.acc_l = bar.low;
                self.acc_c = price;
                self.last_price = Some(price);
                self.last_volume = Some(cum_volume);
                self.last_time = Some(tick_ms);
                TickIngest::RolledBack
            }
            Some(last) => {
                let price_delta = price - self.last_price.unwrap_or(price);
                let volume_delta = cum_volume - last;
                self.last_price = Some(price);
                if volume_delta > 0.0 {
                    self.tick_count += 1;
                    self.tick_volume_total += volume_delta;
                    let buy = estimate_buy_volume(volume_delta, price_delta);
                    self.tick_buy_volume += buy;
                    self.acc_v += volume_delta;
                    self.acc_b += buy;
                    self.acc_h = self.acc_h.max(price);
                    self.acc_l = if self.acc_l == 0.0 { price } else { self.acc_l.min(price) };
                    self.acc_c = price;
                    self.last_time_diff = Some(tick_ms - self.last_time.unwrap_or(tick_ms));
                    self.last_time = Some(tick_ms);
                    self.last_volume = Some(cum_volume);
                    TickIngest::Accumulated { volume_delta, buy_volume: buy }
                } else {
                    TickIngest::Unchanged
                }
            }
        }
    }

    /// Assess the ring against a reference volume: measured drift beside
    /// the pull/no-pull verdict, under the configured threshold. Pure
    /// read — no counters move, nothing caches, the caller decides what
    /// the verdict means. Both confirm paths consult this one rule, so
    /// the decision cannot drift apart between call sites.
    #[must_use]
    pub fn drift_report(&self, reference_vol: f64) -> DriftReport {
        DriftReport::assess(
            drift_pct(self.total_vol, reference_vol),
            self.config.drift_threshold,
        )
    }

    /// Evaluate LTF boundaries at `now_ms` with the current price: finalize
    /// crossed buckets, fill gaps when configured, and run the fetch-drift
    /// check. Accumulation always precedes this call per tick (a crossing
    /// tick's delta belongs to the bucket it leaves).
    pub fn confirm_ltf(
        &mut self,
        now_ms: i64,
        price: f64,
        fetch: Option<FetchCheck<'_>>,
    ) -> LtfOutcome {
        let mut outcome = LtfOutcome {
            boundaries_crossed: 0,
            gaps_added: 0,
            finalized: Vec::new(),
            ready_boundary: false,
            ready_distribution: false,
            pull_pending: false,
        };
        if let Some(start) = self.acc_start {
            let grid = self.config.ltf_ms;
            let crossed = now_ms.div_euclid(grid) - start.div_euclid(grid);
            if crossed > 0 {
                let crossed_usize = crossed as usize;
                self.boundary_count += crossed_usize;
                outcome.boundaries_crossed = crossed_usize;
                if crossed > 1 {
                    let gaps = (crossed - 1) as usize;
                    self.gap_count += gaps;
                    outcome.gaps_added = gaps;
                }
                outcome.ready_boundary = true;
                outcome.ready_distribution = is_valid_volume(self.acc_v);
                if let Some(candle) = self.finalize_accumulator() {
                    outcome.finalized.push(candle);
                }
                if self.config.fill_gaps {
                    for candle in self.fill_gaps(start, outcome.gaps_added) {
                        outcome.finalized.push(candle);
                    }
                }
                self.reset_accumulator(now_ms, price);
            }
            if self.config.fetch_ms > 0 {
                if let Some(last_fetch) = self.last_fetch_time {
                    let fetch_grid = self.config.fetch_ms;
                    if now_ms.div_euclid(fetch_grid) > last_fetch.div_euclid(fetch_grid) {
                        if let Some(check) = fetch {
                            self.fetch_check_count += 1;
                            let report = DriftReport::assess(
                                drift_pct(self.total_vol + self.acc_v, check.volume),
                                self.config.drift_threshold,
                            );
                            if report.pull_needed() {
                                match check.rows {
                                    Some(rows) => {
                                        self.reconcile(rows, check.volume, price);
                                    }
                                    None => outcome.pull_pending = true,
                                }
                            }
                            self.last_fetch_time = Some(now_ms);
                        }
                    }
                }
            }
        }
        outcome
    }

    /// Confirm the chart bar: finalize the remainder accumulator, score
    /// tick accuracy and drifts, and either top up the remainder or flag a
    /// rebuild. The fence refuses repeats on one bar-open time.
    pub fn confirm(&mut self, ctf: &ConfirmedBar) -> ConfirmOutcome {
        if self.last_confirmed.is_some_and(|t| ctf.bar_open_ms <= t) {
            return ConfirmOutcome {
                confirmed: false,
                tick_accuracy: self.tick_accuracy,
                volume_drift_pct: self.volume_drift_pct,
                healthy: self.last_bar_healthy,
                remainder: None,
                pull_needed: false,
            };
        }
        self.last_confirmed = Some(ctf.bar_open_ms);
        if is_valid_volume(self.acc_v) {
            self.finalize_accumulator();
        }
        if self.tick_volume_total > 0.0 {
            self.tick_accuracy = self.tick_volume_total / ctf.volume;
            self.volume_drift_pct = drift_pct(self.tick_volume_total, ctf.volume).unwrap_or(0.0);
        } else {
            self.tick_accuracy = 0.0;
            self.volume_drift_pct = 0.0;
        }
        let report = self.drift_report(ctf.volume);
        self.last_bar_drift = report.measured_drift_pct.unwrap_or(f64::NAN);
        self.last_bar_healthy = report.healthy();
        let mut outcome = ConfirmOutcome {
            confirmed: true,
            tick_accuracy: self.tick_accuracy,
            volume_drift_pct: self.volume_drift_pct,
            healthy: self.last_bar_healthy,
            remainder: None,
            pull_needed: false,
        };
        if report.pull_needed() {
            self.fetch_check_count += 1;
            outcome.pull_needed = true;
        } else {
            let remainder_vol = ctf.volume - self.total_vol;
            outcome.remainder =
                self.add_remainder(remainder_vol, ctf.o, ctf.h, ctf.l, ctf.c, ctf.bar_open_ms);
        }
        self.reset_accumulator(ctf.bar_open_ms, ctf.c);
        outcome
    }

    /// Rebuild the ring from closed lower-timeframe rows against the
    /// reference volume: reset, load every meaningful row (buy splits via
    /// shape estimation), chain the open from the first row, then top up any
    /// remainder. Counts one pull. Rows past capacity evict the oldest
    /// (bounded memory keeps the newest; the Pine loop instead stops at
    /// capacity, but its sizing never overflows, so fitting input agrees).
    pub fn reconcile(
        &mut self,
        rows: &[LtfRow],
        reference_vol: f64,
        fallback_open: f64,
    ) -> ReconcileOutcome {
        if !rows.is_empty() {
            self.reset_ring();
            for row in rows {
                if is_valid_volume(row.volume) {
                    let buy = estimate_delta_volume(row.o, row.h, row.l, row.c, row.volume);
                    self.write_candle(row.h, row.l, row.c, row.volume, buy, row.ts_ms);
                }
            }
            if !self.candles.is_empty() {
                self.candle_open = Some(rows[0].o);
            }
        }
        if self.candles.is_empty() {
            self.candle_open = Some(fallback_open);
        }
        let mut outcome = ReconcileOutcome { rebuilt: self.candles.len(), remainder: None };
        if !rows.is_empty() {
            let remainder_vol = reference_vol - self.total_vol;
            if let Some(last) = rows.last() {
                outcome.remainder =
                    self.add_remainder(remainder_vol, last.o, last.h, last.l, last.c, last.ts_ms);
            }
        }
        self.pull_count += 1;
        outcome
    }

    fn reset_ring(&mut self) {
        self.candles.clear();
        self.total_vol = 0.0;
        self.total_buy = 0.0;
        self.sum_h = 0.0;
        self.sum_l = 0.0;
        self.sum_c = 0.0;
    }

    fn write_candle(&mut self, high: f64, low: f64, close: f64, vol: f64, buy: f64, ts_ms: i64) {
        if self.candles.len() >= self.config.max_candles() {
            if let Some(old) = self.candles.pop_front() {
                self.total_vol -= old.volume;
                self.total_buy -= old.buy_volume;
                self.sum_h -= old.h;
                self.sum_l -= old.l;
                self.sum_c -= old.c;
            }
        }
        self.candles.push_back(StoredCandle {
            ts_ms,
            h: high,
            l: low,
            c: close,
            volume: vol,
            buy_volume: buy,
        });
        self.total_vol += vol;
        self.total_buy += buy;
        self.sum_h += high;
        self.sum_l += low;
        self.sum_c += close;
    }

    fn finalize_accumulator(&mut self) -> Option<BarParts> {
        if !is_valid_volume(self.acc_v) {
            return None;
        }
        let start = self.acc_start.unwrap_or(0);
        let candle = BarParts {
            ts_ms: start,
            o: self.acc_c,
            h: self.acc_h,
            l: self.acc_l,
            c: self.acc_c,
            volume: self.acc_v,
            buy_volume: self.acc_b,
        };
        self.write_candle(self.acc_h, self.acc_l, self.acc_c, self.acc_v, self.acc_b, start);
        self.acc_v = 0.0;
        self.acc_b = 0.0;
        self.acc_h = 0.0;
        self.acc_l = 0.0;
        self.acc_c = 0.0;
        Some(candle)
    }

    fn fill_gaps(&mut self, start: i64, num_gaps: usize) -> Vec<BarParts> {
        let mut out = Vec::new();
        let last_close = self
            .candles
            .back()
            .map(|candle| candle.c)
            .or(self.candle_open)
            .unwrap_or(self.acc_c);
        for i in 0..num_gaps {
            if self.candles.len() >= self.config.max_candles() {
                break;
            }
            let gap_time = start + self.config.ltf_ms + i as i64 * self.config.ltf_ms;
            self.write_candle(last_close, last_close, last_close, 0.0, 0.0, gap_time);
            out.push(BarParts {
                ts_ms: gap_time,
                o: last_close,
                h: last_close,
                l: last_close,
                c: last_close,
                volume: 0.0,
                buy_volume: 0.0,
            });
        }
        out
    }

    fn reset_accumulator(&mut self, now_ms: i64, price: f64) {
        self.acc_h = price;
        self.acc_l = price;
        self.acc_c = price;
        self.acc_v = 0.0;
        self.acc_b = 0.0;
        self.acc_start = Some(now_ms);
    }

    fn add_remainder(
        &mut self,
        remainder_vol: f64,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
        ts_ms: i64,
    ) -> Option<BarParts> {
        if !remainder_vol.abs().is_finite()
            || remainder_vol.abs() <= VOLUME_EPSILON
            || self.candles.len() >= self.config.max_candles()
        {
            return None;
        }
        let vol = remainder_vol.abs();
        let buy = estimate_delta_volume(open, high, low, close, vol);
        self.write_candle(high, low, close, vol, buy, ts_ms);
        Some(BarParts { ts_ms, o: open, h: high, l: low, c: close, volume: vol, buy_volume: buy })
    }
}

impl protopyne_types::Validate for BarRing {
    type Err = KernelError;

    fn validate(&self) -> Result<(), Self::Err> {
        protopyne_types::Validate::validate(&self.config)?;
        if self.candles.len() > self.config.max_candles() {
            return Err(KernelError::InvalidInput(format!(
                "ring holds {} candles over capacity {}",
                self.candles.len(),
                self.config.max_candles()
            )));
        }
        if !(self.total_vol.is_finite() && self.total_vol >= 0.0) {
            return Err(KernelError::InvalidInput(format!(
                "ring total volume out of range: {}",
                self.total_vol
            )));
        }
        if !(self.total_buy.is_finite() && self.total_buy >= 0.0) {
            return Err(KernelError::InvalidInput(format!(
                "ring total buy volume out of range: {}",
                self.total_buy
            )));
        }
        let mut prev: Option<i64> = None;
        for candle in &self.candles {
            if prev.is_some_and(|t| candle.ts_ms <= t) {
                return Err(KernelError::InvalidInput(format!(
                    "ring timestamps not strictly increasing at {}",
                    candle.ts_ms
                )));
            }
            prev = Some(candle.ts_ms);
        }
        Ok(())
    }
}

fn nonzero_or(value: f64, default: f64) -> f64 {
    if value.is_nan() { default } else { value }
}

// ---------------------------------------------------------------------------
// Native engine bridge: Pine `Bars` object field tables in and out.
//
// The engine keeps the Pine object as the single source of truth; native
// execution is a pure transition over its fields (hydrate, run, dehydrate),
// so snapshots, dumps, and later interpreted reads observe identical state
// under both positions. Unmapped fields (timeframe enums, ratios, pulse
// flags owned by the driver, diagnostic placeholders) pass through
// untouched: the bridge only ever writes the keys it maps, and the caller
// owns the table. Anything untranslatable refuses with a named reason —
// never a silent approximation.
// ---------------------------------------------------------------------------

/// Pine object field table shared with the engine bridge: field name to
/// Pine value (`None` is Pine `na`).
pub type FieldTable = HashMap<String, Option<PineValue>>;

/// Physical cursor of a hydrated ring: the caller advances it with Pine's
/// exact rules per appended candle (see [`BarRing::write_pine_fields`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingCursor {
    /// Ring-buffer write position.
    pub head: usize,
    /// Number of valid candles.
    pub count: usize,
}

/// One native `fetch` tick: the legs and flags the Pine body would observe
/// through its builtins. `bar_open_ms` is the `time`-leg value Pine reads
/// for tracking (`last_time`, `last_time_diff`, accumulator stamps are all
/// bar-open-sourced in the oracle); `tick_ms` is the side-channel stamp
/// Pine passes to `onConfirmLTF` as `timenow`.
#[derive(Debug, Clone)]
pub struct NativeFetchTick {
    /// Bar-so-far OHLC plus cumulative volume legs.
    pub legs: BarTick,
    /// Side-channel tick stamp.
    pub tick_ms: i64,
    /// Bar-open milliseconds.
    pub bar_open_ms: i64,
    /// `barstate.isnew` at this tick.
    pub is_new: bool,
    /// `barstate.isconfirmed` at this tick.
    pub is_confirmed: bool,
    /// Confirm legs, required when `is_confirmed`.
    pub confirm: Option<ConfirmedBar>,
}

/// Outcome of one native `fetch` tick: the exact `_writeCandle`-equivalent
/// count for cursor accounting, the end-of-tick pulse flags, and the
/// outcome signal (the pull and capacity paths both fall back to Pine —
/// the pull raises the identical lower-tf refusal there, capacity
/// exhaustion reruns the proven body — so the bridge never invents rows
/// or miscounts a cursor).
#[derive(Debug, Clone, PartialEq)]
pub struct FetchNativeOut {
    /// Candles appended this tick.
    pub appended: usize,
    /// LTF boundary crossed this tick.
    pub ready_boundary: bool,
    /// Distributable volume this tick.
    pub ready_distribution: bool,
    /// Commit or fall back to Pine.
    pub signal: FetchSignal,
}

/// Native-fetch outcome signal: commit the transition, or let the engine
/// rerun the Pine body (identical refusal on pulls, proven behavior on
/// capacity exhaustion).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchSignal {
    /// Dehydrate and serve natively.
    Commit,
    /// Drift tripped a rebuild.
    PullNeeded,
    /// Capacity accounting turned ambiguous.
    FallbackCapacity,
}

fn bridge_invalid(field: &str, detail: &str) -> KernelError {
    KernelError::InvalidInput(format!("native Bars bridge: field '{field}' {detail}"))
}

fn field_opt_f64(fields: &FieldTable, name: &str) -> Result<Option<f64>, KernelError> {
    match fields.get(name) {
        None => Err(bridge_invalid(name, "is missing")),
        Some(None) => Ok(None),
        Some(Some(PineValue::Float(value))) => Ok(Some(*value)),
        Some(Some(PineValue::Int(value))) => Ok(Some(*value as f64)),
        Some(Some(other)) => Err(bridge_invalid(name, &format!("is not a float ({other:?})"))),
    }
}

fn field_req_f64(fields: &FieldTable, name: &str) -> Result<f64, KernelError> {
    match field_opt_f64(fields, name)? {
        Some(value) => Ok(value),
        None => Err(bridge_invalid(name, "is na")),
    }
}

fn pine_int(value: &PineValue, field: &str) -> Result<i64, KernelError> {
    match value {
        PineValue::Int(raw) => Ok(*raw),
        // Engine int fields routinely hold integral floats (function
        // returns unify to float): exact integral values convert, anything
        // fractional refuses — never a silent truncation.
        PineValue::Float(raw) => {
            if raw.is_finite() && raw.fract() == 0.0 && *raw >= i64::MIN as f64 && *raw <= i64::MAX as f64 {
                Ok(*raw as i64)
            } else {
                Err(bridge_invalid(field, &format!("float {raw} is not an integral int")))
            }
        }
        other => Err(bridge_invalid(field, &format!("is not an int ({other:?})"))),
    }
}

fn field_opt_int(fields: &FieldTable, name: &str) -> Result<Option<i64>, KernelError> {
    match fields.get(name) {
        None => Err(bridge_invalid(name, "is missing")),
        Some(None) => Ok(None),
        Some(Some(value)) => pine_int(value, name).map(Some),
    }
}

fn field_req_int(fields: &FieldTable, name: &str) -> Result<i64, KernelError> {
    match field_opt_int(fields, name)? {
        Some(value) => Ok(value),
        None => Err(bridge_invalid(name, "is na")),
    }
}

fn field_req_bool(fields: &FieldTable, name: &str) -> Result<bool, KernelError> {
    match fields.get(name) {
        None => Err(bridge_invalid(name, "is missing")),
        Some(Some(PineValue::Bool(value))) => Ok(*value),
        Some(_) => Err(bridge_invalid(name, "is not a bool")),
    }
}

fn field_req_usize(fields: &FieldTable, name: &str) -> Result<usize, KernelError> {
    let raw = field_req_int(fields, name)?;
    usize::try_from(raw)
        .map_err(|_| bridge_invalid(name, &format!("is negative ({raw})")))
}

fn field_float_array(
    fields: &FieldTable,
    name: &str,
    expect_len: usize,
) -> Result<Vec<Option<f64>>, KernelError> {
    let items = match fields.get(name) {
        None => return Err(bridge_invalid(name, "is missing")),
        Some(None) => return Err(bridge_invalid(name, "is na (run init first)")),
        Some(Some(PineValue::Array(items))) => items,
        Some(Some(other)) => {
            return Err(bridge_invalid(name, &format!("is not an array ({other:?})")));
        }
    };
    if items.len() != expect_len {
        return Err(bridge_invalid(
            name,
            &format!("holds {} slots, expected {expect_len}", items.len()),
        ));
    }
    items
        .iter()
        .map(|slot| match slot {
            None => Ok(None),
            Some(PineValue::Float(value)) => Ok(Some(*value)),
            Some(PineValue::Int(value)) => Ok(Some(*value as f64)),
            Some(other) => Err(bridge_invalid(name, &format!("holds {other:?}"))),
        })
        .collect()
}

fn field_int_array(
    fields: &FieldTable,
    name: &str,
    expect_len: usize,
) -> Result<Vec<Option<i64>>, KernelError> {
    let items = match fields.get(name) {
        None => return Err(bridge_invalid(name, "is missing")),
        Some(None) => return Err(bridge_invalid(name, "is na (run init first)")),
        Some(Some(PineValue::Array(items))) => items,
        Some(Some(other)) => {
            return Err(bridge_invalid(name, &format!("is not an array ({other:?})")));
        }
    };
    if items.len() != expect_len {
        return Err(bridge_invalid(
            name,
            &format!("holds {} slots, expected {expect_len}", items.len()),
        ));
    }
    items
        .iter()
        .map(|slot| match slot {
            None => Ok(None),
            Some(value) => pine_int(value, name).map(Some),
        })
        .collect()
}

fn slot_f64(slot: Option<f64>, array: &str, index: usize) -> Result<f64, KernelError> {
    slot.ok_or_else(|| bridge_invalid(array, &format!("slot {index} is na")))
}

fn slot_i64(slot: Option<i64>, array: &str, index: usize) -> Result<i64, KernelError> {
    slot.ok_or_else(|| bridge_invalid(array, &format!("slot {index} is na")))
}

impl BarRing {
    /// Hydrate a ring from Pine `Bars` object fields plus its physical
    /// cursor. Logical candles follow the oracle's physical rule (direct
    /// order while filling, `(head + i) % max` once full); every live slot
    /// must hold numbers. Capacity mismatch between the Pine `max_candles`
    /// and the configured spans refuses: exotic initializations stay
    /// Pine-oracle runs, never silent approximations.
    pub fn from_pine_fields(fields: &FieldTable) -> Result<(Self, RingCursor), KernelError> {
        let config = BarsConfig {
            ltf_ms: field_req_int(fields, "ltf_ms")?,
            fetch_ms: field_req_int(fields, "fetch_ms")?,
            ctf_ms: field_req_int(fields, "ctf_ms")?,
            htf_ms: field_req_int(fields, "htf_ms")?,
            spike_threshold: field_req_f64(fields, "spike_threshold")?,
            drift_threshold: field_req_f64(fields, "drift_threshold")?,
            fill_gaps: field_req_bool(fields, "fill_gaps")?,
        };
        use protopyne_types::Validate as _;
        config.validate().map_err(|err| {
            bridge_invalid("config", &format!("spans invalid ({err})"))
        })?;
        let max = field_req_usize(fields, "max_candles")?;
        if max == 0 || config.max_candles() != max {
            return Err(bridge_invalid(
                "max_candles",
                &format!("Pine holds {max}, spans derive {}", config.max_candles()),
            ));
        }
        let head = field_req_usize(fields, "head")?;
        let count = field_req_usize(fields, "count")?;
        if head >= max {
            return Err(bridge_invalid("head", &format!("{head} over capacity {max}")));
        }
        if count > max {
            return Err(bridge_invalid("count", &format!("{count} over capacity {max}")));
        }
        let ch = field_float_array(fields, "ch", max)?;
        let cl = field_float_array(fields, "cl", max)?;
        let cc = field_float_array(fields, "cc", max)?;
        let cv = field_float_array(fields, "cv", max)?;
        let cb = field_float_array(fields, "cb", max)?;
        let ct = field_int_array(fields, "ct", max)?;
        let mut ring = Self::new(&config).map_err(|err| {
            bridge_invalid("config", &format!("ring refuses spans ({err})"))
        })?;
        for logical in 0..count {
            let physical = if count < max {
                logical
            } else {
                (head + logical) % max
            };
            ring.candles.push_back(StoredCandle {
                ts_ms: slot_i64(ct[physical], "ct", physical)?,
                h: slot_f64(ch[physical], "ch", physical)?,
                l: slot_f64(cl[physical], "cl", physical)?,
                c: slot_f64(cc[physical], "cc", physical)?,
                volume: slot_f64(cv[physical], "cv", physical)?,
                buy_volume: slot_f64(cb[physical], "cb", physical)?,
            });
        }
        ring.candle_open = field_opt_f64(fields, "candle_open")?;
        ring.total_vol = field_req_f64(fields, "total_vol")?;
        ring.total_buy = field_req_f64(fields, "total_buy")?;
        ring.sum_h = field_req_f64(fields, "sum_h_running")?;
        ring.sum_l = field_req_f64(fields, "sum_l_running")?;
        ring.sum_c = field_req_f64(fields, "sum_c_running")?;
        ring.gap_count = field_req_usize(fields, "gap_count")?;
        ring.boundary_count = field_req_usize(fields, "boundary_count")?;
        ring.pull_count = field_req_usize(fields, "pull_count")?;
        ring.fetch_check_count = field_req_usize(fields, "fetch_check_count")?;
        ring.last_price = field_opt_f64(fields, "last_price")?;
        ring.last_volume = field_opt_f64(fields, "last_volume")?;
        ring.last_time = field_opt_int(fields, "last_time")?;
        ring.last_time_diff = field_opt_int(fields, "last_time_diff")?;
        ring.tick_count = field_req_usize(fields, "tick_count")?;
        ring.tick_volume_total = field_req_f64(fields, "tick_volume_total")?;
        ring.tick_buy_volume = field_req_f64(fields, "tick_buy_volume")?;
        ring.acc_h = field_req_f64(fields, "h")?;
        ring.acc_l = field_req_f64(fields, "l")?;
        ring.acc_c = field_req_f64(fields, "c")?;
        ring.acc_v = field_req_f64(fields, "v")?;
        ring.acc_b = field_req_f64(fields, "b")?;
        ring.acc_start = field_opt_int(fields, "accum_start_time")?;
        ring.last_fetch_time = field_opt_int(fields, "last_fetch_time")?;
        ring.last_confirmed = field_opt_int(fields, "last_confirmed_ctf_time")?;
        ring.tick_accuracy = field_req_f64(fields, "tick_accuracy")?;
        ring.volume_drift_pct = field_req_f64(fields, "volume_drift_pct")?;
        ring.last_bar_drift = field_req_f64(fields, "last_bar_drift")?;
        ring.last_bar_healthy = field_req_bool(fields, "last_bar_healthy")?;
        ring.rollback_count = field_req_usize(fields, "rollback_count")?;
        Ok((ring, RingCursor { head, count }))
    }

    /// Write mapped state back into the caller's table: `appended` new
    /// candles land at `(head + j) % max` exactly like the oracle's ring
    /// writes, the cursor advances, and every mapped scalar follows. Array
    /// lengths, dead slots, and unmapped keys are never touched. The
    /// `len == count + appended` check fails the write rather than a
    /// miscounted cursor ever reaching a snapshot.
    pub fn write_pine_fields(
        &self,
        fields: &mut FieldTable,
        head: usize,
        count: usize,
        appended: usize,
    ) -> Result<(), KernelError> {
        let max = field_req_usize(fields, "max_candles")?;
        if max == 0 {
            return Err(bridge_invalid("max_candles", "is zero"));
        }
        if head >= max || count > max {
            return Err(bridge_invalid("cursor", "head/count over capacity"));
        }
        if self.candles.len() != count + appended {
            return Err(bridge_invalid(
                "cursor",
                &format!(
                    "ring holds {}, cursor counts {} + {appended}",
                    self.candles.len(),
                    count
                ),
            ));
        }
        let logical: Vec<&StoredCandle> = self.candles.iter().collect();
        for (index, slot) in logical.iter().enumerate().skip(count).take(appended) {
            let physical = (head + index) % max;
            let set = |fields: &mut FieldTable, key: &str, value: PineValue| {
                if let Some(Some(PineValue::Array(items))) = fields.get_mut(key) {
                    if let Some(cell) = items.get_mut(physical) {
                        *cell = Some(value);
                    }
                }
            };
            set(fields, "ch", PineValue::Float(slot.h));
            set(fields, "cl", PineValue::Float(slot.l));
            set(fields, "cc", PineValue::Float(slot.c));
            set(fields, "cv", PineValue::Float(slot.volume));
            set(fields, "cb", PineValue::Float(slot.buy_volume));
            set(fields, "ct", PineValue::Int(slot.ts_ms));
        }
        let put = |fields: &mut FieldTable, key: &str, value: Option<PineValue>| {
            fields.insert(key.to_owned(), value);
        };
        put(fields, "head", Some(PineValue::Int(((head + appended) % max) as i64)));
        put(
            fields,
            "count",
            Some(PineValue::Int((count + appended) as i64)),
        );
        let num = |value: f64| Some(PineValue::Float(value));
        let opt_num = |value: Option<f64>| value.map(PineValue::Float);
        let opt_int = |value: Option<i64>| value.map(PineValue::Int);
        let counter = |value: usize| Some(PineValue::Int(value as i64));
        put(fields, "candle_open", opt_num(self.candle_open));
        put(fields, "total_vol", num(self.total_vol));
        put(fields, "total_buy", num(self.total_buy));
        put(fields, "sum_h_running", num(self.sum_h));
        put(fields, "sum_l_running", num(self.sum_l));
        put(fields, "sum_c_running", num(self.sum_c));
        put(fields, "gap_count", counter(self.gap_count));
        put(fields, "boundary_count", counter(self.boundary_count));
        put(fields, "pull_count", counter(self.pull_count));
        put(fields, "fetch_check_count", counter(self.fetch_check_count));
        put(fields, "last_price", opt_num(self.last_price));
        put(fields, "last_volume", opt_num(self.last_volume));
        put(fields, "last_time", opt_int(self.last_time));
        put(fields, "last_time_diff", opt_int(self.last_time_diff));
        put(fields, "tick_count", counter(self.tick_count));
        put(fields, "tick_volume_total", num(self.tick_volume_total));
        put(fields, "tick_buy_volume", num(self.tick_buy_volume));
        put(fields, "h", num(self.acc_h));
        put(fields, "l", num(self.acc_l));
        put(fields, "c", num(self.acc_c));
        put(fields, "v", num(self.acc_v));
        put(fields, "b", num(self.acc_b));
        put(fields, "accum_start_time", opt_int(self.acc_start));
        put(fields, "last_fetch_time", opt_int(self.last_fetch_time));
        put(fields, "last_confirmed_ctf_time", opt_int(self.last_confirmed));
        put(fields, "tick_accuracy", num(self.tick_accuracy));
        put(fields, "volume_drift_pct", num(self.volume_drift_pct));
        put(fields, "last_bar_drift", num(self.last_bar_drift));
        put(fields, "last_bar_healthy", Some(PineValue::Bool(self.last_bar_healthy)));
        put(fields, "rollback_count", counter(self.rollback_count));
        Ok(())
    }

    /// The oracle `fetch` body as native transitions: per-bar clear, then
    /// tick accumulation with the oracle's bar-open time source, the LTF
    /// boundary pass, and chart-bar confirmation. Rollback, seeding, and
    /// the volume gate mirror the Pine branches arm for arm (including the
    /// always-zero `last_time_diff` the bar-open clock implies); only the
    /// pull rebuild stays out — it reports through [`FetchNativeOut`] so
    /// the caller re-runs Pine, which raises the identical lower-tf
    /// refusal. Appends are counted exactly; any capacity ambiguity sets
    /// `fallback_capacity` instead of a miscounted cursor.
    pub fn fetch_tick_native(&mut self, tick: &NativeFetchTick) -> Result<FetchNativeOut, KernelError> {
        let bar = &tick.legs;
        let mut appended = 0usize;
        if tick.is_new {
            let saved = (
                self.pull_count,
                self.fetch_check_count,
                self.rollback_count,
                self.tick_accuracy,
                self.volume_drift_pct,
                self.last_bar_drift,
                self.last_bar_healthy,
            );
            self.on_bar_open(bar.open, bar.high, bar.low, bar.close, tick.bar_open_ms);
            self.pull_count = saved.0;
            self.fetch_check_count = saved.1;
            self.rollback_count = saved.2;
            self.tick_accuracy = saved.3;
            self.volume_drift_pct = saved.4;
            self.last_bar_drift = saved.5;
            self.last_bar_healthy = saved.6;
        } else {
            let price = bar.close;
            let cum_volume = bar.volume;
            match self.last_volume {
                None => {
                    self.last_volume = Some(cum_volume);
                    self.last_price = Some(price);
                    self.last_time = Some(tick.bar_open_ms);
                }
                Some(last) if cum_volume < last => {
                    self.rollback_count += 1;
                    self.acc_v = 0.0;
                    self.acc_b = 0.0;
                    self.acc_h = bar.high;
                    self.acc_l = bar.low;
                    self.acc_c = price;
                    self.last_price = Some(price);
                    self.last_volume = Some(cum_volume);
                    self.last_time = Some(tick.bar_open_ms);
                }
                Some(last) => {
                    let price_delta = price - self.last_price.unwrap_or(price);
                    let volume_delta = cum_volume - last;
                    self.last_price = Some(price);
                    if volume_delta > 0.0 {
                        self.tick_count += 1;
                        self.tick_volume_total += volume_delta;
                        let buy = estimate_buy_volume(volume_delta, price_delta);
                        self.tick_buy_volume += buy;
                        self.acc_v += volume_delta;
                        self.acc_b += buy;
                        self.acc_h = self.acc_h.max(price);
                        self.acc_l = if self.acc_l == 0.0 { price } else { self.acc_l.min(price) };
                        self.acc_c = price;
                        self.last_time_diff =
                            Some(tick.bar_open_ms - self.last_time.unwrap_or(tick.bar_open_ms));
                        self.last_time = Some(tick.bar_open_ms);
                        self.last_volume = Some(cum_volume);
                    }
                }
            }
        }
        let ltf = self.confirm_ltf(tick.tick_ms, bar.close, None);
        appended += ltf.finalized.len();
        if tick.is_confirmed {
            let ctf = tick.confirm.ok_or_else(|| {
                KernelError::InvalidInput(
                    "native Bars bridge: confirmed tick carries no confirm legs".to_owned(),
                )
            })?;
            let before = self.candles.len();
            let max = self.config.max_candles();
            let report = self.confirm(&ctf);
            if report.pull_needed {
                return Ok(FetchNativeOut {
                    appended,
                    ready_boundary: ltf.ready_boundary,
                    ready_distribution: ltf.ready_distribution,
                    signal: FetchSignal::PullNeeded,
                });
            }
            if self.candles.len() < max {
                appended += self.candles.len() - before;
            } else {
                return Ok(FetchNativeOut {
                    appended,
                    ready_boundary: ltf.ready_boundary,
                    ready_distribution: ltf.ready_distribution,
                    signal: FetchSignal::FallbackCapacity,
                });
            }
        }
        Ok(FetchNativeOut {
            appended,
            ready_boundary: ltf.ready_boundary,
            ready_distribution: ltf.ready_distribution,
            signal: FetchSignal::Commit,
        })
    }
}

/// Fold raw trade prints into closed lower-timeframe buckets: first open,
/// high/low extremes, last close, summed volume, per-print directional buy
/// splits against the previous print. Only closed buckets emit — the
/// trailing (still-forming) bucket stays unconfirmed, and a tape that never
/// closes a bucket refuses instead of inventing one. Sparse tapes skip
/// empty buckets; `fill_gaps` chains zero placeholders off the last close.
pub fn tick_resample(
    ticks: &[RawTick],
    ltf_ms: i64,
    fill_gaps: bool,
    lookahead: Lookahead,
) -> Result<Vec<BarParts>, KernelError> {
    lookahead.require_off()?;
    if ltf_ms <= 0 {
        return Err(KernelError::InvalidInput(format!(
            "resample span must be positive, got {ltf_ms}"
        )));
    }
    #[derive(Default)]
    struct Fold {
        o: f64,
        h: f64,
        l: f64,
        c: f64,
        v: f64,
        b: f64,
        seen: bool,
    }
    let mut buckets: BTreeMap<i64, Fold> = BTreeMap::new();
    let mut prev_price: Option<f64> = None;
    for tick in ticks {
        if !tick.price.is_finite() || !tick.volume.is_finite() {
            continue;
        }
        let delta = tick.price - prev_price.unwrap_or(tick.price);
        prev_price = Some(tick.price);
        let idx = tick.ts_ms.div_euclid(ltf_ms);
        let fold = buckets.entry(idx).or_default();
        if fold.seen {
            fold.h = fold.h.max(tick.price);
            fold.l = fold.l.min(tick.price);
        } else {
            fold.o = tick.price;
            fold.h = tick.price;
            fold.l = tick.price;
            fold.seen = true;
        }
        fold.c = tick.price;
        fold.v += tick.volume;
        fold.b += estimate_buy_volume(tick.volume, delta);
    }
    if buckets.len() < 2 {
        return Err(KernelError::InvalidInput(
            "resample refused before the first closed bar: the tape never closes a bucket".to_owned(),
        ));
    }
    let last_idx = *buckets.keys().last().expect("nonempty buckets");
    let mut out = Vec::with_capacity(buckets.len() - 1);
    let mut prev_close = 0.0;
    let keys = buckets.keys().copied();
    for idx in keys {
        if idx == last_idx {
            break;
        }
        if fill_gaps {
            if let Some(prev) = out.last().map(|bar: &BarParts| bar.ts_ms.div_euclid(ltf_ms)) {
                for missing in (prev + 1)..idx {
                    let ts = missing * ltf_ms;
                    out.push(BarParts {
                        ts_ms: ts,
                        o: prev_close,
                        h: prev_close,
                        l: prev_close,
                        c: prev_close,
                        volume: 0.0,
                        buy_volume: 0.0,
                    });
                }
            }
        }
        let fold = buckets.get(&idx).expect("iterated bucket present");
        prev_close = fold.c;
        out.push(BarParts {
            ts_ms: idx * ltf_ms,
            o: fold.o,
            h: fold.h,
            l: fold.l,
            c: fold.c,
            volume: fold.v,
            buy_volume: fold.b,
        });
    }
    Ok(out)
}

/// Project closed bars onto a coarser grid (the native
/// `request.security` equivalent): floor-aligned buckets with first open,
/// high/low extremes, last close, and conserved summed volume. Only
/// confirmed buckets emit — with `now_ms`, any trailing bucket still
/// forming drops out; [`Lookahead::On`] refuses outright.
pub fn project_bars(
    bars: &[ClosedBar],
    target_ms: i64,
    lookahead: Lookahead,
    now_ms: Option<i64>,
) -> Result<Vec<ClosedBar>, KernelError> {
    lookahead.require_off()?;
    if target_ms <= 0 {
        return Err(KernelError::InvalidInput(format!(
            "projection span must be positive, got {target_ms}"
        )));
    }
    let grid = std::num::NonZeroI64::new(target_ms).expect("positive span checked");
    let mut buckets: BTreeMap<i64, (usize, usize)> = BTreeMap::new();
    for (at, bar) in bars.iter().enumerate() {
        let idx = floor_to_grid(bar.ts_ms, grid).div_euclid(target_ms);
        buckets.entry(idx).or_insert((at, at)).1 = at;
    }
    let mut out = Vec::with_capacity(buckets.len());
    for (idx, (first, last)) in &buckets {
        let ts = idx * target_ms;
        if let Some(now) = now_ms {
            if !fully_formed(ts, grid, now) {
                continue;
            }
        }
        let head = &bars[*first];
        let tail = &bars[*last];
        let mut h = head.h;
        let mut l = head.l;
        let mut v = 0.0;
        for bar in &bars[*first..=*last] {
            h = h.max(bar.h);
            l = l.min(bar.l);
            v += bar.volume;
        }
        out.push(ClosedBar { ts_ms: ts, o: head.o, h, l, c: tail.c, volume: v });
    }
    Ok(out)
}

// --- History ---------------------------------------------------------------

/// Share counts at or below this convert to `f64` exactly; anything larger
/// would round silently, so the adapter refuses it instead. Task-value
/// axis: a precision floor for the job, mirroring [`VOLUME_EPSILON`].
pub const MAX_EXACT_VOLUME: u64 = 1 << 53;

/// Confirmed-bar gate: a bar may feed finalized output iff its whole span
/// sits at or before `now` under [`Lookahead::Off`] — the close fence is
/// `open + span <= now`, so a bar closing exactly at `now` counts.
///
/// Gaps change no verdict (missing buckets neither confirm nor un-confirm
/// their neighbors — the sparse default reports them instead of
/// materializing them); [`Lookahead::On`] refuses outright, so future-bar
/// data can never leak into finalized output through an enabled projection
/// path. The accumulator-only live tail stays separate: ticks keep folding
/// through `ingest_tick`/`confirm_ltf` and never consult this gate.
#[must_use]
pub fn is_confirmed_bar(bar_open_ms: i64, tf_ms: i64, now_ms: i64, lookahead: Lookahead) -> bool {
    if lookahead.require_off().is_err() || tf_ms <= 0 {
        return false;
    }
    let grid = std::num::NonZeroI64::new(tf_ms).expect("positive span checked");
    fully_formed(bar_open_ms, grid, now_ms)
}

/// One history row checked for the adapter: structural order plus exact,
/// finite float legs.
fn check_row(prev: Option<&Bar>, bar: &Bar) -> Result<ClosedBar, KernelError> {
    validate_new_bar(prev, bar).map_err(|e| KernelError::InvalidInput(e.to_string()))?;
    if bar.volume > MAX_EXACT_VOLUME {
        return Err(KernelError::InvalidInput(format!(
            "bar volume {} loses integer precision past 2^53: refused, never rounded",
            bar.volume
        )));
    }
    let closed = ClosedBar::from_proto_bar(bar);
    if !(closed.o.is_finite() && closed.h.is_finite() && closed.l.is_finite() && closed.c.is_finite())
    {
        return Err(KernelError::InvalidInput(format!(
            "bar legs unrepresentable at {}: refused, never NaN",
            closed.ts_ms
        )));
    }
    Ok(closed)
}

/// Backfill adapter over loaded history: confirmed rows plus the
/// confirmed-bar gate and whole-suffix rebuild on correction.
///
/// The server owns parsing and fallback (`load_history` over either history
/// arm) and composes that output into [`from_bars`](Self::from_bars) — this
/// adapter never parses, never falls back, and never reaches past the
/// kernels crate, so the crate layers stay acyclic. Stamps read as bucket
/// opens, the exchange-CSV convention the projection goldens prove; `Bar`
/// stamps map identically, never shifted. One owned row store (exact
/// `Bar`s) with one converted view (closed `f64` legs, built once) — the
/// two never drift apart because only [`from_bars`](Self::from_bars) and
/// [`rebuild_suffix`](Self::rebuild_suffix) write them, always together.
/// Per-run isolation holds by construction: owned values, no globals.
#[derive(Debug, Clone)]
pub struct BarsHistory {
    timeframe: Timeframe,
    bars: Vec<Bar>,
    closed: Vec<ClosedBar>,
}

impl BarsHistory {
    /// Load confirmed history: every row runs the one ordering rule
    /// (`validate_new_bar`, shared with the streaming feed and the CSV
    /// loader), the exact-volume guard, and the finite-legs guard. Empty
    /// input refuses, mirroring the loader's empty-tape refusal.
    pub fn from_bars(bars: &[Bar], timeframe: Timeframe) -> Result<Self, KernelError> {
        if bars.is_empty() {
            return Err(KernelError::InvalidInput("history must not be empty".to_owned()));
        }
        let mut prev = None;
        let mut closed = Vec::with_capacity(bars.len());
        for bar in bars {
            closed.push(check_row(prev, bar)?);
            prev = Some(bar);
        }
        Ok(Self { timeframe, bars: bars.to_vec(), closed })
    }

    /// Bar spacing this history was loaded at.
    #[must_use]
    pub fn timeframe(self) -> Timeframe {
        self.timeframe
    }

    /// Bar spacing in milliseconds, from the single ms table.
    #[must_use]
    pub fn tf_ms(self) -> i64 {
        self.timeframe.duration_ms()
    }

    /// Exact source rows, ascending.
    #[must_use]
    pub fn source_bars(&self) -> &[Bar] {
        &self.bars
    }

    /// Converted closed legs, ascending, pairwise with the source rows.
    #[must_use]
    pub fn bars(&self) -> &[ClosedBar] {
        &self.closed
    }

    /// Number of loaded bars (always at least one: empty refuses).
    #[must_use]
    pub fn len(&self) -> usize {
        self.bars.len()
    }

    /// Whether no bars are loaded (unreachable post-construction; the
    /// non-empty invariant lives here beside `len`).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bars.is_empty()
    }

    /// Spacing discontinuities over the loaded rows, composing the shared
    /// window detector — no second gap rule lives here. The window rebuilds
    /// per call so the stored rows stay the single source of truth.
    #[must_use]
    pub fn gaps(&self) -> Vec<BarGap> {
        let mut window = BarWindow::new(self.timeframe);
        for bar in &self.bars {
            // `from_bars` validated this exact sequence, so a push failure
            // is unreachable; stop defensively rather than panic if that
            // ever changes.
            if window.push(bar.clone()).is_err() {
                break;
            }
        }
        window.gaps()
    }

    /// Whether the bar opening at `bar_open_ms` is confirmed at `now_ms`:
    /// the free gate under this history's span.
    #[must_use]
    pub fn is_confirmed(&self, bar_open_ms: i64, now_ms: i64, lookahead: Lookahead) -> bool {
        is_confirmed_bar(bar_open_ms, self.timeframe.duration_ms(), now_ms, lookahead)
    }

    /// Whole-suffix rebuild on correction: drop every row at or past
    /// `from_ts_ms`, then load `corrected` — which must start exactly at
    /// the truncation point and chain cleanly — and return the retained
    /// prefix length. No subrange surgery: the entire suffix recomputes,
    /// so a corrected rebuild always equals a from-scratch build over the
    /// corrected tape. Misaligned starts, empty corrections, and
    /// past-the-end points refuse fail-closed (extension belongs to the
    /// live-tick path, never to a correction).
    pub fn rebuild_suffix(
        &mut self,
        from_ts_ms: i64,
        corrected: &[Bar],
    ) -> Result<usize, KernelError> {
        let Some(first) = corrected.first() else {
            return Err(KernelError::InvalidInput("correction must carry at least one bar".to_owned()));
        };
        if first.ts.timestamp_millis() != from_ts_ms {
            return Err(KernelError::InvalidInput(format!(
                "correction must start at the truncation point {from_ts_ms}, got {}",
                first.ts.timestamp_millis()
            )));
        }
        let retained =
            self.bars.partition_point(|bar| bar.ts.timestamp_millis() < from_ts_ms);
        if retained >= self.bars.len() {
            return Err(KernelError::InvalidInput(format!(
                "correction starts past the last bar: {from_ts_ms} beyond coverage"
            )));
        }
        let mut prev = self.bars[..retained].last();
        let mut checked = Vec::with_capacity(corrected.len());
        let mut converted = Vec::with_capacity(corrected.len());
        for bar in corrected {
            converted.push(check_row(prev, bar)?);
            prev = Some(bar);
            checked.push(bar.clone());
        }
        self.bars.truncate(retained);
        self.closed.truncate(retained);
        self.bars.extend(checked);
        self.closed.extend(converted);
        Ok(retained)
    }
}

impl protopyne_types::Validate for BarsHistory {
    type Err = KernelError;

    fn validate(&self) -> Result<(), Self::Err> {
        if self.bars.len() != self.closed.len() {
            return Err(KernelError::InvalidInput(format!(
                "history views diverged: {} source rows vs {} converted",
                self.bars.len(),
                self.closed.len()
            )));
        }
        let mut prev = None;
        for bar in &self.bars {
            validate_new_bar(prev, bar).map_err(|e| KernelError::InvalidInput(e.to_string()))?;
            prev = Some(bar);
        }
        Ok(())
    }
}

// --- Sessions ---------------------------------------------------------------

/// Daylight-aware regular-hours clock over millisecond stamps.
///
/// Pine session strings (`input.session("0930-1600")`) name exchange wall
/// clock; this clock evaluates them for the New York session with the US
/// daylight rules encoded directly (no timezone database): EST (UTC-5) from
/// the first Sunday of November 02:00 local to the second Sunday of March
/// 02:00 local, EDT (UTC-4) between. Membership is start-inclusive,
/// end-exclusive on wall-clock minutes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TradingSession {
    open_min: u32,
    close_min: u32,
}

impl TradingSession {
    /// Parse a `HHMM-HHMM` session string. Overnight ranges (open past
    /// close) refuse: regular sessions never wrap midnight.
    pub fn parse(session: &str) -> Result<Self, KernelError> {
        let bytes = session.as_bytes();
        let bad = || {
            KernelError::InvalidInput(format!("session must read HHMM-HHMM, got {session:?}"))
        };
        if bytes.len() != 9 || bytes[4] != b'-' {
            return Err(bad());
        }
        let num = |lo: usize| {
            if !bytes[lo].is_ascii_digit() || !bytes[lo + 1].is_ascii_digit() {
                return Err(bad());
            }
            Ok(u32::from(bytes[lo] - b'0') * 10 + u32::from(bytes[lo + 1] - b'0'))
        };
        let (oh, om, ch, cm) = (num(0)?, num(2)?, num(5)?, num(7)?);
        if oh > 23 || ch > 23 || om > 59 || cm > 59 {
            return Err(bad());
        }
        let (open_min, close_min) = (oh * 60 + om, ch * 60 + cm);
        if open_min >= close_min {
            return Err(KernelError::InvalidInput(format!(
                "session must open before it closes, got {session:?}"
            )));
        }
        Ok(Self { open_min, close_min })
    }

    /// Whether the stamp falls inside the session on its ET wall clock.
    #[must_use]
    pub fn contains(self, ts_ms: i64) -> bool {
        let mins = et_wall_minutes(ts_ms);
        mins >= self.open_min && mins < self.close_min
    }

    /// ET calendar date `(year, month, day)` of the stamp.
    #[must_use]
    pub fn session_date(self, ts_ms: i64) -> (i32, u32, u32) {
        let _ = self;
        et_civil_date(ts_ms)
    }
}

impl protopyne_types::Validate for TradingSession {
    type Err = KernelError;

    fn validate(&self) -> Result<(), Self::Err> {
        if self.open_min >= self.close_min || self.close_min > 24 * 60 {
            return Err(KernelError::InvalidInput(format!(
                "session opens {} and closes {}: must open strictly before close inside one day",
                self.open_min, self.close_min
            )));
        }
        Ok(())
    }
}

/// Whether America/New_York observes daylight saving at the stamp: from the
/// second Sunday of March 07:00 UTC up to the first Sunday of November
/// 06:00 UTC (02:00 local both ends).
#[must_use]
pub fn is_dst_et(ts_ms: i64) -> bool {
    let (year, _, _) = civil_from_days(ts_ms.div_euclid(86_400_000));
    ts_ms >= dst_spring_utc_ms(year) && ts_ms < dst_fall_utc_ms(year)
}

/// UTC offset of America/New_York at the stamp, in milliseconds.
#[must_use]
pub fn et_offset_ms(ts_ms: i64) -> i64 {
    if is_dst_et(ts_ms) { -4 * 3_600_000 } else { -5 * 3_600_000 }
}

/// Spring-forward instant: second Sunday of March, 07:00 UTC.
fn dst_spring_utc_ms(year: i32) -> i64 {
    let first = days_from_civil(year, 3, 1);
    let to_sunday = (7 - weekday_of(first)) % 7;
    (first + to_sunday + 7) * 86_400_000 + 7 * 3_600_000
}

/// Fall-back instant: first Sunday of November, 06:00 UTC.
fn dst_fall_utc_ms(year: i32) -> i64 {
    let first = days_from_civil(year, 11, 1);
    let to_sunday = (7 - weekday_of(first)) % 7;
    (first + to_sunday) * 86_400_000 + 6 * 3_600_000
}

/// Monday = 1 through Sunday = 7 for a day count (day zero was a Thursday).
fn weekday_of(day_count: i64) -> i64 {
    (day_count.rem_euclid(7) + 3) % 7 + 1
}

fn et_wall_minutes(ts_ms: i64) -> u32 {
    let local = ts_ms + et_offset_ms(ts_ms);
    local.rem_euclid(86_400_000).div_euclid(60_000) as u32
}

fn et_civil_date(ts_ms: i64) -> (i32, u32, u32) {
    civil_from_days((ts_ms + et_offset_ms(ts_ms)).div_euclid(86_400_000))
}

/// Days since the Unix epoch for a proleptic Gregorian date.
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let adj_year = i64::from(if month <= 2 { year - 1 } else { year });
    let era = adj_year.div_euclid(400);
    let yoe = adj_year - era * 400;
    let mp = (i64::from(month) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Proleptic Gregorian date for days since the Unix epoch.
fn civil_from_days(day_count: i64) -> (i32, u32, u32) {
    let shifted = day_count + 719_468;
    let era = shifted.div_euclid(146_097);
    let doe = shifted - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let full_year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = (if month <= 2 { full_year + 1 } else { full_year }) as i32;
    (year, month, day)
}

#[cfg(test)]
#[path = "../tests/bars.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/bars_calibration.rs"]
mod bars_calibration_tests;

#[cfg(test)]
#[path = "../tests/bars_history.rs"]
mod bars_history_tests;
