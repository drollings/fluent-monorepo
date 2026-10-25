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

use protopyne_kernels::bars::estimate_delta_volume_estimated;
use protopyne_kernels::indicator::{builtin_registry, IndicatorRegistry, OwnedOhlcv, ParamSet};
use protopyne_kernels::market_context::{detect_regime_stable, health_mas, MarketRegime, HEALTH_K};
use protopyne_kernels::price_source::PriceSource;
use protopyne_kernels::smoothing::{SmoothingArgs, SmoothingFamily, MA_PERIOD_MAX, MA_PERIOD_MIN};
use protopyne_types::session::TradingSession;

use crate::elements::{Signal, SignalConfig, SignalInstance, SignalOutput};
use crate::error::StrategyError;
use crate::integration_lite::IntegrationLiteStrategyConfig;
use crate::proposal::Direction;

/// Frozen affine constants mirroring the calibrated display catalog, per
/// lite leg kind: `(offset, gain slope, scale)`. The display crate owns the
/// catalog; these values are transcribed here (not referenced) because the
/// strategy layer may not import the terminal, and any drift fails the
/// per-kind calibration the moment a leg moves.
const LEG_CONSTANTS: &[(&str, (f64, f64, f64))] = &[
    ("rsi", (-50.0, 0.0084, 740.0)),
    ("cci", (-50.0, 0.0035, 500.0)),
    ("cmo", (0.0, 0.002, 1000.0)),
    ("mfi", (-50.0, 0.0084, 740.0)),
    ("stoch", (-50.0, 0.0005, 7000.0)),
];

/// Lite legs the signal may dial: kernel-backed single-output arms. The
/// terminal resolves blanks against its panel rows; the signal takes
/// explicit dials and refuses anything outside this set instead of
/// substituting.
const LEG_KINDS: &[&str] = &["rsi", "cci", "cmo", "mfi", "stoch"];

/// ATR length behind health distances and the extension filter.
const ATR_LENGTH: i64 = 14;
/// Volume SMA length behind conviction.
const VOL_SMA_LENGTH: i64 = 20;
/// ALMA/VWMA/EMA lengths behind the MA-health fold.
const MA_PRIMARY_LENGTH: i64 = 8;
const MA_SIGNAL_LENGTH: i64 = 12;
const MA_MEAN_LENGTH: i64 = 21;
/// Trailing window behind realised volatility.
const VOLATILITY_LENGTH: usize = 20;

/// Indicator dials the signal math reads: the terminal-config mirror with
/// script defaults. Strategy triggers and filters live in
/// [`IntegrationLiteStrategyConfig`]; this struct carries only the market
/// legs. Anything outside the kernel catalogs refuses at construction.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LiteIndicatorParams {
    /// Fast-arm kernel id.
    pub fast_kernel: String,
    /// Fast-arm window in bars.
    pub fast_length: i64,
    /// Fast-arm trailing smoothing family (`"na"` disarms).
    pub fast_smooth: String,
    /// Fast-arm trailing smoothing window.
    pub fast_smooth_length: i64,
    /// Fast-arm display gain.
    pub fast_gain: f64,
    /// Slow-arm kernel id.
    pub slow_kernel: String,
    /// Slow-arm window in bars.
    pub slow_length: i64,
    /// Slow-arm trailing smoothing family.
    pub slow_smooth: String,
    /// Slow-arm trailing smoothing window.
    pub slow_smooth_length: i64,
    /// Slow-arm display gain.
    pub slow_gain: f64,
    /// Adaptive EIT smoothing.
    pub eit_alpha: f64,
    /// Ribbon signal-leg smoothing family.
    pub signal_type: String,
    /// Ribbon signal-leg window.
    pub signal_length: i64,
    /// Ribbon signal-leg parameter (EIT alpha / ALMA offset).
    pub signal_param1: f64,
    /// Ribbon mean-leg smoothing family.
    pub mean_type: String,
    /// Ribbon mean-leg window.
    pub mean_length: i64,
    /// Ribbon mean-leg parameter.
    pub mean_param1: f64,
    /// Ribbon-health gain.
    pub ribbon_health_k: f64,
    /// MA-health gain.
    pub ma_health_k: f64,
    /// OHLC column the arms read when the adaptive relay is off.
    pub price_source: String,
    /// Read the adaptive EIT relay instead of the price source.
    pub adaptive: bool,
    /// Session filter switch (wired here, not display-only).
    pub session_filter: bool,
    /// Session window in `HHMM-HHMM` form.
    pub session: String,
}

impl Default for LiteIndicatorParams {
    fn default() -> Self {
        Self {
            fast_kernel: "rsi".to_owned(),
            fast_length: 4,
            fast_smooth: "na".to_owned(),
            fast_smooth_length: 3,
            fast_gain: 1.2,
            slow_kernel: "cci".to_owned(),
            slow_length: 8,
            slow_smooth: "na".to_owned(),
            slow_smooth_length: 3,
            slow_gain: 1.2,
            eit_alpha: 0.14,
            signal_type: "eit".to_owned(),
            signal_length: 10,
            signal_param1: 0.12,
            mean_type: "ema".to_owned(),
            mean_length: 21,
            mean_param1: 0.06,
            ribbon_health_k: 1.8,
            ma_health_k: 1.2,
            price_source: "close".to_owned(),
            adaptive: false,
            session_filter: true,
            session: "0930-1600".to_owned(),
        }
    }
}

impl LiteIndicatorParams {
    /// Fail-closed self-consistency: every kernel id registered, every
    /// window inside the shared MA bounds, every smoothing family known,
    /// gains finite and positive, source parsed, session parsed when armed.
    pub fn validate(&self) -> Result<(), StrategyError> {
        let bad =
            |slot: &str, reason: String| StrategyError::InvalidElement(format!("{slot}: {reason}"));
        for (slot, kind) in [
            ("fast_kernel", &self.fast_kernel),
            ("slow_kernel", &self.slow_kernel),
        ] {
            if !LEG_KINDS.contains(&kind.as_str()) {
                return Err(bad(slot, format!("{kind:?} is not a lite leg kind")));
            }
        }
        for (slot, window) in [
            ("fast_length", self.fast_length),
            ("slow_length", self.slow_length),
            ("fast_smooth_length", self.fast_smooth_length),
            ("slow_smooth_length", self.slow_smooth_length),
            ("signal_length", self.signal_length),
            ("mean_length", self.mean_length),
        ] {
            if !(MA_PERIOD_MIN..=MA_PERIOD_MAX).contains(&window) {
                return Err(bad(
                    slot,
                    format!("window {window} outside [{MA_PERIOD_MIN}, {MA_PERIOD_MAX}]"),
                ));
            }
        }
        for (slot, family) in [
            ("fast_smooth", &self.fast_smooth),
            ("slow_smooth", &self.slow_smooth),
            ("signal_type", &self.signal_type),
            ("mean_type", &self.mean_type),
        ] {
            if SmoothingFamily::parse(family).is_none() {
                return Err(bad(slot, format!("unknown smoothing family {family:?}")));
            }
        }
        for (slot, gain) in [("fast_gain", self.fast_gain), ("slow_gain", self.slow_gain)] {
            if !gain.is_finite() || gain <= 0.0 {
                return Err(bad(
                    slot,
                    format!("gain {gain} must be positive and finite"),
                ));
            }
        }
        if !self.eit_alpha.is_finite() || self.eit_alpha <= 0.0 || self.eit_alpha > 1.0 {
            return Err(bad(
                "eit_alpha",
                format!(
                    "adaptive smoothing {} must sit within (0, 1]",
                    self.eit_alpha
                ),
            ));
        }
        for (slot, param1) in [
            ("signal_param1", self.signal_param1),
            ("mean_param1", self.mean_param1),
        ] {
            if !param1.is_finite() || param1 < 0.0 {
                return Err(bad(
                    slot,
                    format!("parameter {param1} must be finite and non-negative"),
                ));
            }
        }
        for (slot, gain) in [
            ("ribbon_health_k", self.ribbon_health_k),
            ("ma_health_k", self.ma_health_k),
        ] {
            if !gain.is_finite() || gain <= 0.0 {
                return Err(bad(
                    slot,
                    format!("gain {gain} must be positive and finite"),
                ));
            }
        }
        if PriceSource::parse(&self.price_source).is_none() {
            return Err(bad(
                "price_source",
                format!("unknown input source {:?}", self.price_source),
            ));
        }
        if self.session_filter && TradingSession::parse(&self.session).is_err() {
            return Err(bad(
                "session",
                format!("malformed session {:?}", self.session),
            ));
        }
        Ok(())
    }
}

/// One native IntegrationLite signal: closed-bar entries over kernels.
pub struct IntegrationLiteSignal {
    name: String,
    indicator: LiteIndicatorParams,
    strategy: IntegrationLiteStrategyConfig,
    session: Option<TradingSession>,
    registry: Arc<IndicatorRegistry>,
}

impl IntegrationLiteSignal {
    /// Build the signal over explicit dials. Refuses at construction:
    /// either config invalid, the session malformed while armed, or a
    /// kernel id missing from the registry.
    pub fn new(
        indicator: LiteIndicatorParams,
        strategy: IntegrationLiteStrategyConfig,
    ) -> Result<Self, StrategyError> {
        indicator.validate()?;
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
        let registry = Arc::new(builtin_registry().map_err(|e| kernel_error(&e))?);
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
            registry,
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
            ATR_LENGTH,
            VOL_SMA_LENGTH,
            MA_PRIMARY_LENGTH,
            MA_SIGNAL_LENGTH,
            MA_MEAN_LENGTH,
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
            prev_regime: MarketRegime::default(),
        }))
    }

    fn evaluate(&self, view: &crate::view::MarketView<'_>) -> Result<SignalOutput, StrategyError> {
        evaluate_stateless(self, view, &mut MarketRegime::default())
    }
}

impl IntegrationLiteSignal {
    fn clone_for_instance(&self) -> InstanceShared {
        InstanceShared {
            indicator: self.indicator.clone(),
            strategy: self.strategy.clone(),
            session: self.session,
            registry: Arc::clone(&self.registry),
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
    registry: Arc<IndicatorRegistry>,
    warmup: usize,
}

/// One run's signal state: the last committed tape length plus its decision
/// (forming-bar re-evaluation returns the cache), and the held regime label
/// behind the hysteresis-stable gate.
pub struct IntegrationLiteSignalInstance {
    signal: InstanceShared,
    committed_len: usize,
    cached: Option<SignalOutput>,
    prev_regime: MarketRegime,
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
        let mut prev = self.prev_regime;
        let out = evaluate_stateless_shared(&self.signal, view, &mut prev)?;
        self.prev_regime = prev;
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
    prev_regime: &mut MarketRegime,
) -> Result<SignalOutput, StrategyError> {
    evaluate_stateless_shared(&signal.clone_for_instance(), view, prev_regime)
}

/// Shared evaluation core: batch kernels over the closed tape, arm triggers
/// within the setup window, pass every enabled filter, emit one decision.
/// Refuses (never substitutes) when the tape is short, the session is
/// closed, no trigger is armed, a filter blocks, or both sides arm at once.
fn evaluate_stateless_shared(
    signal: &InstanceShared,
    view: &crate::view::MarketView<'_>,
    prev_regime: &mut MarketRegime,
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
    if let Some(session) = signal.session {
        let last_ms = bars.last().expect("non-empty").ts.timestamp_millis();
        if !session.contains(last_ms) {
            return Err(refuse("session: last bar sits outside the trading session"));
        }
    }
    let legs = lite_legs(signal, bars)?;
    let n = bars.len();
    let last = n - 1;

    // Trigger arming within the setup window (0 = the closing bar only).
    let window = signal.strategy.sig_window.max(0) as usize;
    let mut long_armed = false;
    let mut short_armed = false;
    for back in 0..=window.min(last.saturating_sub(1)) {
        let i = last - back;
        if i < 1 {
            continue;
        }
        let (long, short) = triggers_at(signal, &legs, i);
        long_armed |= long;
        short_armed |= short;
    }
    if long_armed == short_armed {
        return Err(refuse(if long_armed {
            "ambiguous: both sides armed on the closing window"
        } else {
            "no trigger armed on the closing window"
        }));
    }
    let long = long_armed;

    // Every enabled filter must pass for the armed side.
    if !filters_pass(signal, &legs, bars, last, long, prev_regime) {
        return Err(refuse("blocked: an enabled filter did not pass"));
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
    let agree_slow = legs.slow[last].is_some_and(|v| (long && v > 0.0) || (!long && v < 0.0));
    let agree_health =
        legs.ribbon_health[last].is_some_and(|v| (long && v > 0.0) || (!long && v < 0.0));
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
    let start = closes.len().saturating_sub(VOLATILITY_LENGTH + 1);
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

/// Evaluated legs over the closed tape, index-aligned with `bars`.
/// Evaluated legs over a closed tape, index-aligned with `bars`: the
/// same batch the signal decides on, exposed so the equity runner's exits,
/// trailing, and ATR stops read the identical series instead of a second
/// evaluation.
#[derive(Debug, Clone)]
pub struct LiteLegs {
    /// Bounded fast oscillator leg.
    pub fast: Vec<Option<f64>>,
    /// Bounded slow oscillator leg.
    pub slow: Vec<Option<f64>>,
    /// EIT adaptive leg.
    pub adaptive: Vec<Option<f64>>,
    /// Ribbon signal leg.
    pub signal: Vec<Option<f64>>,
    /// Ribbon mean leg.
    pub mean: Vec<Option<f64>>,
    /// Bounded ribbon health.
    pub ribbon_health: Vec<Option<f64>>,
    /// Bounded MA health.
    pub ma_health: Vec<Option<f64>>,
    /// ATR-14.
    pub atr: Vec<Option<f64>>,
    /// Guarded volume conviction.
    pub vol_ratio: Vec<f64>,
    /// Bar closes.
    pub close: Vec<f64>,
}

/// Squash one raw reading through the frozen affine shape: shift, scale,
/// then the shared sigmoid — the same op order the display catalog pins.
fn squash_raw(value: f64, offset: f64, slope: f64, scale: f64) -> f64 {
    use protopyne_kernels::bars::fast_tanh;
    fast_tanh((value + offset) * slope) * scale
}

fn leg_constants(kind: &str) -> (f64, f64, f64) {
    LEG_CONSTANTS
        .iter()
        .find(|(id, _)| *id == kind)
        .map(|(_, c)| *c)
        .expect("leg kinds validated at construction")
}

/// Run one kernel batch, mapping kernel refusal onto the dial.
fn run_kernel(
    registry: &Arc<IndicatorRegistry>,
    id: &str,
    params: &ParamSet,
    frame: &OwnedOhlcv,
) -> Result<Vec<Option<f64>>, StrategyError> {
    registry
        .get(id)
        .ok_or_else(|| StrategyError::InvalidElement(format!("unknown indicator id {id:?}")))?
        .batch(&frame.views(), params)
        .map(|result| result.values)
        .map_err(|e| StrategyError::InvalidElement(format!("indicator {id:?}: {e}")))
}

/// Relay frame: every OHLC leg carries the series, volume carries the real
/// bar volume so volume-reading smoothers see the tape.
fn relay_frame(series: &[Option<f64>], volumes: &[f64]) -> OwnedOhlcv {
    use protopyne_kernels::indicator::nan_hole;
    let leg: Vec<f64> = series.iter().map(|slot| nan_hole(*slot)).collect();
    let volume: Vec<f64> = series
        .iter()
        .enumerate()
        .map(|(i, _)| volumes.get(i).copied().unwrap_or(f64::NAN))
        .collect();
    OwnedOhlcv {
        open: leg.clone(),
        high: leg.clone(),
        low: leg.clone(),
        close: leg,
        volume,
    }
}

/// Volume frame: the volume series rides the close leg so the SMA kernel —
/// which reads closes — averages volume.
fn volume_frame(volumes: &[f64]) -> OwnedOhlcv {
    OwnedOhlcv {
        open: volumes.to_vec(),
        high: volumes.to_vec(),
        low: volumes.to_vec(),
        close: volumes.to_vec(),
        volume: volumes.to_vec(),
    }
}

/// One arm leg with trailing smoothing and gain: kernel raw over the source
/// frame, optional family smoothing over the relay, the frozen affine
/// squash, then the gain. Length-only smoothing refuses alpha-driven
/// families through the kernel contract, never a default alpha.
fn arm_leg(
    registry: &Arc<IndicatorRegistry>,
    frame: &OwnedOhlcv,
    volumes: &[f64],
    kind: &str,
    length: i64,
    smooth: &str,
    smooth_length: i64,
    gain: f64,
    slot: &str,
) -> Result<Vec<Option<f64>>, StrategyError> {
    let raw = run_kernel(
        registry,
        kind,
        &ParamSet::new().with_int("length", length),
        frame,
    )?;
    let family = SmoothingFamily::parse(smooth).ok_or_else(|| {
        StrategyError::InvalidElement(format!("unknown smoothing family {smooth:?}"))
    })?;
    let smoothed = if family == SmoothingFamily::Na {
        raw
    } else {
        family
            .evaluate(
                &relay_frame(&raw, volumes),
                &SmoothingArgs::length(smooth_length),
            )
            .map_err(|e| StrategyError::InvalidElement(format!("{slot} smoothing: {e}")))?
    };
    let (offset, slope, scale) = leg_constants(kind);
    Ok(smoothed
        .into_iter()
        .map(|slot| {
            slot.and_then(|v| {
                // Gain rides outside the squash, exactly the script order
                // (`normalizeOsc(...) * k`): the affine shape is calibrated,
                // the gain only widens it.
                let shaped = squash_raw(v, offset, slope, scale) * gain;
                shaped.is_finite().then_some(shaped)
            })
        })
        .collect())
}

/// Batch every leg the signal reads over closed bars: kernels only,
/// no decisions. The runner calls this once per bar for exits, trailing,
/// and ATR stops; the signal calls it for entries — one function, one
/// recurrence, two callers.
pub fn market_legs(
    indicator: &LiteIndicatorParams,
    bars: &[protopyne_types::Bar],
) -> Result<LiteLegs, StrategyError> {
    indicator.validate()?;
    let registry = Arc::new(builtin_registry().map_err(|e| kernel_error(&e))?);
    for kind in [&indicator.fast_kernel, &indicator.slow_kernel] {
        if registry.get(kind).is_none() {
            return Err(StrategyError::InvalidElement(format!(
                "unknown indicator id {kind:?}"
            )));
        }
    }
    lite_legs_inner(indicator, &registry, bars)
}

fn lite_legs(
    signal: &InstanceShared,
    bars: &[protopyne_types::Bar],
) -> Result<LiteLegs, StrategyError> {
    lite_legs_inner(&signal.indicator, &signal.registry, bars)
}

/// Shared leg batch over an explicit registry.
fn lite_legs_inner(
    indicator: &LiteIndicatorParams,
    registry: &Arc<IndicatorRegistry>,
    bars: &[protopyne_types::Bar],
) -> Result<LiteLegs, StrategyError> {
    use rust_decimal::prelude::ToPrimitive as _;
    let refuse = |reason: &str| StrategyError::InvalidElement(reason.to_owned());
    let ind = indicator;
    let mut frame = OwnedOhlcv::from_bars(bars);
    let volumes: Vec<f64> = bars.iter().map(|b| b.volume as f64).collect();
    let close: Vec<f64> = bars
        .iter()
        .map(|b| b.c.to_f64().unwrap_or(f64::NAN))
        .collect();
    // Source selection mirrors the terminal: the adaptive EIT relay when
    // armed, the selected price-source column otherwise. Multi-leg arms
    // refuse resynthesized inputs instead of miscomputing.
    let source =
        PriceSource::parse(&ind.price_source).ok_or_else(|| refuse("unknown input source"))?;
    let multi_leg = [ind.fast_kernel.as_str(), ind.slow_kernel.as_str()]
        .iter()
        .any(|kind| matches!(*kind, "mfi" | "stoch"));
    if (source != PriceSource::Close || ind.adaptive) && multi_leg {
        return Err(refuse("multi-leg arms refuse resynthesized inputs"));
    }
    // The single EIT relay, when armed: the oscillator source rides it
    // while every context leg keeps reading raw bars. `raw` is the
    // pre-relay frame the context legs fall back to.
    let raw = frame.clone();
    let mut relay: Option<Vec<Option<f64>>> = None;
    if ind.adaptive {
        let first = run_kernel(
            registry,
            "eit",
            &ParamSet::new().with_float("alpha", ind.eit_alpha),
            &frame,
        )?;
        let leg: Vec<f64> = first
            .iter()
            .map(|slot| protopyne_kernels::indicator::nan_hole(*slot))
            .collect();
        frame.open.clone_from(&leg);
        frame.high.clone_from(&leg);
        frame.low.clone_from(&leg);
        frame.close = leg;
        relay = Some(first);
    } else if source != PriceSource::Close {
        frame.close = source.column_of(&frame);
    }
    // Context legs (ATR, MA health) read raw bars under the relay: the
    // script's chart-timeframe context fetches them from raw bars while
    // only the oscillator source rides the relay. Without the relay the
    // working frame is already the context frame.
    let context = if relay.is_some() { &raw } else { &frame };
    let fast = arm_leg(
        registry,
        &frame,
        &volumes,
        &ind.fast_kernel,
        ind.fast_length,
        &ind.fast_smooth,
        ind.fast_smooth_length,
        ind.fast_gain,
        "fast",
    )?;
    let slow = arm_leg(
        registry,
        &frame,
        &volumes,
        &ind.slow_kernel,
        ind.slow_length,
        &ind.slow_smooth,
        ind.slow_smooth_length,
        ind.slow_gain,
        "slow",
    )?;
    // The reported relay is the single application over raw bars, never
    // the kernel re-run over the relay frame (that would smooth twice).
    let adaptive = match relay {
        Some(first) => first,
        None => run_kernel(
            registry,
            "eit",
            &ParamSet::new().with_float("alpha", ind.eit_alpha),
            &frame,
        )?,
    };
    let signal_leg = SmoothingFamily::parse(&ind.signal_type)
        .ok_or_else(|| refuse("unknown signal smoothing family"))?
        .evaluate(
            &relay_frame(&adaptive, &volumes),
            &SmoothingArgs::length(ind.signal_length).with_n1(ind.signal_param1),
        )
        .map_err(|e| StrategyError::InvalidElement(format!("signal leg: {e}")))?;
    let mean = SmoothingFamily::parse(&ind.mean_type)
        .ok_or_else(|| refuse("unknown mean smoothing family"))?
        .evaluate(
            &relay_frame(&adaptive, &volumes),
            &SmoothingArgs::length(ind.mean_length).with_n1(ind.mean_param1),
        )
        .map_err(|e| StrategyError::InvalidElement(format!("mean leg: {e}")))?;
    let atr = run_kernel(
        registry,
        "atr",
        &ParamSet::new().with_int("length", ATR_LENGTH),
        context,
    )?;
    let vol_sma = run_kernel(
        registry,
        "sma",
        &ParamSet::new().with_int("length", VOL_SMA_LENGTH),
        &volume_frame(&volumes),
    )?;
    let vol_ratio: Vec<f64> = volumes
        .iter()
        .zip(vol_sma.iter())
        .map(|(vol, sma)| match sma {
            Some(sma) if *sma > 0.0 => {
                let ratio = vol / sma;
                if ratio.is_finite() {
                    ratio
                } else {
                    0.0
                }
            }
            _ => 0.0,
        })
        .collect();
    let health_gain = |i: usize| vol_ratio[i] * (ind.eit_alpha + ind.ribbon_health_k);
    let at = |series: &[Option<f64>], i: usize| series.get(i).and_then(|slot| *slot);
    let ribbon_health = (0..bars.len())
        .map(|i| {
            health_mas(
                at(&atr, i),
                at(&adaptive, i),
                at(&signal_leg, i),
                at(&mean, i),
                health_gain(i),
            )
            .map(|score| score * ind.ribbon_health_k)
        })
        .collect();
    let alma = run_kernel(
        registry,
        "alma",
        &ParamSet::new()
            .with_int("length", MA_PRIMARY_LENGTH)
            .with_float("offset", 0.85)
            .with_float("sigma", 6.0),
        context,
    )?;
    let vwma = run_kernel(
        registry,
        "vwma",
        &ParamSet::new().with_int("length", MA_SIGNAL_LENGTH),
        context,
    )?;
    let ema = run_kernel(
        registry,
        "ema",
        &ParamSet::new().with_int("length", MA_MEAN_LENGTH),
        context,
    )?;
    let ma_health = (0..bars.len())
        .map(|i| {
            health_mas(
                at(&atr, i),
                at(&alma, i),
                at(&vwma, i),
                at(&ema, i),
                HEALTH_K,
            )
            .map(|score| score * ind.ma_health_k)
        })
        .collect();
    Ok(LiteLegs {
        fast,
        slow,
        adaptive,
        signal: signal_leg,
        mean,
        ribbon_health,
        ma_health,
        atr,
        vol_ratio,
        close,
    })
}

/// Last two finite values ending at `i`, oldest first; `None` while either
/// end is still warming.
fn pair_at(series: &[Option<f64>], i: usize) -> Option<(f64, f64)> {
    if i < 1 {
        return None;
    }
    match (series.get(i - 1), series.get(i)) {
        (Some(Some(prev)), Some(Some(curr))) if prev.is_finite() && curr.is_finite() => {
            Some((*prev, *curr))
        }
        _ => None,
    }
}

fn crossed_up(series: &[Option<f64>], level: f64, i: usize) -> bool {
    pair_at(series, i).is_some_and(|(prev, curr)| prev <= level && curr > level)
}

fn crossed_down(series: &[Option<f64>], level: f64, i: usize) -> bool {
    pair_at(series, i).is_some_and(|(prev, curr)| prev >= level && curr < level)
}

fn crossed_up_series(fast: &[Option<f64>], slow: &[Option<f64>], i: usize) -> bool {
    match (pair_at(fast, i), pair_at(slow, i)) {
        (Some((fp, fc)), Some((sp, sc))) => fp <= sp && fc > sc,
        _ => false,
    }
}

fn crossed_down_series(fast: &[Option<f64>], slow: &[Option<f64>], i: usize) -> bool {
    match (pair_at(fast, i), pair_at(slow, i)) {
        (Some((fp, fc)), Some((sp, sc))) => fp >= sp && fc < sc,
        _ => false,
    }
}

/// Adaptive threshold mirroring the script: 100 while fast/slow agree in
/// sign, else the slow magnitude floored at 50.
fn adaptive_threshold(fast: f64, slow: f64) -> f64 {
    if (fast >= 0.0) == (slow >= 0.0) {
        100.0
    } else {
        slow.abs().max(50.0)
    }
}

/// Triggers armed by bar `i`: any enabled trigger arms an entry for its
/// side. Returns `(long_armed, short_armed)`.
fn triggers_at(signal: &InstanceShared, legs: &LiteLegs, i: usize) -> (bool, bool) {
    let s = &signal.strategy;
    let (mut long, mut short) = (false, false);
    let arm = |slot: &mut bool| *slot = true;
    if s.sig_momentum {
        let (fast, slow) = (
            legs.fast.get(i).and_then(|v| *v),
            legs.slow.get(i).and_then(|v| *v),
        );
        if let (Some(f), Some(sl)) = (fast, slow) {
            let (up, down) = if s.sig_adaptive_level {
                let t = adaptive_threshold(f, sl);
                (
                    crossed_up(&legs.fast, t, i),
                    crossed_down(&legs.fast, -t, i),
                )
            } else {
                (
                    crossed_up(&legs.fast, s.sig_entry_level, i),
                    crossed_down(&legs.fast, -s.sig_entry_level, i),
                )
            };
            if up {
                arm(&mut long);
            }
            if down {
                arm(&mut short);
            }
        }
    }
    if s.sig_ribbon_flip {
        if crossed_up_series(&legs.adaptive, &legs.signal, i) {
            arm(&mut long);
        }
        if crossed_down_series(&legs.adaptive, &legs.signal, i) {
            arm(&mut short);
        }
    }
    if s.sig_osc_cross {
        if crossed_up_series(&legs.fast, &legs.slow, i) {
            arm(&mut long);
        }
        if crossed_down_series(&legs.fast, &legs.slow, i) {
            arm(&mut short);
        }
    }
    if s.sig_exhaust_entry {
        if crossed_up(&legs.fast, -s.sig_exhaust_level, i) {
            arm(&mut long);
        }
        if crossed_down(&legs.fast, s.sig_exhaust_level, i) {
            arm(&mut short);
        }
    }
    if s.sig_dual {
        // Counter-trend exhaustion confirmed by both oscillators. Slow
        // crosses through its level while fast sits beyond its own — same
        // bar when the lookback is zero, any bar in the window otherwise.
        let lookback = s.sig_dual_lookback.max(0) as usize;
        let fast_beyond = |level: f64, below: bool| {
            (0..=lookback.min(i)).any(|back| {
                legs.fast[i - back].is_some_and(|v| if below { v < level } else { v > level })
            })
        };
        if crossed_down(&legs.slow, s.sig_dual_slow, i) && fast_beyond(s.sig_dual_fast, false) {
            arm(&mut short);
        }
        if crossed_up(&legs.slow, -s.sig_dual_slow, i) && fast_beyond(-s.sig_dual_fast, true) {
            arm(&mut long);
        }
    }
    (long, short)
}

/// Every enabled filter must pass for the armed side. The regime gate reads
/// the hysteresis-stable label (held in the instance across commits); the
/// frozen probe never participates.
fn filters_pass(
    signal: &InstanceShared,
    legs: &LiteLegs,
    bars: &[protopyne_types::Bar],
    i: usize,
    long: bool,
    prev_regime: &mut MarketRegime,
) -> bool {
    let s = &signal.strategy;
    let at = |series: &[Option<f64>]| series.get(i).and_then(|v| *v);
    let side = |v: f64| if long { v > 0.0 } else { v < 0.0 };
    if s.filt_ribbon {
        let (a, g) = (at(&legs.adaptive), at(&legs.signal));
        match (a, g) {
            (Some(a), Some(g)) if (long && a > g) || (!long && a < g) => {}
            _ => return false,
        }
    }
    if s.filt_ma_health
        && !at(&legs.ma_health).is_some_and(|v| side(v) && v.abs() >= s.filt_ma_health_min)
    {
        return false;
    }
    if s.filt_ribbon_health && !at(&legs.ribbon_health).is_some_and(side) {
        return false;
    }
    // Slow-side filter with a possibly negative floor: long needs the slow
    // leg above +min, short needs it below -min, exactly the script rule.
    if s.filt_slow {
        let ok = if long {
            at(&legs.slow).is_some_and(|v| v > s.filt_slow_min)
        } else {
            at(&legs.slow).is_some_and(|v| v < -s.filt_slow_min)
        };
        if !ok {
            return false;
        }
    }
    if s.filt_delta {
        let len = (s.filt_delta_len.max(1) as usize).min(i + 1);
        let mut sum = 0.0;
        let mut count = 0_usize;
        for back in 0..len {
            // Shape-estimated delta over the closed tape: buy share minus
            // sell share per bar, aligned with the volume window.
            let b = &bars[i - back];
            use rust_decimal::prelude::ToPrimitive as _;
            let est = estimate_delta_volume_estimated(
                b.o.to_f64().unwrap_or(f64::NAN),
                b.h.to_f64().unwrap_or(f64::NAN),
                b.l.to_f64().unwrap_or(f64::NAN),
                b.c.to_f64().unwrap_or(f64::NAN),
                b.volume as f64,
            );
            let sell = b.volume as f64 - est.buy_volume;
            if est.buy_volume.is_finite() && sell.is_finite() {
                sum += est.buy_volume - sell;
                count += 1;
            }
        }
        if count == 0 {
            return false;
        }
        let mean = sum / count as f64;
        if long && mean <= 0.0 {
            return false;
        }
        if !long && mean >= 0.0 {
            return false;
        }
    }
    // `vol_ratio` is guarded finite at construction, so `<` is exact here.
    if s.filt_volume && legs.vol_ratio[i] < s.filt_volume_min {
        return false;
    }
    if s.filt_extension {
        match (at(&legs.mean), at(&legs.atr)) {
            (Some(mean), Some(atr)) if atr > 0.0 => {
                if (legs.close[i] - mean).abs() > s.filt_extension_max * atr {
                    return false;
                }
            }
            _ => return false,
        }
    }
    if s.filt_regime_lowvol || s.filt_regime_volatile {
        let regime = detect_regime_stable(
            *prev_regime,
            at(&legs.ma_health),
            None,
            Some(legs.vol_ratio[i]),
        );
        *prev_regime = regime;
        if s.filt_regime_lowvol && regime == MarketRegime::LowVolume {
            return false;
        }
        if s.filt_regime_volatile && regime == MarketRegime::Volatile {
            return false;
        }
    }
    true
}

#[cfg(test)]
#[path = "../tests/integration_lite_signal.rs"]
mod tests;
