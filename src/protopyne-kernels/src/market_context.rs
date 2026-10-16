//! Native `MarketContext`: the `libApex` context as a pure view over the
//! shared multi-timeframe store.
//!
//! `fetch` in `_libApex.pine` reads nine kernel series plus the OHLCV legs
//! and folds them into one per-timeframe context. This module ports that
//! fold 1:1 — same fields, same op order, same frozen thresholds — while
//! the store owns resampling and kernel evaluation, so no kernel math and
//! no second health scorer live here. Display-only helpers
//! (`getColorMomentumAlpha`, `ManagedTable :: update`, zone drawing) stay
//! out: they never enter the hot path. The `candles` pattern slot is absent
//! for the same reason — `fetch` never writes it; pattern state lives in
//! `candles.rs` predicates.
//!
//! Two entry points share one pure core: [`update`] reads a context at one
//! frame bar (replay-friendly explicit index), [`fetch_all`] ensures the
//! union of series for several spans through one store revision and reads
//! each span at its latest confirmed bar.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::BuildHasher;

use crate::averages::{AlmaIndicator, VwmaIndicator};
use crate::bars::fast_tanh;
use crate::ema::EmaIndicator;
use crate::error::KernelError;
use crate::indicator::{IndicatorRegistry, ParamSet, fin_or_none};
use crate::math::div_or_none;
use crate::mtf::{Lookahead, MtfStore, SeriesKey};
use crate::oscillators::{CciIndicator, RsiIndicator};
use crate::sma::SmaIndicator;
use crate::ta_stat::StddevIndicator;
use crate::volatility::AtrIndicator;
use protopyne_types::Timeframe;

/// `getHealthMAs` scaling factor, as passed by `fetch` (task-value:
/// sensitivity of the alignment score; frozen, never tuned to fit).
pub const HEALTH_K: f64 = 2.0;
/// `detectRegime` trend floor: `trend_score > 20` or a trend tie trends
/// (task-value: actionability of the label; frozen).
pub const TREND_SCORE_FLOOR: f64 = 20.0;
/// `detectRegime` volatility floor with a volatility tie (task-value).
pub const VOLATILITY_SCORE_FLOOR: f64 = 20.0;
/// `detectRegime` volume floor with a volume tie (task-value).
pub const VOLUME_SCORE_FLOOR: f64 = 25.0;
/// `trend_direction` deadband: beyond ±15 reads ±1 (task-value).
pub const DIRECTION_BAND: f64 = 15.0;
/// Volatility score scale: `8 * volatility_ratio` (task-value).
pub const VOLATILITY_SCALE: f64 = 8.0;
/// Volume score scale: `10 * vol_ratio` (task-value).
pub const VOLUME_SCALE: f64 = 10.0;
/// CCI normalization to the zero-centered band (task-value).
pub const CCI_SCALE: f64 = 3.0;
/// RSI zero-center (task-value).
pub const RSI_CENTER: f64 = 50.0;
/// RSI normalization to the zero-centered band (task-value).
pub const RSI_SCALE: f64 = 2.0;
/// Trend-strength normalization to 0–1 (task-value).
pub const TREND_NORM: f64 = 100.0;
/// `getHealthMAs` aligned-bull base (`OSC_UPPER_RANGE`, task-value).
pub const HEALTH_UPPER_BASE: f64 = 10.0;
/// `getHealthMAs` aligned-bear base (`OSC_LOWER_THRESHOLD`, task-value).
pub const HEALTH_LOWER_BASE: f64 = -50.0;
/// `getHealthMAs` aligned amplitude (task-value).
pub const HEALTH_AMPLITUDE: f64 = 120.0;
/// `getHealthMAs` mixed mid weight (task-value).
pub const HEALTH_MID_WEIGHT: f64 = 0.7;
/// `getHealthMAs` mixed deadband and span (task-value).
pub const HEALTH_DEADBAND: f64 = 0.01;
/// `getHealthMAs` mixed span (task-value).
pub const HEALTH_MIXED_SPAN: f64 = 20.0;
/// `getHealthMAs` mixed base (task-value).
pub const HEALTH_MIXED_BASE: f64 = -10.0;
/// Stability band for cross-surface regime switches (task-value: label
/// stability for consumers that display or gate on the label; frozen). A
/// quarter of the smallest win floor: genuine rotations (surfaces separate
/// by tens on real tapes) switch promptly, chop inside the band holds. The
/// parity probe keeps exact-`==` semantics on [`detect_regime`]; this
/// constant serves the stability wrapper only, and the two are never
/// confused.
pub const REGIME_HYSTERESIS: f64 = 5.0;

/// Market regime labels, mirroring `UTIL.MarketRegimes` member for member.
/// `Display` renders the Pine member name so audits compare directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MarketRegime {
    /// No dominant surface (also the fresh-context default).
    #[default]
    Ranging,
    /// Trend surface wins, health positive.
    TrendingBull,
    /// Trend surface wins, health non-positive.
    TrendingBear,
    /// Dispersion surface wins.
    Volatile,
    /// Nothing wins (also the all-absent fail-closed label).
    LowVolume,
}

impl fmt::Display for MarketRegime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ranging => f.write_str("RANGING"),
            Self::TrendingBull => f.write_str("TRENDING_BULL"),
            Self::TrendingBear => f.write_str("TRENDING_BEAR"),
            Self::Volatile => f.write_str("VOLATILE"),
            Self::LowVolume => f.write_str("LOW_VOLUME"),
        }
    }
}

/// Native `MarketContext`: every `fetch`-written field of the Pine type in
/// declaration order. Floats read `Option<f64>` (`None` is Pine `na` via
/// the locked `fin_or_none` policy); `tf` resolves the fixed-ms span and
/// `regime` the frozen label. No `Decimal` anywhere on this path — money
/// exactness lives at the order boundary, indicator math is `f64`.
#[derive(Debug, Clone, PartialEq)]
pub struct MarketContext {
    /// Resolved span (Pine reports chart-relative `C`; native always names
    /// the fixed-ms span — representation differs, values never do).
    pub tf: Timeframe,
    /// Fast average (ALMA-8).
    pub ma_primary: Option<f64>,
    /// Signal average (VWMA-12).
    pub ma_signal: Option<f64>,
    /// Baseline average (EMA-21).
    pub ma_mean: Option<f64>,
    /// Average true range (14).
    pub atr: Option<f64>,
    /// Long average true range (50).
    pub atr_long: Option<f64>,
    /// Normalized CCI (raw × 3).
    pub cci: Option<f64>,
    /// Normalized RSI ((raw − 50) × 2).
    pub rsi: Option<f64>,
    /// Close dispersion (14).
    pub stdev: Option<f64>,
    /// Current volume (`nz` shape: prior carries past absent prints).
    pub vol: Option<f64>,
    /// Volume SMA (20, over the volume leg).
    pub vol_sma: Option<f64>,
    /// ATR-normalized distance of close from the baseline.
    pub atr_distance: Option<f64>,
    /// MA alignment score (−150..+110 surfacing range).
    pub ma_health: Option<f64>,
    /// Squeeze gauge (`stdev / atr`).
    pub volatility_ratio: Option<f64>,
    /// Participation gauge (`vol / vol_sma`, guard-gated with carry).
    pub vol_ratio: Option<f64>,
    /// Frozen regime label.
    pub regime: MarketRegime,
    /// `|health| / 100`.
    pub trend_strength: Option<f64>,
    /// Beyond ±15 reads ±1, else 0 (absent health reads 0, mirroring the
    /// oracle's `?:` else-branch on warming bars).
    pub trend_direction: Option<f64>,
}

impl MarketContext {
    /// Fresh context with Pine declaration defaults (zeroed floats, absent
    /// volume, `RANGING`): the carry-chain seed before any `update`.
    #[must_use]
    pub fn fresh(tf: Timeframe) -> Self {
        Self {
            tf,
            ma_primary: Some(0.0),
            ma_signal: Some(0.0),
            ma_mean: Some(0.0),
            atr: Some(0.0),
            atr_long: Some(0.0),
            cci: Some(0.0),
            rsi: Some(0.0),
            stdev: Some(0.0),
            vol: None,
            vol_sma: Some(0.0),
            atr_distance: Some(0.0),
            ma_health: Some(0.0),
            volatility_ratio: Some(0.0),
            vol_ratio: Some(0.0),
            regime: MarketRegime::Ranging,
            trend_strength: Some(0.0),
            trend_direction: Some(0.0),
        }
    }
}

/// Frozen `fetch` parameter bundle for one span: the nine kernel series
/// `fetch` reads (ALMA-8, VWMA-12, EMA-21, ATR-14/50, CCI-14, RSI-14,
/// STDEV-14, SMA-20 over the volume leg). Kernel ids reference the
/// implementations' own `ID` constants — no second id table lives here.
#[derive(Debug, Clone)]
pub struct MarketContextKey {
    /// Resolved span.
    pub tf: Timeframe,
    /// ALMA(8, 0.85, 6).
    pub alma8: ParamSet,
    /// VWMA(12).
    pub vwma12: ParamSet,
    /// EMA(21).
    pub ema21: ParamSet,
    /// ATR(14).
    pub atr14: ParamSet,
    /// ATR(50).
    pub atr50: ParamSet,
    /// CCI(14).
    pub cci14: ParamSet,
    /// RSI(14).
    pub rsi14: ParamSet,
    /// STDEV(14, nbdev 1).
    pub stdev14: ParamSet,
    /// SMA(20) over the volume leg.
    pub vol_sma20: ParamSet,
}

impl MarketContextKey {
    /// Bundle the frozen `fetch` parameters for one span.
    #[must_use]
    pub fn new(tf: Timeframe) -> Self {
        let len = |n: i64| ParamSet::new().with_int("length", n);
        Self {
            tf,
            alma8: ParamSet::new()
                .with_int("length", 8)
                .with_float("offset", 0.85)
                .with_float("sigma", 6.0),
            vwma12: len(12),
            ema21: len(21),
            atr14: len(14),
            atr50: len(50),
            cci14: len(14),
            rsi14: len(14),
            stdev14: ParamSet::new().with_int("length", 14).with_float("nbdev", 1.0),
            vol_sma20: len(20),
        }
    }

    /// Span in milliseconds, from the single ms table.
    #[must_use]
    pub fn tf_ms(&self) -> i64 {
        self.tf.duration_ms()
    }

    /// The nine store requests backing one `update` (series key plus the
    /// parameters it computes with; the volume SMA reads the volume leg).
    /// Spans come from `Timeframe`, so key construction cannot fail.
    #[must_use]
    pub fn series_keys(&self) -> Vec<(SeriesKey, ParamSet)> {
        let tf_ms = self.tf_ms();
        let one = |kernel: &str, params: &ParamSet| SeriesKey {
            tf_ms,
            kernel: kernel.to_owned(),
            params_hash: params.fingerprint(),
            volume_source: false,
        };
        vec![
            (one(AlmaIndicator::ID, &self.alma8), self.alma8.clone()),
            (one(VwmaIndicator::ID, &self.vwma12), self.vwma12.clone()),
            (one(EmaIndicator::ID, &self.ema21), self.ema21.clone()),
            (one(AtrIndicator::ID, &self.atr14), self.atr14.clone()),
            (one(AtrIndicator::ID, &self.atr50), self.atr50.clone()),
            (one(CciIndicator::ID, &self.cci14), self.cci14.clone()),
            (one(RsiIndicator::ID, &self.rsi14), self.rsi14.clone()),
            (one(StddevIndicator::ID, &self.stdev14), self.stdev14.clone()),
            (
                SeriesKey {
                    tf_ms,
                    kernel: SmaIndicator::ID.to_owned(),
                    params_hash: self.vol_sma20.fingerprint(),
                    volume_source: true,
                },
                self.vol_sma20.clone(),
            ),
        ]
    }
}

/// MA alignment score, ported 1:1 from `getHealthMAs` (same branches, same
/// op order, same frozen constants): full-stack alignment scores through
/// `fast_tanh`, counter-alignment mirrors negative, mixed stacks blend by
/// distance share with a deadband.
///
/// Absence mirrors Pine `na` structurally, never by early return: the
/// oracle initializes its accumulators to zero, na comparisons read false,
/// and only the deadband decides — so absent legs on an unaligned stack
/// read exactly `0.0` (pinned by the warming/flat probes), while a true
/// comparison against an absent distance poisons to na through `NaN`, and
/// a zero ATR (zero true ranges) behaves the same way. The final
/// `fin_or_none` is the locked na policy.
#[must_use]
pub fn health_mas(
    atr: Option<f64>,
    fast: Option<f64>,
    mid: Option<f64>,
    slow: Option<f64>,
    k: f64,
) -> Option<f64> {
    // Pine `na` comparisons read false; present-but-unequal stacks compare
    // on raw legs even when the distances themselves are absent.
    let gt = |left: Option<f64>, right: Option<f64>| {
        matches!((left, right), (Some(left), Some(right)) if left > right)
    };
    let lt = |left: Option<f64>, right: Option<f64>| {
        matches!((left, right), (Some(left), Some(right)) if left < right)
    };
    let dist = |a: Option<f64>, b: Option<f64>| match (a, b, atr) {
        (Some(a), Some(b), Some(atr)) if atr != 0.0 => Some((a - b) / atr),
        _ => None,
    };
    let fast_dist = dist(fast, mid);
    let slow_dist = dist(mid, slow);
    let score = if gt(fast, mid) && gt(mid, slow) {
        match (fast_dist, slow_dist) {
            (Some(fast), Some(slow)) => {
                HEALTH_UPPER_BASE + fast_tanh(k * fast.min(slow)) * HEALTH_AMPLITUDE
            }
            // Unreachable with the frozen bundle (full alignment needs the
            // slow average, which warms long after ATR): fail-closed na.
            _ => return None,
        }
    } else if lt(fast, mid) && lt(mid, slow) {
        match (fast_dist, slow_dist) {
            (Some(fast), Some(slow)) => {
                HEALTH_LOWER_BASE - fast_tanh(k * fast.abs().min(slow.abs())) * HEALTH_AMPLITUDE
            }
            _ => return None,
        }
    } else {
        let mut bullish = 0.0;
        let mut bearish = 0.0;
        // A true comparison against an absent distance adds Pine `na`
        // (`NaN` here): the total poisons, the deadband reads false, and
        // the blend resolves na — exactly the oracle's mixed-stack path.
        if gt(fast, mid) {
            bullish += fast_dist.map_or(f64::NAN, f64::abs);
        }
        if lt(fast, mid) {
            bearish += fast_dist.map_or(f64::NAN, f64::abs);
        }
        if gt(mid, slow) {
            bullish += slow_dist.map_or(f64::NAN, |dist| dist.abs() * HEALTH_MID_WEIGHT);
        }
        if lt(mid, slow) {
            bearish += slow_dist.map_or(f64::NAN, |dist| dist.abs() * HEALTH_MID_WEIGHT);
        }
        let total = bullish + bearish;
        if total <= HEALTH_DEADBAND {
            0.0
        } else {
            HEALTH_MIXED_BASE + (bullish / total * HEALTH_MIXED_SPAN)
        }
    };
    fin_or_none(score)
}

/// The three normalized regime surfaces behind [`detect_regime`] (trend
/// magnitude, `8×` volatility ratio, `10×` volume ratio): one helper so the
/// frozen detector and the stability wrapper share the exact transforms —
/// the `8×`/`10×`/abs shapes exist once, and the detector recomposes over
/// them byte-identically.
fn regime_surfaces(
    ma_health: Option<f64>,
    volatility_ratio: Option<f64>,
    vol_ratio: Option<f64>,
) -> (Option<f64>, Option<f64>, Option<f64>) {
    let trend = ma_health.and_then(fin_or_none).map(f64::abs);
    let volatility = volatility_ratio
        .and_then(fin_or_none)
        .map(|ratio| VOLATILITY_SCALE * ratio);
    let volume = vol_ratio.and_then(fin_or_none).map(|ratio| VOLUME_SCALE * ratio);
    (trend, volatility, volume)
}

/// Ratio-based regime label, ported 1:1 from `detectRegime`: the highest
/// polynomial surface wins, ties yield to trend first. The oracle teaches
/// two na-semantics the hard way (warming-bar probe): `math.max`
/// propagates absence (any absent score poisons the max, so no tie can
/// fire), and absent-`==`-absent never ties — a warming bar with trend 0.0
/// present reads `LOW_VOLUME`, never the `0 == 0` trend tie. A present
/// trend past its floor still fires with absent siblings (the `> 20`
/// disjunct needs no tie); `VOLATILE`/`RANGING` need all three present.
/// All absent reads `LOW_VOLUME` — the fail-closed quiet label.
/// Thresholds frozen, never tuned.
///
/// `#[allow(clippy::float_cmp)]`: the `max == surface` ties mirror Pine
/// `==` exactly — a tolerance band would reassign ties and diverge from
/// the oracle on chop-shaped tapes.
#[allow(clippy::float_cmp)]
#[must_use]
pub fn detect_regime(
    ma_health: Option<f64>,
    volatility_ratio: Option<f64>,
    vol_ratio: Option<f64>,
) -> MarketRegime {
    let (trend, volatility, volume) = regime_surfaces(ma_health, volatility_ratio, vol_ratio);
    // Propagating max: one absent surface voids every tie.
    let max = match (trend, volatility, volume) {
        (Some(trend), Some(volatility), Some(volume)) => {
            Some(trend.max(volatility).max(volume))
        }
        _ => None,
    };
    let tied = |score: Option<f64>| match (max, score) {
        (Some(top), Some(own)) => top == own,
        _ => false,
    };
    if trend.is_some_and(|score| score > TREND_SCORE_FLOOR) || tied(trend) {
        if ma_health.is_some_and(|health| health > 0.0) {
            MarketRegime::TrendingBull
        } else {
            MarketRegime::TrendingBear
        }
    } else if tied(volatility)
        && volatility.is_some_and(|score| score > VOLATILITY_SCORE_FLOOR)
    {
        MarketRegime::Volatile
    } else if tied(volume) && volume.is_some_and(|score| score > VOLUME_SCORE_FLOOR) {
        MarketRegime::Ranging
    } else {
        MarketRegime::LowVolume
    }
}

/// Label-stable regime read for consumers that need a quiet label (charts,
/// gates): the frozen detector decides the fresh label, and this wrapper
/// decides whether the switch earns the flicker. Switching across surfaces
/// needs the fresh surface to beat the held surface by more than
/// [`REGIME_HYSTERESIS`]; going quiet needs the held surface decayed below
/// its win floor minus the band. Same-surface sign flips (bull↔bear) switch
/// immediately — the sign is the signal, not noise. Quiet never vetoes a
/// fresh signal, and vanished evidence never holds a stale label. Pure;
/// transition table pinned in tests. Never a substitute for
/// [`detect_regime`] in parity probes.
#[must_use]
pub fn detect_regime_stable(
    prev: MarketRegime,
    ma_health: Option<f64>,
    volatility_ratio: Option<f64>,
    vol_ratio: Option<f64>,
) -> MarketRegime {
    let fresh = detect_regime(ma_health, volatility_ratio, vol_ratio);
    if fresh == prev || prev == MarketRegime::LowVolume {
        return fresh;
    }
    // Same surface (bull↔bear): the sign flip is decisive, never held.
    if matches!(prev, MarketRegime::TrendingBull | MarketRegime::TrendingBear)
        && matches!(fresh, MarketRegime::TrendingBull | MarketRegime::TrendingBear)
    {
        return fresh;
    }
    let (trend, volatility, volume) = regime_surfaces(ma_health, volatility_ratio, vol_ratio);
    // Held surface plus the floor that won it; quiet holds neither.
    let (held, floor) = match prev {
        MarketRegime::TrendingBull | MarketRegime::TrendingBear => (trend, Some(TREND_SCORE_FLOOR)),
        MarketRegime::Volatile => (volatility, Some(VOLATILITY_SCORE_FLOOR)),
        MarketRegime::Ranging => (volume, Some(VOLUME_SCORE_FLOOR)),
        MarketRegime::LowVolume => (None, None),
    };
    // A won fresh label always carries its surface (every non-quiet win
    // needs its surface present); quiet carries none.
    let won = match fresh {
        MarketRegime::TrendingBull | MarketRegime::TrendingBear => trend,
        MarketRegime::Volatile => volatility,
        MarketRegime::Ranging => volume,
        MarketRegime::LowVolume => None,
    };
    // Held evidence gone: never hold a stale label.
    let Some(held) = held else {
        return fresh;
    };
    match (floor, won) {
        // Going quiet: switch once decayed below floor minus band.
        (Some(floor), None) => {
            if held < floor - REGIME_HYSTERESIS {
                fresh
            } else {
                prev
            }
        }
        // Cross-surface: fresh must beat held by more than the band.
        (_, Some(won)) => {
            if won > held + REGIME_HYSTERESIS {
                fresh
            } else {
                prev
            }
        }
        // Floorless hold (unreachable — every held surface carries its win
        // floor): fail toward the fresh signal, never toward a stale hold.
        (None, None) => fresh,
    }
}

/// Read one bundle leg at frame bar `at`: resolve the request, then the
/// ensured cache, then the bar value (absent stays na through the fold).
/// Where the bundle carries several legs under one kernel id (the two ATR
/// spans), `params` selects by parameter fingerprint — never by position,
/// so request order cannot move the reading. Where `(kernel,
/// volume_source)` is unique, `params` stays `None` and the single match is
/// asserted (the frozen bundle holds one leg per remaining pair).
fn read_leg(
    store: &MtfStore,
    requests: &[(SeriesKey, ParamSet)],
    at: usize,
    kernel: &str,
    volume_source: bool,
    params: Option<&ParamSet>,
) -> Result<Option<f64>, KernelError> {
    let mut candidates = requests
        .iter()
        .filter(|(candidate, _)| candidate.kernel == kernel && candidate.volume_source == volume_source);
    let (series, _) = if let Some(params) = params {
        candidates
            .find(|(_, have)| have.fingerprint() == params.fingerprint())
            .ok_or_else(|| {
                KernelError::InvalidInput(format!(
                    "context bundle has no {kernel} series with these parameters"
                ))
            })?
    } else {
        let found: Vec<_> = candidates.collect();
        debug_assert_eq!(
            found.len(),
            1,
            "context bundle holds one {kernel} leg for volume_source={volume_source}"
        );
        found.into_iter().next().ok_or_else(|| {
            KernelError::InvalidInput(format!("context bundle has no {kernel} series"))
        })?
    };
    let cache = store.cache(series).ok_or_else(|| {
        KernelError::InvalidInput(format!("series {series} not ensured (call fetch_all first)"))
    })?;
    Ok(cache.values.get(at).copied().flatten())
}

/// Read one context at frame bar `at`: resolve the nine series plus the
/// close/volume legs, normalize (CCI × 3, RSI recentered × 2), fold the
/// derived metrics in Pine op order, and label. Every step propagates
/// absence; `prev` carries `vol`/`vol_ratio` past guard-failing bars (the
/// `nz` shape), defaulting to absent/zero fresh.
///
/// `requests` are the ensured series this read resolves against (one
/// span's legs — [`fetch_all`] threads its union per span instead of
/// re-deriving `series_keys()` here, so the read can never drift from what
/// was ensured). A `prev` read for another span is a caller bug and refuses
/// with a reason — carry never crosses spans.
///
/// Missing caches refuse (call `fetch_all` first — a missing series is a
/// caller bug, never market na); an out-of-range `at` refuses with a
/// reason. Within-range absent values stay na through the fold.
fn update_with_requests(
    store: &MtfStore,
    key: &MarketContextKey,
    requests: &[(SeriesKey, ParamSet)],
    at: usize,
    prev: Option<&MarketContext>,
) -> Result<MarketContext, KernelError> {
    if let Some(prior) = prev {
        if prior.tf != key.tf {
            return Err(KernelError::InvalidInput(format!(
                "context prev for {} read on {} (carry never crosses spans)",
                prior.tf.duration_ms(),
                key.tf_ms()
            )));
        }
    }
    let frame = store.frame(key.tf_ms()).ok_or_else(|| {
        KernelError::InvalidInput(format!(
            "no frame for {}ms (ensure a context series on this span first)",
            key.tf_ms()
        ))
    })?;
    if at >= frame.len() {
        return Err(KernelError::InvalidInput(format!(
            "context bar {at} out of range over {} confirmed buckets",
            frame.len()
        )));
    }
    let ma_primary = read_leg(store, requests, at, AlmaIndicator::ID, false, None)?;
    let ma_signal = read_leg(store, requests, at, VwmaIndicator::ID, false, None)?;
    let ma_mean = read_leg(store, requests, at, EmaIndicator::ID, false, None)?;
    // The two ATR spans share one kernel id: resolved by parameters, so a
    // reordered bundle reads identically.
    let atr = read_leg(store, requests, at, AtrIndicator::ID, false, Some(&key.atr14))?;
    let atr_long = read_leg(store, requests, at, AtrIndicator::ID, false, Some(&key.atr50))?;
    let cci = read_leg(store, requests, at, CciIndicator::ID, false, None)?
        .map(|value| value * CCI_SCALE);
    let rsi = read_leg(store, requests, at, RsiIndicator::ID, false, None)?
        .map(|value| (value - RSI_CENTER) * RSI_SCALE);
    let stdev = read_leg(store, requests, at, StddevIndicator::ID, false, None)?;
    let vol_sma = read_leg(store, requests, at, SmaIndicator::ID, true, None)?;
    let close = frame.close.get(at).copied();
    let vol = frame.volume.get(at).copied().or_else(|| prev.and_then(|prior| prior.vol));

    let atr_distance = match (close, ma_mean, atr) {
        // `atr` rides kernel outputs (finite where present), so the only
        // zero/infinite refusal `div_or_none` adds beyond the old explicit
        // guard is unreachable — the guard folds into the primitive.
        (Some(close), Some(mean), Some(atr)) => div_or_none(close - mean, atr),
        _ => None,
    };
    let ma_health = health_mas(atr, ma_primary, ma_signal, ma_mean, HEALTH_K);
    let volatility_ratio = match (stdev, atr) {
        // Same kernel-output finiteness argument as `atr_distance` above:
        // the explicit zero guard folds into the primitive.
        (Some(stdev), Some(atr)) => div_or_none(stdev, atr),
        _ => None,
    };
    // The volume guard is producer-confidence (do I have valid inputs?):
    // it blocks the update and carries the prior (fresh: the zero default),
    // never inventing a ratio. Label correctness reads the carried value
    // through the frozen detector, unchanged.
    let vol_ratio = match (vol, vol_sma) {
        (Some(vol), Some(sma)) if sma > 0.0 => fin_or_none(vol / sma),
        _ => prev.and_then(|prior| prior.vol_ratio).or(Some(0.0)),
    };
    let trend_strength = ma_health.map(|health| health.abs() / TREND_NORM).and_then(fin_or_none);
    let trend_direction = match ma_health {
        Some(health) if health > DIRECTION_BAND => Some(1.0),
        Some(health) if health < -DIRECTION_BAND => Some(-1.0),
        _ => Some(0.0),
    };
    let regime = detect_regime(ma_health, volatility_ratio, vol_ratio);
    Ok(MarketContext {
        tf: key.tf,
        ma_primary,
        ma_signal,
        ma_mean,
        atr,
        atr_long,
        cci,
        rsi,
        stdev,
        vol,
        vol_sma,
        atr_distance,
        ma_health,
        volatility_ratio,
        vol_ratio,
        regime,
        trend_strength,
        trend_direction,
    })
}

/// Read one context at frame bar `at` over this span's own bundle: the
/// single-span entry point over `update_with_requests`. Kept so direct
/// readers (probes, calibration) resolve exactly what [`fetch_all`]
/// ensures for the span — one bundle derivation, composed by both callers.
pub fn update(
    store: &MtfStore,
    key: &MarketContextKey,
    at: usize,
    prev: Option<&MarketContext>,
) -> Result<MarketContext, KernelError> {
    let requests = key.series_keys();
    update_with_requests(store, key, &requests, at, prev)
}

/// Multi-span fan-out over one shared store: ensure the union of series
/// once (one revision counter, shared subexpressions across spans), then
/// read each span at its latest confirmed bar. Returns one context per key,
/// in key order. Prev carry arrives as a map keyed by span (`tf_ms`): each
/// span reads only its own prior, so a stale span's carry can never leak
/// into another span through ordering; spans missing from the map read
/// fresh. An empty confirmed frame refuses — the forming tail never yields
/// a context.
pub fn fetch_all<S: BuildHasher>(
    store: &mut MtfStore,
    registry: &IndicatorRegistry,
    keys: &[MarketContextKey],
    prevs: &HashMap<i64, MarketContext, S>,
    now_ms: i64,
    lookahead: Lookahead,
) -> Result<Vec<MarketContext>, KernelError> {
    // First-wins union dedupe over a set (same outcome as the scan, stated
    // O(1) expected per insert instead of O(n²)).
    let mut union: Vec<(SeriesKey, ParamSet)> = Vec::new();
    let mut seen: HashSet<SeriesKey> = HashSet::new();
    for key in keys {
        for request in key.series_keys() {
            if seen.insert(request.0.clone()) {
                union.push(request);
            }
        }
    }
    store.ensure(registry, &union, now_ms, lookahead)?;
    keys.iter()
        .map(|key| {
            let len = store
                .frame(key.tf_ms())
                .ok_or_else(|| {
                    KernelError::InvalidInput(format!(
                        "no frame for {}ms (ensure a context series on this span first)",
                        key.tf_ms()
                    ))
                })?
                .len();
            let at = len.checked_sub(1).ok_or_else(|| {
                KernelError::InvalidInput(format!(
                    "no confirmed {}ms bucket at {now_ms}",
                    key.tf_ms()
                ))
            })?;
            // This span's ensured legs only (the union filtered to the
            // span), plus this span's prev: the read resolves exactly what
            // was ensured, and carry stays inside the span.
            let span_requests: Vec<(SeriesKey, ParamSet)> = union
                .iter()
                .filter(|(series, _)| series.tf_ms == key.tf_ms())
                .cloned()
                .collect();
            update_with_requests(store, key, &span_requests, at, prevs.get(&key.tf_ms()))
        })
        .collect()
}

#[cfg(test)]
#[path = "../tests/market_context_native.rs"]
mod market_context_native_tests;

#[cfg(test)]
#[path = "../tests/market_context_calibration.rs"]
mod market_context_calibration_tests;
