//! One market-leg batch, one oscillator catalog.
//!
//! The decision path and the chart pane read the same market context: one
//! oscillator pair, an EIT relay, a ribbon pair, and the health/volatility
//! context behind them. Computing that context twice — once per consumer —
//! was both a drift hazard and the source of the quadratic backtest cost, so
//! the batch lives here and every consumer indexes it.
//!
//! The catalog beside it is the one home for the frozen affine triple
//! `(offset, k, multiplier)` that maps a raw oscillator reading onto its
//! calibrated band. Three copies existed (a display enum, a panel defaults
//! row, and a transcribed table in the strategy crate); a Pine edit that
//! moved an arm would have been fixed in one or two of them.
//!
//! The batch is causal: every kernel here only reads the current and past
//! bars, so a batch over a whole tape equals the batch over each prefix.
//! That equality is what licenses computing once per run instead of once per
//! bar, and it is pinned by a property test beside the recorded baseline.

use fluent_wvr::{Describable, FieldAccess};
use protopyne_types::session::TradingSession;
use protopyne_types::Bar;

use crate::bars::fast_tanh;
use crate::error::KernelError;
use crate::indicator::{nan_hole, shared_registry, IndicatorRegistry, OwnedOhlcv, ParamSet};
use crate::market_context::{health_mas, HEALTH_K};
use crate::price_source::PriceSource;
use crate::smoothing::{SmoothingArgs, SmoothingFamily, MA_PERIOD_MAX, MA_PERIOD_MIN};

/// ATR span behind health distances and the extension filter.
pub const ATR_LENGTH: i64 = 14;
/// Long ATR span behind the regime's squeeze surface.
pub const ATR_LONG_LENGTH: i64 = 50;
/// Close-dispersion span behind the regime's squeeze surface.
pub const STDEV_LENGTH: i64 = 14;
/// Volume SMA span behind conviction.
pub const VOL_SMA_LENGTH: i64 = 20;
/// ALMA span behind the MA-health fold.
pub const MA_PRIMARY_LENGTH: i64 = 8;
/// VWMA span behind the MA-health fold.
pub const MA_SIGNAL_LENGTH: i64 = 12;
/// EMA span behind the MA-health fold.
pub const MA_MEAN_LENGTH: i64 = 21;
/// Trailing window behind the lite realised-volatility estimator
/// (`protopyne-strategy signals_lite::realised_volatility`, manual
/// mean/var over closes). Intentionally separate from the kernel
/// estimator's `KERNEL_VOLATILITY_WINDOW` (strategy `signals_kernel`,
/// returns + `stddev` kernel): different math over a same-sized window —
/// the names now say which is which (M4.2). Do not unify the constants.
pub const LITE_VOLATILITY_LENGTH: usize = 20;
/// Default ribbon-health gain (the reference dial's shipped value).
pub const RIBBON_HEALTH_K: f64 = 1.8;
/// Default MA-health display gain (the reference dial's shipped value).
pub const MA_HEALTH_K: f64 = 1.2;

/// One row of the frozen oscillator catalog.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LegRow {
    /// Canonical lowercase id (settings/schema spelling).
    pub kind: &'static str,
    /// The kernel that produces the raw reading. `None` for a pass-through
    /// source (`raw`) and for a refused row (`cdv`).
    pub kernel: Option<&'static str>,
    /// Frozen affine triple: shift, steepness, band half-width.
    pub constants: (f64, f64, f64),
    /// Why the row cannot be evaluated, when it cannot. A refused row keeps
    /// its constants so a consumer can still present the catalog entry.
    pub refusal: Option<&'static str>,
}

/// The frozen oscillator catalog, in Pine dispatch order.
///
/// `raw` is the pass-through source: no kernel, and the neutral squash a
/// Pine `Smoothings.NA`/unlisted type reads. `cdv` is a listed Pine
/// oscillator with no kernel here — it parses but refuses evaluation with a
/// reason, never substituting the neutral constants.
pub const LEG_CATALOG: &[LegRow] = &[
    LegRow {
        kind: "aroon",
        kernel: Some("aroon"),
        constants: (-50.0, 0.005, 800.0),
        refusal: None,
    },
    LegRow {
        kind: "cci",
        kernel: Some("cci"),
        constants: (-50.0, 0.0035, 500.0),
        refusal: None,
    },
    LegRow {
        kind: "cdv",
        kernel: None,
        constants: (0.0, 1.0, 100.0),
        refusal: Some("cumulative delta volume has no kernel in this build"),
    },
    LegRow {
        kind: "cmo",
        kernel: Some("cmo"),
        constants: (0.0, 0.002, 1000.0),
        refusal: None,
    },
    LegRow {
        kind: "mfi",
        kernel: Some("mfi"),
        constants: (-50.0, 0.0084, 740.0),
        refusal: None,
    },
    LegRow {
        kind: "rsi",
        kernel: Some("rsi"),
        constants: (-50.0, 0.0084, 740.0),
        refusal: None,
    },
    LegRow {
        kind: "srsi",
        kernel: Some("srsi"),
        constants: (-50.0, 0.0006, 7000.0),
        refusal: None,
    },
    LegRow {
        kind: "stoch",
        kernel: Some("stoch"),
        constants: (-50.0, 0.0005, 7000.0),
        refusal: None,
    },
    LegRow {
        kind: "trendsi",
        kernel: Some("trendsi"),
        constants: (0.0, 0.016, 9000.0),
        refusal: None,
    },
    LegRow {
        kind: "voss",
        kernel: Some("voss"),
        constants: (0.0, 1.5, 300.0),
        refusal: None,
    },
    LegRow {
        kind: "raw",
        kernel: None,
        constants: (-50.0, 0.0084, 740.0),
        refusal: None,
    },
];

/// Every oscillator id the catalog names, in Pine dispatch order.
pub const LITE_LEG_KINDS: &[&str] = &[
    "aroon", "cci", "cdv", "cmo", "mfi", "rsi", "srsi", "stoch", "trendsi", "voss", "raw",
];

/// The arms the decision path accepts. The catalog is data (what exists);
/// this list is policy (what a strategy dial may select). Widening it is a
/// decision-path change, so it does not ride along with catalog work.
pub const SIGNAL_LEG_KINDS: &[&str] = &["rsi", "cci", "cmo", "mfi", "stoch"];

/// One catalog row by id.
#[must_use]
pub fn leg_row(kind: &str) -> Option<&'static LegRow> {
    LEG_CATALOG.iter().find(|row| row.kind == kind)
}

/// The frozen affine triple for `kind`, or `None` for an unlisted id or the
/// refused `cdv` row.
#[must_use]
pub fn leg_constants(kind: &str) -> Option<(f64, f64, f64)> {
    let row = leg_row(kind)?;
    if row.refusal.is_some() {
        return None;
    }
    Some(row.constants)
}

/// The kernel id behind `kind`, when the row has one.
#[must_use]
pub fn leg_kernel(kind: &str) -> Option<&'static str> {
    leg_row(kind).and_then(|row| row.kernel)
}

/// Why `kind` cannot be evaluated, when it cannot.
#[must_use]
pub fn leg_refusal(kind: &str) -> Option<&'static str> {
    leg_row(kind).and_then(|row| row.refusal)
}

/// Indicator dials the market-leg batch reads: the same field names and
/// defaults the strategy and terminal have always carried.
///
/// Strategy triggers and filters live in the strategy configuration; this
/// type carries only the market legs.
#[derive(
    Debug,
    Clone,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    bon::Builder,
    FieldAccess,
    Describable,
)]
#[builder(start_fn = new)]
pub struct LiteParams {
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
    /// Ribbon-health gain, which steepens the health fold itself (the
    /// reference passes it inside the scorer's `k`). The reference's *outer*
    /// display multiplier is not applied here: the decision path reads the
    /// raw score, because a positive gain never changes its sign and a
    /// gain-scaled magnitude must never masquerade as a stricter filter.
    pub ribbon_health_k: f64,
    /// OHLC column the arms read when the adaptive relay is off.
    pub price_source: String,
    /// Read the adaptive EIT relay instead of the price source.
    pub adaptive: bool,
    /// Session filter switch (read by the signal, not the leg batch).
    pub session_filter: bool,
    /// Session window in `HHMM-HHMM` form.
    pub session: String,
}

impl Default for LiteParams {
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
            ribbon_health_k: RIBBON_HEALTH_K,
            price_source: "close".to_owned(),
            adaptive: false,
            session_filter: true,
            session: "0930-1600".to_owned(),
        }
    }
}

impl LiteParams {
    /// Fail-closed self-consistency: every selected arm is a decision-path
    /// arm, every window sits inside the shared MA bounds, every smoothing
    /// family is known, gains are finite and positive, the source parses,
    /// and an armed session parses.
    pub fn validate(&self) -> Result<(), KernelError> {
        let bad =
            |slot: &str, reason: String| KernelError::InvalidParam(format!("{slot}: {reason}"));
        for (slot, kind) in [
            ("fast_kernel", &self.fast_kernel),
            ("slow_kernel", &self.slow_kernel),
        ] {
            if !SIGNAL_LEG_KINDS.contains(&kind.as_str()) {
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
        if !self.ribbon_health_k.is_finite() || self.ribbon_health_k <= 0.0 {
            return Err(bad(
                "ribbon_health_k",
                format!("gain {} must be positive and finite", self.ribbon_health_k),
            ));
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

/// One arm's raw and shaped readings: the kernel output before the frozen
/// squash, and the squashed/gained band value. The display needs both (it
/// renders the unsquashed reading and applies its own warmup mask); the
/// decision path reads only [`shaped`](Self::shaped).
#[derive(Debug, Clone)]
pub struct ArmSeries {
    /// Kernel raw reading, holes preserved.
    pub raw: Vec<Option<f64>>,
    /// Squashed and gained calibrated reading, holes preserved.
    pub shaped: Vec<Option<f64>>,
}

/// Squash one raw reading through the frozen affine shape: shift, scale,
/// then the shared sigmoid — the same op order the display catalog pins.
#[must_use]
pub fn squash_raw(value: f64, offset: f64, slope: f64, scale: f64) -> f64 {
    fast_tanh((value + offset) * slope) * scale
}

/// Every evaluated leg over one closed tape, index-aligned with its bars.
///
/// The decision path indexes this once per run; the display maps it into its
/// own pane shapes. `atr_risk` is the stop/trailing span, `atr_long` and
/// `stdev` are the regime's squeeze surface — dropping any of the three
/// silently changes a decision.
#[derive(Debug, Clone)]
pub struct LiteLegs {
    /// Bounded fast oscillator leg.
    pub fast: Vec<Option<f64>>,
    /// Bounded slow oscillator leg.
    pub slow: Vec<Option<f64>>,
    /// The fast arm's unsquashed reading (display parity).
    pub fast_raw: Vec<Option<f64>>,
    /// The slow arm's unsquashed reading (display parity).
    pub slow_raw: Vec<Option<f64>>,
    /// EIT adaptive leg.
    pub adaptive: Vec<Option<f64>>,
    /// Ribbon signal leg.
    pub signal: Vec<Option<f64>>,
    /// Ribbon mean leg.
    pub mean: Vec<Option<f64>>,
    /// Raw ribbon health.
    pub ribbon_health: Vec<Option<f64>>,
    /// Raw MA health.
    pub ma_health: Vec<Option<f64>>,
    /// ATR-14: the context span behind health distances and the extension
    /// filter.
    pub atr: Vec<Option<f64>>,
    /// ATR at the *risk* span: the stop and trailing construction's own ATR,
    /// which the reference reads separately from the context span.
    pub atr_risk: Vec<Option<f64>>,
    /// ATR-50: the long span the regime's squeeze surface compares against.
    pub atr_long: Vec<Option<f64>>,
    /// Close dispersion (STDEV-14) for the regime's squeeze surface.
    pub stdev: Vec<Option<f64>>,
    /// Guarded volume conviction.
    pub vol_ratio: Vec<f64>,
    /// Volume SMA behind the conviction ratio.
    pub vol_sma: Vec<Option<f64>>,
    /// Bar closes.
    pub close: Vec<f64>,
}

/// One index of [`LiteLegs`], borrowed: every series read at the same slot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LegAt<'a> {
    /// The index this view was taken at.
    pub index: usize,
    /// Fast oscillator leg.
    pub fast: Option<f64>,
    /// Slow oscillator leg.
    pub slow: Option<f64>,
    /// Fast arm raw reading.
    pub fast_raw: Option<f64>,
    /// Slow arm raw reading.
    pub slow_raw: Option<f64>,
    /// EIT adaptive leg.
    pub adaptive: Option<f64>,
    /// Ribbon signal leg.
    pub signal: Option<f64>,
    /// Ribbon mean leg.
    pub mean: Option<f64>,
    /// Raw ribbon health.
    pub ribbon_health: Option<f64>,
    /// Raw MA health.
    pub ma_health: Option<f64>,
    /// Context ATR.
    pub atr: Option<f64>,
    /// Risk-span ATR.
    pub atr_risk: Option<f64>,
    /// Long ATR.
    pub atr_long: Option<f64>,
    /// Close dispersion.
    pub stdev: Option<f64>,
    /// Guarded volume conviction.
    pub vol_ratio: Option<f64>,
    /// Volume SMA.
    pub vol_sma: Option<f64>,
    /// Bar close.
    pub close: Option<f64>,
    /// Borrowed marker so the view keeps the legs alive.
    marker: std::marker::PhantomData<&'a LiteLegs>,
}

impl LiteLegs {
    /// The per-index view. Every series is read at `index`; a shorter series
    /// reads as a hole rather than panicking.
    #[must_use]
    pub fn at(&self, index: usize) -> LegAt<'_> {
        let slot = |series: &[Option<f64>]| series.get(index).copied().flatten();
        LegAt {
            index,
            fast: slot(&self.fast),
            slow: slot(&self.slow),
            fast_raw: slot(&self.fast_raw),
            slow_raw: slot(&self.slow_raw),
            adaptive: slot(&self.adaptive),
            signal: slot(&self.signal),
            mean: slot(&self.mean),
            ribbon_health: slot(&self.ribbon_health),
            ma_health: slot(&self.ma_health),
            atr: slot(&self.atr),
            atr_risk: slot(&self.atr_risk),
            atr_long: slot(&self.atr_long),
            stdev: slot(&self.stdev),
            vol_ratio: self.vol_ratio.get(index).copied(),
            vol_sma: self.vol_sma.get(index).copied().flatten(),
            close: self.close.get(index).copied(),
            marker: std::marker::PhantomData,
        }
    }

    /// Number of aligned bars.
    #[must_use]
    pub fn len(&self) -> usize {
        self.close.len()
    }

    /// Whether the batch carries no bars.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.close.is_empty()
    }
}

/// Batch every leg over closed bars: kernels only, no decisions.
pub fn lite_legs(
    bars: &[Bar],
    params: &LiteParams,
    risk_atr_len: i64,
) -> Result<LiteLegs, KernelError> {
    let frame = OwnedOhlcv::from_bars(bars);
    let closes: Vec<f64> = bars
        .iter()
        .map(|bar| protopyne_types::bar_leg_as_f64(bar.c))
        .collect();
    lite_legs_from_frame(&frame, &closes, params, risk_atr_len)
}

/// Batch every leg over an explicit OHLCV frame.
///
/// The chart pane already holds a frame (it never materializes `Bar`s), so
/// this is the one batch both consumers call; [`lite_legs`] is the bar-shaped
/// entry.
pub fn lite_legs_from_frame(
    frame: &OwnedOhlcv,
    closes: &[f64],
    params: &LiteParams,
    risk_atr_len: i64,
) -> Result<LiteLegs, KernelError> {
    params.validate()?;
    if risk_atr_len < 1 {
        return Err(KernelError::InvalidParam(format!(
            "risk ATR length {risk_atr_len} must be positive"
        )));
    }
    let registry = shared_registry()?;
    batch_legs(frame, closes, params, registry, risk_atr_len)
}

/// The context legs the batch and the chart pane both read.
#[derive(Debug, Clone)]
pub struct ContextLegs {
    /// Context ATR (14).
    pub atr: Vec<Option<f64>>,
    /// Long ATR (50).
    pub atr_long: Vec<Option<f64>>,
    /// Close dispersion (STDEV-14).
    pub stdev: Vec<Option<f64>>,
    /// Volume SMA.
    pub vol_sma: Vec<Option<f64>>,
    /// Guarded volume conviction.
    pub vol_ratio: Vec<f64>,
}

/// Batch the context legs over one frame: ATR at both spans, the squeeze
/// dispersion, and the guarded volume conviction.
pub fn context_legs(frame: &OwnedOhlcv) -> Result<ContextLegs, KernelError> {
    let registry = shared_registry()?;
    context_legs_with(registry, frame)
}

fn context_legs_with(
    registry: &IndicatorRegistry,
    frame: &OwnedOhlcv,
) -> Result<ContextLegs, KernelError> {
    let atr = run_kernel(
        registry,
        "atr",
        &ParamSet::new().with_int("length", ATR_LENGTH),
        frame,
    )?;
    let atr_long = run_kernel(
        registry,
        "atr",
        &ParamSet::new().with_int("length", ATR_LONG_LENGTH),
        frame,
    )?;
    let stdev = run_kernel(
        registry,
        "stddev",
        &ParamSet::new()
            .with_int("length", STDEV_LENGTH)
            .with_float("nbdev", 1.0),
        frame,
    )?;
    let volumes = frame.volume.clone();
    let vol_sma = run_kernel(
        registry,
        "sma",
        &ParamSet::new().with_int("length", VOL_SMA_LENGTH),
        &volume_frame(&volumes),
    )?;
    let vol_ratio = volume_conviction(&volumes, &vol_sma);
    Ok(ContextLegs {
        atr,
        atr_long,
        stdev,
        vol_sma,
        vol_ratio,
    })
}

/// The MA-health fold over the shared context bundle, computing its own
/// context ATR.
pub fn ma_health_series(frame: &OwnedOhlcv) -> Result<Vec<Option<f64>>, KernelError> {
    let registry = shared_registry()?;
    let atr = run_kernel(
        registry,
        "atr",
        &ParamSet::new().with_int("length", ATR_LENGTH),
        frame,
    )?;
    ma_health_with(registry, frame, &atr)
}

fn batch_legs(
    input: &OwnedOhlcv,
    closes: &[f64],
    params: &LiteParams,
    registry: &IndicatorRegistry,
    risk_atr_len: i64,
) -> Result<LiteLegs, KernelError> {
    let refuse = |reason: &str| KernelError::InvalidParam(reason.to_owned());
    let ind = params;
    let mut frame = input.clone();
    let volumes: Vec<f64> = input.volume.clone();
    let close: Vec<f64> = closes.to_vec();
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
        let leg: Vec<f64> = first.iter().map(|slot| nan_hole(*slot)).collect();
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
    let fast_arm = arm_leg(
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
    let slow_arm = arm_leg(
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
        .map_err(|e| KernelError::InvalidParam(format!("signal leg: {e}")))?;
    let mean = SmoothingFamily::parse(&ind.mean_type)
        .ok_or_else(|| refuse("unknown mean smoothing family"))?
        .evaluate(
            &relay_frame(&adaptive, &volumes),
            &SmoothingArgs::length(ind.mean_length).with_n1(ind.mean_param1),
        )
        .map_err(|e| KernelError::InvalidParam(format!("mean leg: {e}")))?;
    let context_legs = context_legs_with(registry, context)?;
    // The risk span is a separate kernel request: the reference reads its own
    // ATR for stops and trailing, and it is the operator's dial. At the default
    // length the two series are identical.
    let atr_risk = if risk_atr_len == ATR_LENGTH {
        context_legs.atr.clone()
    } else {
        run_kernel(
            registry,
            "atr",
            &ParamSet::new().with_int("length", risk_atr_len),
            context,
        )?
    };
    let ribbon = ribbon_health(
        &context_legs.atr,
        &adaptive,
        &signal_leg,
        &mean,
        &context_legs.vol_ratio,
        ind.eit_alpha,
        ind.ribbon_health_k,
    );
    let ma_health = ma_health_with(registry, context, &context_legs.atr)?;
    Ok(LiteLegs {
        fast: fast_arm.shaped,
        slow: slow_arm.shaped,
        fast_raw: fast_arm.raw,
        slow_raw: slow_arm.raw,
        adaptive,
        signal: signal_leg,
        mean,
        ribbon_health: ribbon,
        ma_health,
        atr: context_legs.atr,
        atr_risk,
        atr_long: context_legs.atr_long,
        stdev: context_legs.stdev,
        vol_ratio: context_legs.vol_ratio,
        vol_sma: context_legs.vol_sma,
        close,
    })
}

/// Guarded volume conviction: the ratio of volume to its SMA, `0` wherever a
/// partial or non-positive average would make the ratio unreadable.
#[must_use]
pub fn volume_conviction(volumes: &[f64], vol_sma: &[Option<f64>]) -> Vec<f64> {
    volumes
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
        .collect()
}

/// The ribbon-health fold: ATR, the adaptive relay, the ribbon pair, and the
/// per-bar gain. Raw (no outer display multiplier) by design.
#[must_use]
pub fn ribbon_health(
    atr: &[Option<f64>],
    adaptive: &[Option<f64>],
    signal: &[Option<f64>],
    mean: &[Option<f64>],
    vol_ratio: &[f64],
    eit_alpha: f64,
    ribbon_health_k: f64,
) -> Vec<Option<f64>> {
    let at = |series: &[Option<f64>], i: usize| series.get(i).and_then(|slot| *slot);
    (0..atr.len())
        .map(|i| {
            let gain = vol_ratio.get(i).copied().unwrap_or(0.0) * (eit_alpha + ribbon_health_k);
            health_mas(
                at(atr, i),
                at(adaptive, i),
                at(signal, i),
                at(mean, i),
                gain,
            )
        })
        .collect()
}

/// The MA-health fold over the shared context bundle (ALMA-8, VWMA-12,
/// EMA-21) read against the supplied ATR. Raw; display gains apply outside.
pub fn ma_health(frame: &OwnedOhlcv, atr: &[Option<f64>]) -> Result<Vec<Option<f64>>, KernelError> {
    let registry = shared_registry()?;
    ma_health_with(registry, frame, atr)
}

fn ma_health_with(
    registry: &IndicatorRegistry,
    frame: &OwnedOhlcv,
    atr: &[Option<f64>],
) -> Result<Vec<Option<f64>>, KernelError> {
    let alma = run_kernel(
        registry,
        "alma",
        &ParamSet::new()
            .with_int("length", MA_PRIMARY_LENGTH)
            .with_float("offset", 0.85)
            .with_float("sigma", 6.0),
        frame,
    )?;
    let vwma = run_kernel(
        registry,
        "vwma",
        &ParamSet::new().with_int("length", MA_SIGNAL_LENGTH),
        frame,
    )?;
    let ema = run_kernel(
        registry,
        "ema",
        &ParamSet::new().with_int("length", MA_MEAN_LENGTH),
        frame,
    )?;
    let at = |series: &[Option<f64>], i: usize| series.get(i).and_then(|slot| *slot);
    Ok((0..frame.close.len())
        .map(|i| {
            health_mas(
                at(atr, i),
                at(&alma, i),
                at(&vwma, i),
                at(&ema, i),
                HEALTH_K,
            )
        })
        .collect())
}

/// One arm: kernel raw over the source frame, optional family smoothing over
/// the relay, then the frozen affine squash and the gain.
pub fn arm_leg(
    registry: &IndicatorRegistry,
    frame: &OwnedOhlcv,
    volumes: &[f64],
    kind: &str,
    length: i64,
    smooth: &str,
    smooth_length: i64,
    gain: f64,
    slot: &str,
) -> Result<ArmSeries, KernelError> {
    let kernel = leg_kernel(kind).ok_or_else(|| {
        let reason = leg_refusal(kind).map_or_else(
            || format!("{kind:?} is not a kernel-backed leg"),
            str::to_owned,
        );
        KernelError::InvalidParam(format!("{slot}: {reason}"))
    })?;
    let raw = run_kernel(
        registry,
        kernel,
        &ParamSet::new().with_int("length", length),
        frame,
    )?;
    let family = SmoothingFamily::parse(smooth)
        .ok_or_else(|| KernelError::InvalidParam(format!("unknown smoothing family {smooth:?}")))?;
    let smoothed = if family == SmoothingFamily::Na {
        raw.clone()
    } else {
        family
            .evaluate(
                &relay_frame(&raw, volumes),
                &SmoothingArgs::length(smooth_length),
            )
            .map_err(|e| KernelError::InvalidParam(format!("{slot} smoothing: {e}")))?
    };
    let (offset, slope, scale) = leg_constants(kind).expect("kernel-backed rows carry constants");
    let shaped = smoothed
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
        .collect();
    Ok(ArmSeries { raw, shaped })
}

/// Run one kernel batch, mapping kernel refusal onto the leg.
fn run_kernel(
    registry: &IndicatorRegistry,
    id: &str,
    params: &ParamSet,
    frame: &OwnedOhlcv,
) -> Result<Vec<Option<f64>>, KernelError> {
    registry
        .get(id)
        .ok_or_else(|| KernelError::UnknownIndicator(id.to_owned()))?
        .batch(&frame.views(), params)
        .map(|result| result.values)
        .map_err(|e| KernelError::InvalidParam(format!("indicator {id:?}: {e}")))
}

/// Relay frame: every OHLC leg carries the series, volume carries the real
/// bar volume so volume-reading smoothers see the tape.
fn relay_frame(series: &[Option<f64>], volumes: &[f64]) -> OwnedOhlcv {
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

#[cfg(test)]
#[path = "../tests/lite.rs"]
mod tests;
