//! Native IntegrationLite signal: closed-bar, non-repainting decisions over
//! kernels only.
//!
//! The signal reads a committed (closed-bar) tape through the same kernel
//! recurrences the parity harness pins: oscillator fast/slow legs with
//! trailing smoothing and gain-aware thresholds, the EIT adaptive + typed
//! signal/mean ribbon pair, health folds, and shape-estimated delta splits.
//! Session context enters through the parsed session string; regime context
//! through the hysteresis-stable label while the frozen probe stays
//! untouched. Every-bar versus confirmed-bar handling is a caller contract:
//! the caller passes closed bars only, the instance caches the last
//! committed decision, and re-evaluating an unchanged tape returns the cache
//! — so forming-bar re-evaluation can never revise a confirmed decision.
//! No I/O, no clock reads inside `evaluate`; sizing, stops, and exits stay
//! in their own elements.

use std::sync::Arc;

use fluent_wvr::validate::Validate;
use protopyne_kernels::indicator::builtin_registry;
use protopyne_kernels::lite::{self, lite_legs};
use protopyne_types::session::TradingSession;

use crate::slots::filter::{Admission, Filter, FilterScope};
use crate::slots::trigger::{Trigger, TriggerFamily};
use crate::tape::Tape;

use crate::elements::{Signal, SignalConfig, SignalInstance, SignalOutput};
use crate::error::StrategyError;
use crate::integration_lite::{
    entry_from_config, EntrySpec, FilterSlot, IntegrationLiteStrategyConfig, TriggerSlot,
};
use crate::proposal::Direction;

/// The market-leg dials, owned by the kernel crate: one definition shared
/// with the chart pane.
pub use protopyne_kernels::lite::{LiteLegs, LiteParams as LiteIndicatorParams};

/// Every leg the signal reads over closed bars, batched once per run.
///
/// This is the strategy-facing entry to the shared batch: the one
/// implementation lives in the kernel crate, and the error is mapped onto the
/// strategy vocabulary so a caller never has to know which crate refused.
pub fn market_legs(
    indicator: &LiteIndicatorParams,
    risk_atr_len: i64,
    bars: &[protopyne_types::Bar],
) -> Result<LiteLegs, StrategyError> {
    lite_legs(bars, indicator, risk_atr_len)
        .map_err(|error| StrategyError::InvalidElement(format!("leg batch: {error}")))
}

/// One native IntegrationLite signal: closed-bar entries over kernels.
pub struct IntegrationLiteSignal {
    name: String,
    indicator: LiteIndicatorParams,
    strategy: IntegrationLiteStrategyConfig,
    session: Option<TradingSession>,
}

impl IntegrationLiteSignal {
    /// Build the signal over explicit dials. Refuses at construction:
    /// either config invalid, the session malformed while armed, or a
    /// kernel id missing from the registry.
    pub fn new(
        indicator: LiteIndicatorParams,
        strategy: IntegrationLiteStrategyConfig,
    ) -> Result<Self, StrategyError> {
        indicator
            .validate()
            .map_err(|e| StrategyError::InvalidElement(e.to_string()))?;
        strategy
            .validate()
            .map_err(|e| StrategyError::InvalidElement(e.to_string()))?;
        let session =
            if indicator.session_filter {
                Some(TradingSession::parse(&indicator.session).map_err(|e| {
                    StrategyError::InvalidElement(format!("malformed session: {e}"))
                })?)
            } else {
                None
            };
        let registry = builtin_registry().map_err(|e| kernel_error(&e))?;
        for kind in [&indicator.fast_kernel, &indicator.slow_kernel] {
            if registry.get(kind).is_none() {
                return Err(StrategyError::InvalidElement(format!(
                    "unknown indicator id {kind:?}"
                )));
            }
        }
        Ok(Self {
            name: "integration_lite".to_owned(),
            indicator,
            strategy,
            session,
        })
    }

    /// Override the attribution name.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Bars the instance needs before it can decide: the deepest kernel
    /// window plus trailing smoothing, the signal/mean/ATR/volume/MA legs,
    /// the trigger lookbacks, and the strategy warmup dial.
    #[must_use]
    pub fn required_warmup(&self) -> usize {
        let s = &self.strategy;
        let i = &self.indicator;
        let kernel = ([
            i.fast_length,
            i.slow_length,
            i.fast_smooth_length,
            i.slow_smooth_length,
            i.signal_length,
            i.mean_length,
            lite::ATR_LENGTH,
            lite::VOL_SMA_LENGTH,
            lite::MA_PRIMARY_LENGTH,
            lite::MA_SIGNAL_LENGTH,
            lite::MA_MEAN_LENGTH,
            s.sig_dual_lookback,
            s.filt_delta_len,
        ]
        .into_iter()
        .max()
        .unwrap_or(14) as usize)
            + 2;
        kernel.max(s.warmup_bars.max(0) as usize)
    }
}

fn kernel_error(error: &protopyne_kernels::KernelError) -> StrategyError {
    StrategyError::InvalidElement(format!("kernel registry: {error}"))
}

impl Signal for IntegrationLiteSignal {
    fn name(&self) -> &str {
        &self.name
    }

    fn bind(&self, _cfg: &SignalConfig) -> Result<Box<dyn SignalInstance>, StrategyError> {
        Ok(Box::new(IntegrationLiteSignalInstance {
            signal: self.clone_for_instance(),
            committed_len: 0,
            cached: None,
            filters: build_filters(&self.strategy),
            precomputed: None,
        }))
    }

    fn evaluate(&self, view: &crate::view::MarketView<'_>) -> Result<SignalOutput, StrategyError> {
        evaluate_stateless(self, view)
    }
}

impl IntegrationLiteSignal {
    /// Bind an instance over legs batched once for the whole run.
    ///
    /// The batch is causal, so indexing it at each view's last bar reads the
    /// same values a per-view prefix batch would have produced; the equality
    /// is pinned by test. This is the seam that keeps a long run linear.
    pub fn bind_with_legs(
        &self,
        _cfg: &SignalConfig,
        legs: Arc<LiteLegs>,
    ) -> Result<Box<dyn SignalInstance>, StrategyError> {
        Ok(Box::new(IntegrationLiteSignalInstance {
            signal: self.clone_for_instance(),
            committed_len: 0,
            cached: None,
            filters: build_filters(&self.strategy),
            precomputed: Some(legs),
        }))
    }

    fn clone_for_instance(&self) -> InstanceShared {
        InstanceShared {
            indicator: self.indicator.clone(),
            strategy: self.strategy.clone(),
            session: self.session,
            triggers: Arc::new(build_triggers(&self.strategy)),
            warmup: self.required_warmup(),
        }
    }
}

/// Shareable evaluation inputs (the signal itself is not `Clone` over the
/// trait object boundary concerns; the instance holds this instead).
#[derive(Clone)]
struct InstanceShared {
    indicator: LiteIndicatorParams,
    strategy: IntegrationLiteStrategyConfig,
    session: Option<TradingSession>,
    triggers: Arc<Vec<Arc<dyn Trigger>>>,
    warmup: usize,
}

/// One run's signal state: the last committed tape length plus its decision
/// (forming-bar re-evaluation returns the cache), and the held regime label
/// behind the hysteresis-stable gate.
pub struct IntegrationLiteSignalInstance {
    signal: InstanceShared,
    committed_len: usize,
    cached: Option<SignalOutput>,
    filters: Vec<Arc<dyn Filter>>,
    /// Legs batched once for the whole run, when the caller supplies them.
    /// The batch is causal, so indexing it at the view's last bar reads the
    /// same values a prefix batch would have produced.
    precomputed: Option<Arc<LiteLegs>>,
}

impl SignalInstance for IntegrationLiteSignalInstance {
    fn evaluate(
        &mut self,
        view: &crate::view::MarketView<'_>,
    ) -> Result<SignalOutput, StrategyError> {
        let len = view.data.bars.len();
        if len == self.committed_len {
            if let Some(cached) = self.cached {
                return Ok(cached);
            }
        }
        let out = evaluate_stateless_shared(
            &self.signal,
            view,
            &self.filters,
            self.precomputed.as_deref(),
        )?;
        self.committed_len = len;
        self.cached = Some(out);
        Ok(out)
    }

    fn warmup(&self) -> usize {
        self.signal.warmup
    }
}

fn evaluate_stateless(
    signal: &IntegrationLiteSignal,
    view: &crate::view::MarketView<'_>,
) -> Result<SignalOutput, StrategyError> {
    let filters = build_filters(&signal.strategy);
    evaluate_stateless_shared(&signal.clone_for_instance(), view, &filters, None)
}

/// Shared evaluation core: batch kernels over the closed tape, arm triggers
/// within the setup window, pass every enabled filter, emit one decision.
/// Refuses (never substitutes) when the tape is short, the session is
/// closed, no trigger is armed, a filter blocks, or both sides arm at once.
fn evaluate_stateless_shared(
    signal: &InstanceShared,
    view: &crate::view::MarketView<'_>,
    filters: &[Arc<dyn Filter>],
    precomputed: Option<&LiteLegs>,
) -> Result<SignalOutput, StrategyError> {
    let bars = view.data.bars;
    let refuse = |reason: &str| StrategyError::InvalidElement(reason.to_owned());
    if bars.len() < signal.warmup {
        return Err(refuse(&format!(
            "warmup: {} bars fed, {} needed",
            bars.len(),
            signal.warmup
        )));
    }
    if !signal.strategy.enabled {
        return Err(refuse("disabled: trading is switched off"));
    }
    if let Some(session) = signal.session {
        let last_ms = bars.last().expect("non-empty").ts.timestamp_millis();
        if !session.contains(last_ms) {
            return Err(refuse("session: last bar sits outside the trading session"));
        }
    }
    // One batch per run: the caller may hand in legs computed over the whole
    // tape, and the causal batch makes that identical to this view's prefix.
    let owned;
    let legs = if let Some(legs) = precomputed {
        if legs.len() < bars.len() {
            return Err(refuse(
                "precomputed legs are shorter than the tape they are indexed with",
            ));
        }
        legs
    } else {
        owned = lite_legs(bars, &signal.indicator, signal.strategy.risk_atr_len)
            .map_err(|error| StrategyError::InvalidElement(format!("leg batch: {error}")))?;
        &owned
    };
    let n = bars.len();
    let last = n - 1;

    // Trigger arming within the setup window (0 = the closing bar only).
    let window = signal.strategy.sig_window.max(0) as usize;
    let mut armed = Armed::default();
    for back in 0..=window.min(last.saturating_sub(1)) {
        let i = last - back;
        if i < 1 {
            continue;
        }
        if let Some(tape) = Tape::new(bars, legs, view.closes, i) {
            armed = armed.merge(triggers_at(&signal.triggers, &tape));
        }
    }
    if armed.long == armed.short {
        return Err(refuse(if armed.long {
            "ambiguous: both sides armed on the closing window"
        } else {
            "no trigger armed on the closing window"
        }));
    }
    let long = armed.long;

    // Every enabled filter must pass for the armed side. The counter-trend
    // arm may bypass the trend filters when the operator asks it to: it is a
    // different trade thesis, not a stronger reading, so it carries its own
    // admission instead of borrowing the trend one.
    let dual_only = if long {
        armed.dual_long && !armed.long_from_trend()
    } else {
        armed.dual_short && !armed.short_from_trend()
    };
    let bypass = signal.strategy.sig_dual && signal.strategy.sig_dual_bypass && dual_only;
    let filter_set = if bypass {
        FilterSet::CounterTrend
    } else {
        FilterSet::Full
    };
    let tape = Tape::new(bars, legs, view.closes, last)
        .ok_or_else(|| refuse("precomputed legs did not align with the tape"))?;
    let direction = if long {
        Direction::Long
    } else {
        Direction::Short
    };
    if let Err(reason) = filters_pass(filters, &tape, direction, filter_set) {
        return Err(refuse(reason));
    }

    // Direction gate from the strategy dial.
    match signal.strategy.direction.as_str() {
        "Long Only" if !long => return Err(refuse("direction: strategy allows long only")),
        "Short Only" if long => return Err(refuse("direction: strategy allows short only")),
        _ => {}
    }

    // Task-value scores: slow-leg agreement plus health alignment feed
    // conviction; volume conviction feeds activity; realised volatility is
    // measured over the trailing window, `None` only when unmeasurable.
    let agree_slow = legs
        .slow
        .get(last)
        .copied()
        .flatten()
        .is_some_and(|v| (long && v > 0.0) || (!long && v < 0.0));
    let agree_health = legs
        .ribbon_health
        .get(last)
        .copied()
        .flatten()
        .is_some_and(|v| (long && v > 0.0) || (!long && v < 0.0));
    let conviction =
        (0.5 + 0.25 * f64::from(agree_slow) + 0.25 * f64::from(agree_health)).clamp(0.0, 1.0);
    let activity = (legs.vol_ratio[last] / (1.0 + legs.vol_ratio[last])).clamp(0.0, 1.0);
    let volatility = realised_volatility(view.closes);
    Ok(SignalOutput {
        direction: if long {
            Direction::Long
        } else {
            Direction::Short
        },
        conviction,
        activity_score: activity,
        volatility,
        edge: None,
        odds: None,
    })
}

/// Realised per-bar volatility of simple returns over the trailing window;
/// `None` only when unmeasurable (fewer than two closes or non-finite
/// input). Zero variance measures as `Some(0.0)`, never `None`.
fn realised_volatility(closes: &[f64]) -> Option<f64> {
    if closes.len() < 2 {
        return None;
    }
    let start = closes.len().saturating_sub(lite::VOLATILITY_LENGTH + 1);
    let window = &closes[start..];
    let mut sum = 0.0;
    let mut sum_sq = 0.0;
    let mut count = 0_usize;
    for pair in window.windows(2) {
        let (prev, curr) = (pair[0], pair[1]);
        if !prev.is_finite() || !curr.is_finite() || prev == 0.0 {
            return None;
        }
        let r = curr / prev - 1.0;
        sum += r;
        sum_sq += r * r;
        count += 1;
    }
    if count == 0 {
        return None;
    }
    let mean = sum / count as f64;
    let var = (sum_sq / count as f64 - mean * mean).max(0.0);
    Some(var.sqrt())
}

/// Which triggers armed one bar, by side and by family.
///
/// The split matters because the counter-trend arm is a different trade thesis:
/// it may be admitted through its own filter set, and it must not be read as
/// evidence that the trend triggers also fired.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // six independent verdicts, not a state machine
struct Armed {
    /// Any long trigger fired.
    long: bool,
    /// Any short trigger fired.
    short: bool,
    /// The counter-trend exhaustion trigger fired long.
    dual_long: bool,
    /// The counter-trend exhaustion trigger fired short.
    dual_short: bool,
    /// A trend-following trigger (momentum, exhaustion reversal, ribbon flip,
    /// oscillator cross) fired long.
    trend_long: bool,
    /// A trend-following trigger fired short.
    trend_short: bool,
}

impl Armed {
    fn merge(self, other: Self) -> Self {
        Self {
            long: self.long || other.long,
            short: self.short || other.short,
            dual_long: self.dual_long || other.dual_long,
            dual_short: self.dual_short || other.dual_short,
            trend_long: self.trend_long || other.trend_long,
            trend_short: self.trend_short || other.trend_short,
        }
    }

    fn long_from_trend(self) -> bool {
        self.trend_long
    }

    fn short_from_trend(self) -> bool {
        self.trend_short
    }
}

/// Which filters apply to one admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilterSet {
    /// Every enabled filter (the trend path).
    Full,
    /// The counter-trend admission: session, regime, relative volume, and the
    /// delta alignment. The trend filters are skipped by design because the
    /// trade thesis is exhaustion, not continuation.
    CounterTrend,
}

/// Map one nested trigger row onto catalog build parameters.
///
/// The mapping is total over the catalog: every row kind the projection
/// emits builds. A row kind with no mapping refuses at build time instead
/// of silently arming nothing.
fn trigger_params_for_slot(slot: &TriggerSlot) -> serde_json::Value {
    match slot.kind.as_str() {
        "momentum" => serde_json::json!({
            "long_level": slot.level,
            "short_level": -slot.level,
            "adaptive": slot.adaptive,
        }),
        "ribbon_flip" => serde_json::json!({}),
        "osc_cross" => serde_json::json!({"fast": "fast", "slow": "slow"}),
        "exhaustion" => serde_json::json!({"band": slot.level}),
        "dual_exhaustion" => serde_json::json!({
            "slow_level": slot.level,
            "fast_level": slot.fast_level,
            "lookback": slot.lookback.max(0),
        }),
        _ => serde_json::json!({ "kind": slot.kind }),
    }
}

/// Map one nested filter row onto catalog build parameters.
fn filter_params_for_slot(slot: &FilterSlot) -> serde_json::Value {
    match slot.kind.as_str() {
        "ribbon" | "ribbon_health" => serde_json::json!({}),
        "ma_health" | "slow_side" | "volume_min" => serde_json::json!({"minimum": slot.minimum}),
        "delta_side" => serde_json::json!({"length": slot.length.max(1)}),
        "atr_distance_max" => serde_json::json!({"maximum": slot.maximum}),
        "regime_skip" => serde_json::json!({
            "low_volume": slot.low_volume,
            "volatile": slot.volatile,
        }),
        _ => serde_json::json!({ "kind": slot.kind }),
    }
}

/// Build the trigger set from the nested entry section, in row order.
///
/// This is the canonical assembly: the flat config reaches the slots only
/// through the [`entry_from_config`] projection. Unknown row kinds refuse
/// at build time (the catalog errors); they never silently arm nothing.
///
/// The rows are stateless (every trigger is `Copy` with no bar state), so
/// rebuilding per call — as the position exit path does every bar — is
/// behavior-neutral. A stateful row must never be added here without moving
/// the build to bind time.
#[must_use]
pub fn build_triggers_from_entry(entry: &EntrySpec) -> Vec<Arc<dyn Trigger>> {
    use crate::slots::trigger::triggers;

    entry
        .triggers
        .iter()
        .map(|slot| {
            triggers()
                .build(&slot.kind, &trigger_params_for_slot(slot))
                .expect("nested entry rows always build their catalog slots")
        })
        .collect()
}

/// Build the admission guards from the nested entry section, in row order.
///
/// Same canonicality contract as [`build_triggers_from_entry`]: rows that
/// can carry bar state (the regime hysteresis) build once per instance at
/// bind time, never per bar.
#[must_use]
pub fn build_filters_from_entry(entry: &EntrySpec) -> Vec<Arc<dyn Filter>> {
    use crate::slots::filter::filters;

    entry
        .filters
        .iter()
        .filter(|slot| slot.enable)
        .map(|slot| {
            filters()
                .build(&slot.kind, &filter_params_for_slot(slot))
                .expect("nested entry rows always build their catalog slots")
        })
        .collect()
}

/// Build the trigger set from the flat config, in the reference's dispatch
/// order.
///
/// Compatibility wrapper: the flat config projects onto the nested entry
/// and builds from there, so the flat struct reaches the slots only through
/// the canonical assembly. The order matters because the first arming side
/// wins a tie only through the ambiguity refusal.
#[must_use]
pub fn build_triggers(strategy: &IntegrationLiteStrategyConfig) -> Vec<Arc<dyn Trigger>> {
    build_triggers_from_entry(&entry_from_config(strategy))
}

/// Merge every trigger's verdict into the side/family flags the admission
/// path reads.
fn triggers_at(triggers: &[Arc<dyn Trigger>], tape: &Tape<'_>) -> Armed {
    let mut armed = Armed::default();
    for trigger in triggers {
        let verdict = trigger.eval(tape);
        let counter = verdict.family == TriggerFamily::CounterTrend;
        armed = Armed {
            long: armed.long || verdict.long,
            short: armed.short || verdict.short,
            dual_long: armed.dual_long || (counter && verdict.long),
            dual_short: armed.dual_short || (counter && verdict.short),
            trend_long: armed.trend_long || (!counter && verdict.long),
            trend_short: armed.trend_short || (!counter && verdict.short),
        };
    }
    armed
}

/// The entry triggers armed on the closing bar, for callers that need the raw
/// trigger before filters (the position exit path: an opposite trigger closes
/// a trade whether or not it would have passed the entry filters).
#[must_use]
pub fn entry_triggers(
    strategy: &IntegrationLiteStrategyConfig,
    legs: &LiteLegs,
    bars: &[protopyne_types::Bar],
    at: usize,
) -> (bool, bool) {
    if at < 1 {
        return (false, false);
    }
    let Some(tape) = Tape::new(bars, legs, &legs.close, at) else {
        return (false, false);
    };
    let triggers = build_triggers(strategy);
    let armed = triggers_at(&triggers, &tape);
    (armed.long, armed.short)
}

/// Build the admission guards from the flat config, in the reference's
/// evaluation order.
///
/// Compatibility wrapper: the flat config projects onto the nested entry
/// and builds from there. The order is load-bearing: the regime label is
/// written last, so an earlier block leaves the hysteresis state untouched
/// exactly as the inline pass did — and because rows can carry bar state
/// (the regime hysteresis), the build happens once per signal instance at
/// bind time, never per bar.
#[must_use]
pub fn build_filters(strategy: &IntegrationLiteStrategyConfig) -> Vec<Arc<dyn Filter>> {
    build_filters_from_entry(&entry_from_config(strategy))
}

/// Every applicable filter must pass for the armed side. `TrendOnly`
/// guards are skipped for the counter-trend admission, which carries its own
/// thesis.
fn filters_pass(
    filters: &[Arc<dyn Filter>],
    tape: &Tape<'_>,
    direction: Direction,
    set: FilterSet,
) -> Result<(), &'static str> {
    for filter in filters {
        if set == FilterSet::CounterTrend && filter.scope() == FilterScope::TrendOnly {
            continue;
        }
        if let Admission::Block(reason) = filter.admit(tape, direction) {
            return Err(reason);
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "../tests/integration_lite_signal.rs"]
mod tests;
