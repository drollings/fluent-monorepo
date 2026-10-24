//! IntegrationLite parity pane plus the pivot-threshold calibration record.
//!
//! [`integration_lite_view`] builds the headless pane from native kernels
//! over converted rows: RSI/CCI raw oscillators squashed through the
//! frozen tables, the EIT adaptive/signal pair as the ribbon band, and
//! pivot marks at the calibrated threshold. The sibling freeze harness
//! ([`compare_freeze`]) replays the same window through the Pine engine and
//! pins per-series max-diffs, so native and Pine agree bar-for-bar before
//! any pane caches pivot output.
//!
//! [`CALIBRATED_PIVOT_THRESHOLD`] is the frozen task-value control for
//! alert fitness (±100 nominal): flat/chop control windows must not fire,
//! and known-pivot windows must score full precision/recall at it. The
//! measured evidence lives in the checked-in calibration report beside the
//! fixtures — this module carries only the frozen wiring.

use fluent_wvr::prelude::*;
use fluent_wvr::{Describable, FieldAccess};
use protopyne_chart_model::{ChartRow, DisplayOnly};
use protopyne_kernels::bars::{estimate_delta_volume_estimated, fast_tanh, DeltaConfidence};
use protopyne_kernels::indicator::{nan_hole, OwnedOhlcv, ParamSet};
use protopyne_kernels::smoothing::{SmoothingFamily, MA_PERIOD_MAX, MA_PERIOD_MIN};

use crate::topology::{kernel_preview_frame, kernel_series};
use protopyne_kernels::market_context::{health_mas, HEALTH_K};

use fluent_wvr::validate::{check_for_execute, valid_output, Validate};

use fluent_egui::EguiError;
use fluent_egui::{
    delta_candle_view, pivot_marks, resolve_color, ribbon_view, DeltaView, PivotMark, RibbonView,
    ThemeConfig,
};

use crate::indicators::smooth_raw_for_display;
use crate::oscillators::{normalize_series, squash_series_to_100, squash_to_100, OscillatorKind};

/// Kernel length for the fast/slow raw oscillators (freeze-pinned: the Pine
/// freeze drivers evaluate `normalizeOsc` at length 14 on both arms).
pub const RAW_OSCILLATOR_LENGTH: i64 = 14;
/// EIT smoothing for the ribbon signal leg (script signal preset).
pub const SIGNAL_EIT_ALPHA: f64 = 0.12;
/// Frozen pivot threshold on the normalized band (task value: alert
/// fitness — flat/chop windows must not fire, known pivots must score
/// fully; see the calibration report beside the fixtures for the measured
/// evidence).
pub const CALIBRATED_PIVOT_THRESHOLD: f64 = 100.0;

/// Normalize one lite-pane arm (fast = RSI, slow = CCI).
///
/// Both are catalog rows with frozen constants, so the lift cannot refuse
/// them here; a refusal would mean the catalog lost an evaluable row, which
/// fails loudly instead of substituting defaults.
fn lite_normalize(raw: &[Option<f64>], kind: OscillatorKind) -> Vec<Option<f64>> {
    normalize_series(raw, kind).expect("lite fast/slow arms are evaluable")
}

/// One Lite leg selector: the panel resolves each leg independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiteLeg {
    Fast,
    Slow,
}

/// One bounded Lite leg from its configured kind: kernel raw at the leg
/// period, trailing smoothing when armed, the catalog squash scaled by the
/// leg gain, compressed onto the bounded band. Direct calls without
/// resolution read the neutral fallbacks (length 14, no smoothing, unit
/// gain), reproducing the frozen arms exactly; the workspace resolves
/// blanks against the panel kind rows before evaluation.
fn lite_panel_leg(
    rows: &[ChartRow],
    config: &IntegrationLiteConfig,
    leg: LiteLeg,
) -> Result<Vec<Option<f64>>, EguiError> {
    let (kind_slot, kind_name, period, smooth_slot, smooth_type, smooth_period, gain) = match leg {
        LiteLeg::Fast => (
            "fast_kind",
            config.fast_kind.as_str(),
            config.fast_period,
            "fast_smooth_type",
            config.fast_smooth_type.as_str(),
            config.fast_smooth_period,
            config.fast_k,
        ),
        LiteLeg::Slow => (
            "slow_kind",
            config.slow_kind.as_str(),
            config.slow_period,
            "slow_smooth_type",
            config.slow_smooth_type.as_str(),
            config.slow_smooth_period,
            config.slow_k,
        ),
    };
    let kind = IntegrationLiteConfig::leg_kind(kind_slot, kind_name)?;
    let frame = kernel_preview_frame(rows);
    let raw = kernel_series(
        lite_kernel(kind),
        &ParamSet::new().with_int("length", period.unwrap_or(RAW_OSCILLATOR_LENGTH)),
        &frame,
    )?;
    let holes = raw.iter().take_while(|slot| slot.is_none()).count();
    let smoothed = smooth_raw_for_display(
        &raw,
        rows,
        smooth_type,
        smooth_period.unwrap_or(3),
        holes,
        smooth_slot,
    )?;
    let unbounded = lite_normalize(&smoothed, kind);
    let scaled = gain.unwrap_or(1.0);
    Ok(unbounded
        .into_iter()
        .map(|slot| {
            slot.and_then(|value| {
                let shaped = squash_to_100(value * scaled);
                shaped.is_finite().then_some(shaped)
            })
        })
        .collect())
}

/// Fast arm of the lite pane (RSI).
const LITE_FAST_KIND: OscillatorKind = OscillatorKind::Rsi;
/// Slow arm of the lite pane (CCI).
const LITE_SLOW_KIND: OscillatorKind = OscillatorKind::Cci;

/// Kernel id for one lite arm, sourced from the catalog row instead of a
/// hardcoded string; both lite arms are kernel-backed rows, so a missing
/// kernel means the catalog lost an evaluable row and fails loudly.
fn lite_kernel(kind: OscillatorKind) -> &'static str {
    kind.kernel_id()
        .expect("lite fast/slow arms are kernel-backed")
}

/// Native kernel series behind one pane: raw oscillators plus the
/// smoothing pair, all over the same converted rows.
#[derive(Debug, Clone)]
pub struct LiteNative {
    /// Raw RSI readings (unsquashed).
    pub fast_raw: Vec<Option<f64>>,
    /// Raw CCI readings (unsquashed).
    pub slow_raw: Vec<Option<f64>>,
    /// EIT adaptive leg.
    pub adaptive: Vec<Option<f64>>,
    /// EIT smoothing of the adaptive leg (holes as `NaN` through the kernel
    /// na-policy, so gaps stay gaps).
    pub signal: Vec<Option<f64>>,
}

/// One synthetic close-only frame from a display series.
///
/// Every OHLC leg carries the value (holes as `NaN`) and volume is the
/// synthetic zero the smoothing kernels ignore. The values enter through the
/// [`DisplayOnly`] fence and are revealed exactly here — the auditable escape
/// for a display-descended preview frame. This is the single construction the
/// pane and the momentum inputs share, so the fence is named once.
fn synth_close_frame(values: &[Option<f64>]) -> OwnedOhlcv {
    let revealed: Vec<f64> = values
        .iter()
        .map(|slot| DisplayOnly::new(nan_hole(*slot)).reveal())
        .collect();
    OwnedOhlcv {
        open: revealed.clone(),
        high: revealed.clone(),
        low: revealed.clone(),
        close: revealed,
        volume: vec![0.0; values.len()],
    }
}

/// Evaluate the native pane series over converted rows.
pub fn native_lite_series(rows: &[ChartRow], eit_alpha: f64) -> Result<LiteNative, EguiError> {
    if !eit_alpha.is_finite() || eit_alpha <= 0.0 || eit_alpha > 1.0 {
        return Err(EguiError::InvalidParam {
            what: "eit_alpha".to_owned(),
            reason: "adaptive smoothing must sit within (0, 1]".to_owned(),
        });
    }
    let frame = kernel_preview_frame(rows);
    let length = ParamSet::new().with_int("length", RAW_OSCILLATOR_LENGTH);
    let fast_raw = kernel_series(lite_kernel(LITE_FAST_KIND), &length, &frame)?;
    let slow_raw = kernel_series(lite_kernel(LITE_SLOW_KIND), &length, &frame)?;
    let adaptive = kernel_series(
        "eit",
        &ParamSet::new().with_float("alpha", eit_alpha),
        &frame,
    )?;
    // Relayed adaptive legs descend from the fenced frame above, so they
    // stay fenced until the same reveal; the synthetic zero volume is a
    // literal, not display-descended, and needs no fence.
    let synth = synth_close_frame(&adaptive);
    let signal = kernel_series(
        "eit",
        &ParamSet::new().with_float("alpha", SIGNAL_EIT_ALPHA),
        &synth,
    )?;
    Ok(LiteNative {
        fast_raw,
        slow_raw,
        adaptive,
        signal,
    })
}

/// Headless IntegrationLite pane: normalized oscillators, ribbon band, and
/// pivots at `threshold` over the fast leg.
#[derive(Debug, Clone)]
pub struct IntegrationLiteView {
    /// Normalized fast oscillator (RSI arm).
    pub fast: Vec<Option<f64>>,
    /// Normalized slow oscillator (CCI arm).
    pub slow: Vec<Option<f64>>,
    /// Ribbon band between the smoothing pair.
    pub ribbon: RibbonView,
    /// Threshold crossings on the fast leg.
    pub pivots: Vec<PivotMark>,
    /// Threshold the pivots fired at.
    pub threshold: f64,
}

/// Build the pane at an explicit threshold.
pub fn integration_lite_view(
    rows: &[ChartRow],
    eit_alpha: f64,
    fill_alpha: f64,
    threshold: f64,
) -> Result<IntegrationLiteView, EguiError> {
    let native = native_lite_series(rows, eit_alpha)?;
    let fast = lite_normalize(&native.fast_raw, LITE_FAST_KIND);
    let slow = lite_normalize(&native.slow_raw, LITE_SLOW_KIND);
    let ribbon = ribbon_view(&native.adaptive, &native.signal, fill_alpha)?;
    let pivots = pivot_marks(&fast, threshold)?;
    Ok(IntegrationLiteView {
        fast,
        slow,
        ribbon,
        pivots,
        threshold,
    })
}

/// Build the pane at the calibrated threshold.
pub fn integration_lite_view_calibrated(
    rows: &[ChartRow],
    eit_alpha: f64,
    fill_alpha: f64,
) -> Result<IntegrationLiteView, EguiError> {
    integration_lite_view(rows, eit_alpha, fill_alpha, CALIBRATED_PIVOT_THRESHOLD)
}

/// Default oscillator brightness behind the dimmed legs (reference input).
pub const OSCILLATOR_BRIGHTNESS: u32 = 30;
/// Transparency of the volume colors (reference input, percent).
pub const VOLUME_TRANSPARENCY: f64 = 60.0;

/// Ribbon fill transparency for a fill strength, mirroring the reference
/// `95 - (60 * alpha)` shading: weak legs run nearly invisible, strong legs
/// plateau near half opacity. Total: clamps into `[0, 100]`; non-finite
/// strengths paint nothing rather than an undefined shade.
#[must_use]
pub fn ribbon_fill_transparency(fill_alpha: f64) -> f64 {
    if !fill_alpha.is_finite() {
        return 100.0;
    }
    (95.0 - 60.0 * fill_alpha).clamp(0.0, 100.0)
}

/// Oscillator leg transparency for a brightness input, mirroring the
/// reference `90 - brightness` dimming. Total: clamps into `[0, 100]`.
#[must_use]
pub fn oscillator_transparency(brightness: u32) -> f64 {
    f64::from(90_u32.saturating_sub(brightness))
}

/// One bar's momentum tint key from its direction crossed with the ribbon
/// trend: the single-bar core behind [`candle_momentum_keys`], so the frame
/// path derives per-column keys from the same borrowed trend slice instead
/// of materializing a fresh window `Vec`.
#[must_use]
pub fn momentum_key_for(open: f64, close: f64, bullish: bool) -> &'static str {
    match (close >= open, bullish) {
        (true, true) => "bull",
        (false, true) => "bull_down",
        (false, false) => "bear",
        (true, false) => "bear_up",
    }
}
/// Candle tint keys from bar direction crossed with the ribbon trend.
///
/// Mirrors the reference four-color `barcolor` branch in display space:
/// trend-up bars that closed up read `"bull"`, trend-up bars that closed
/// down read `"bull_down"` (retrace), trend-down bars that closed down read
/// `"bear"`, and trend-down bars that closed up read `"bear_up"` (relief).
/// Output lines up with `rows` index-for-index; refuses trend-length
/// mismatch. Flat bars (`close == open`) read with the trend.
pub fn candle_momentum_keys(
    rows: &[ChartRow],
    bullish: &[bool],
) -> Result<Vec<&'static str>, EguiError> {
    if rows.len() != bullish.len() {
        return Err(EguiError::InvalidParam {
            what: "series".to_owned(),
            reason: "rows and ribbon keys lengths differ".to_owned(),
        });
    }
    Ok(rows
        .iter()
        .zip(bullish.iter())
        .map(|(row, trend)| momentum_key_for(row.open, row.close, *trend))
        .collect())
}

/// EMA length of the ribbon mean leg (reference `i_ribbon_mean_period`;
/// the `EMA` factory branch ignores `param1`, so plain length-21 EMA).
pub const RIBBON_MEAN_LENGTH: i64 = 21;
/// Volume SMA length behind the conviction ratio (reference
/// `ta.sma(volume, 20)` in the context fetch).
pub const VOL_SMA_LENGTH: i64 = 20;
/// ATR length behind the health distances (reference context ATR).
pub const ATR_LENGTH: i64 = 14;
/// Ribbon health gain (reference `i_ribbon_health_k`; task value: display
/// fitness of the health fold, never producer confidence).
pub const RIBBON_HEALTH_K: f64 = 1.8;

/// Four tint corners at the reference `barcolor` call
/// (`i_color_bull`, `i_color_bear`, `i_color_bull_down`, `i_color_bear_up`),
/// resolved from the theme at the call site so the port itself stays
/// headless RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MomentumPalette {
    /// Bullish candle in a bullish trend.
    pub bull_up: [u8; 3],
    /// Bearish candle in a bearish trend.
    pub bear_down: [u8; 3],
    /// Bullish candle in a bearish trend (retrace).
    pub bull_down: [u8; 3],
    /// Bearish candle in a bullish trend (relief).
    pub bear_up: [u8; 3],
}

impl MomentumPalette {
    /// Resolve the four corners from the theme's Pine input hues. A broken
    /// theme refuses here — the caller surfaces it through the error chip
    /// instead of tinting from silent gray.
    pub fn from_theme(theme: &ThemeConfig) -> Result<Self, EguiError> {
        let channel =
            |key: &str| resolve_color(theme, key).map(|color| [color.r(), color.g(), color.b()]);
        Ok(Self {
            bull_up: channel("bull")?,
            bear_down: channel("bear")?,
            bull_down: channel("bull_down")?,
            bear_up: channel("bear_up")?,
        })
    }
}

/// One ported candle tint: momentum-gradient RGB plus Pine transparency.
///
/// The shell turns `transparency` into byte opacity through
/// `opacity_from_transparency` and paints it beside the candle geometry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MomentumTint {
    /// Gradient red channel.
    pub red: u8,
    /// Gradient green channel.
    pub green: u8,
    /// Gradient blue channel.
    pub blue: u8,
    /// `color.new` transparency percent within `[0, 100]`.
    pub transparency: f64,
}

/// Port of `libApex.getColorMomentumAlpha` (`_libApex.pine`): candle color
/// from price momentum and volume conviction with smooth gradients and
/// per-bar transparency.
///
/// Mirrors the reference line for line — `fast_tanh` conviction scaling,
/// `int()` truncation toward zero, momentum clamped to ±100, gradient
/// corner picks on candle direction (`close > open` is bullish; flat reads
/// bearish through the `else`), and the momentum leg scaled by conviction
/// before the gradient map. The one substitution is documented at
/// [`candle_momentum_series`]: `alpha` is the pane's fixed EIT smoothing
/// because the adaptive per-bar alpha needs LTF tick data the fixture
/// history never carries.
///
/// Non-finite inputs yield `None`, mirroring Pine `na` propagation (an `na`
/// gradient paints no tint, so the shell falls back to the discrete key).
#[must_use]
pub fn momentum_tint(
    open: f64,
    close: f64,
    momentum: f64,
    alpha: f64,
    vol_ratio: f64,
    palette: &MomentumPalette,
) -> Option<MomentumTint> {
    for value in [open, close, momentum, alpha, vol_ratio] {
        if !value.is_finite() {
            return None;
        }
    }
    // Higher conviction reads more opaque: `fast_tanh(alpha + vol_ratio) * 90`.
    let normalized = fast_tanh(alpha + vol_ratio) * 90.0;
    // Pine `int()` truncates toward zero; the clamped band is the valid
    // `color.new` range.
    let transparency = (100.0 - normalized).trunc().clamp(0.0, 100.0);
    let clamped = momentum.clamp(-100.0, 100.0);
    // Bullish candles blend relief-blue into bull-green as momentum rises;
    // bearish candles blend bear-red into retrace-orange.
    let (start, end) = if close > open {
        (palette.bear_up, palette.bull_up)
    } else {
        (palette.bear_down, palette.bull_down)
    };
    // `from_gradient(value, -100, 100, start, end)`: conviction scales the
    // leg before the map, so weak bars sit mid-blend.
    let position = ((clamped * normalized / 100.0 + 100.0) / 200.0).clamp(0.0, 1.0);
    let mix = |low: u8, high: u8| {
        (f64::from(low) + (f64::from(high) - f64::from(low)) * position)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Some(MomentumTint {
        red: mix(start[0], end[0]),
        green: mix(start[1], end[1]),
        blue: mix(start[2], end[2]),
        transparency,
    })
}

/// Per-bar momentum inputs behind the tint port, over one row window.
#[derive(Debug, Clone, PartialEq)]
pub struct CandleMomentum {
    /// Ribbon health (`getHealthMAs` over ATR/adaptive/signal/mean, gained);
    /// holes where any leg is still warming.
    pub health: Vec<Option<f64>>,
    /// Guarded volume conviction (`vol / vol_sma`, `0` past the warmup).
    pub vol_ratio: Vec<f64>,
}

/// Build the momentum inputs from native kernels over converted rows.
///
/// `health` reuses the kernel truth — ATR-14, the pane's adaptive/signal
/// pair, an EMA-21 mean relayed through the shared synth-frame trick as the
/// signal leg, and [`health_mas`] with a per-bar gain of
/// `vol_ratio * (alpha + RIBBON_HEALTH_K)`, all × `RIBBON_HEALTH_K` — while
/// `vol_ratio` mirrors the context fetch (`ta.sma(volume, 20)`, ratio guarded
/// by `sma > 0`, else `0`). Warmup holes stay holes; every output lines up
/// with `rows`. Refuses non-positive or non-finite `eit_alpha`, like the pane
/// build.
pub fn candle_momentum_series(
    rows: &[ChartRow],
    eit_alpha: f64,
) -> Result<CandleMomentum, EguiError> {
    let (health, vol_ratio) = ribbon_health_inputs(rows, eit_alpha, RIBBON_HEALTH_K)?;
    Ok(CandleMomentum { health, vol_ratio })
}

/// Ribbon health and the volume conviction behind it, at an explicit gain.
///
/// The one kernel fold both [`candle_momentum_series`] and the sub-pane share:
/// ATR-14, the pane's adaptive/signal pair, an EMA-21 mean, and [`health_mas`]
/// with a per-bar gain of `vol_ratio * (eit_alpha + health_k)`, all ×
/// `health_k`. `vol_ratio` is the guarded volume conviction. Warmup holes stay
/// holes; every output lines up with `rows`. Refuses non-positive or
/// non-finite `eit_alpha`.
fn ribbon_health_inputs(
    rows: &[ChartRow],
    eit_alpha: f64,
    health_k: f64,
) -> Result<(Vec<Option<f64>>, Vec<f64>), EguiError> {
    if !eit_alpha.is_finite() || eit_alpha <= 0.0 || eit_alpha > 1.0 {
        return Err(EguiError::InvalidParam {
            what: "eit_alpha".to_owned(),
            reason: "adaptive smoothing must sit within (0, 1]".to_owned(),
        });
    }
    let native = native_lite_series(rows, eit_alpha)?;
    let mean = kernel_series(
        "ema",
        &ParamSet::new().with_int("length", RIBBON_MEAN_LENGTH),
        &synth_close_frame(&native.adaptive),
    )?;
    let frame = kernel_preview_frame(rows);
    let atr = kernel_series(
        "atr",
        &ParamSet::new().with_int("length", ATR_LENGTH),
        &frame,
    )?;
    let volumes: Vec<Option<f64>> = rows.iter().map(|row| Some(row.volume as f64)).collect();
    let vol_sma = kernel_series(
        "sma",
        &ParamSet::new().with_int("length", VOL_SMA_LENGTH),
        &synth_close_frame(&volumes),
    )?;
    let vol_ratio: Vec<f64> = rows
        .iter()
        .zip(vol_sma.iter())
        .map(|(row, sma)| match sma {
            Some(sma) if *sma > 0.0 => {
                let ratio = row.volume as f64 / sma;
                if ratio.is_finite() {
                    ratio
                } else {
                    0.0
                }
            }
            _ => 0.0,
        })
        .collect();
    let health: Vec<Option<f64>> = (0..rows.len())
        .map(|index| {
            let gain = vol_ratio[index] * (eit_alpha + health_k);
            let leg = |series: &[Option<f64>]| series.get(index).and_then(|slot| *slot);
            health_mas(
                leg(&atr),
                leg(&native.adaptive),
                leg(&native.signal),
                leg(&mean),
                gain,
            )
            .map(|score| score * health_k)
        })
        .collect();
    Ok((health, vol_ratio))
}

/// Tint every bar through the ported gradient, preserving holes.
///
/// Output lines up with `rows` index-for-index: bars whose health is still
/// warming yield `None` so the shell falls back to the discrete momentum
/// key. Refuses length mismatch and non-positive or non-finite `alpha`.
pub fn momentum_tints(
    rows: &[ChartRow],
    momentum: &CandleMomentum,
    alpha: f64,
    palette: &MomentumPalette,
) -> Result<Vec<Option<MomentumTint>>, EguiError> {
    if rows.len() != momentum.health.len() || rows.len() != momentum.vol_ratio.len() {
        return Err(EguiError::InvalidParam {
            what: "series".to_owned(),
            reason: "rows, health, and conviction lengths must match".to_owned(),
        });
    }
    if !alpha.is_finite() || alpha <= 0.0 || alpha > 1.0 {
        return Err(EguiError::InvalidParam {
            what: "alpha".to_owned(),
            reason: "conviction smoothing must sit within (0, 1]".to_owned(),
        });
    }
    Ok(rows
        .iter()
        .zip(momentum.health.iter().zip(momentum.vol_ratio.iter()))
        .map(|(row, (health, ratio))| match health {
            Some(score) => momentum_tint(row.open, row.close, *score, alpha, *ratio, palette),
            None => None,
        })
        .collect())
}

/// Frozen Pine-comparable series: squashed oscillators plus the adaptive leg.
#[derive(Debug, Clone)]
pub struct LiteFrozen {
    /// Normalized fast oscillator.
    pub fast: Vec<Option<f64>>,
    /// Normalized slow oscillator.
    pub slow: Vec<Option<f64>>,
    /// EIT adaptive leg.
    pub adaptive: Vec<Option<f64>>,
}

impl LiteFrozen {
    /// Squash native raws into the frozen comparison shape.
    #[must_use]
    pub fn from_native(native: &LiteNative) -> Self {
        Self {
            fast: lite_normalize(&native.fast_raw, LITE_FAST_KIND),
            slow: lite_normalize(&native.slow_raw, LITE_SLOW_KIND),
            adaptive: native.adaptive.clone(),
        }
    }
}

/// Max-diff over one paired series: bars where both sides read count as
/// compared; presence mismatches and non-finite pairs diverge infinitely so
/// the pin fails loudly instead of silently skipping.
#[must_use]
pub fn series_max_diff(native: &[Option<f64>], pine: &[Option<f64>]) -> (f64, usize) {
    let mut diff = 0.0_f64;
    let mut compared = 0_usize;
    for (left, right) in native.iter().zip(pine.iter()) {
        match (left, right) {
            (Some(a), Some(b)) if a.is_finite() && b.is_finite() => {
                diff = diff.max((a - b).abs());
                compared += 1;
            }
            (None, None) => {}
            _ => {
                diff = f64::INFINITY;
            }
        }
    }
    (diff, compared)
}

/// Per-series freeze comparison between native and Pine panes.
#[derive(Debug, Clone, Copy)]
pub struct FreezePair {
    /// Maximum absolute difference over compared bars.
    pub max_abs_diff: f64,
    /// Bars where both sides read.
    pub compared: usize,
}

/// Native-vs-Pine freeze report over the three pinned series.
#[derive(Debug, Clone, Copy)]
pub struct FreezeReport {
    /// Fast oscillator agreement.
    pub fast: FreezePair,
    /// Slow oscillator agreement.
    pub slow: FreezePair,
    /// Adaptive leg agreement.
    pub adaptive: FreezePair,
}

/// Compare native and Pine frozen panes series-by-series.
#[must_use]
pub fn compare_freeze(native: &LiteFrozen, pine: &LiteFrozen) -> FreezeReport {
    let pair = |left: &[Option<f64>], right: &[Option<f64>]| {
        let (max_abs_diff, compared) = series_max_diff(left, right);
        FreezePair {
            max_abs_diff,
            compared,
        }
    };
    FreezeReport {
        fast: pair(&native.fast, &pine.fast),
        slow: pair(&native.slow, &pine.slow),
        adaptive: pair(&native.adaptive, &pine.adaptive),
    }
}

/// Precision and recall of the pivot detector at `threshold` against labeled
/// `expected` marks (exact index-and-side match). Empty outputs score
/// perfect only against empty expectations.
pub fn precision_recall(
    fast: &[Option<f64>],
    expected: &[PivotMark],
    threshold: f64,
) -> Result<(f64, f64), EguiError> {
    let got = pivot_marks(fast, threshold)?;
    let hits = got.iter().filter(|mark| expected.contains(mark)).count();
    let precision = if got.is_empty() {
        f64::from(expected.is_empty())
    } else {
        hits as f64 / got.len() as f64
    };
    let recall = if expected.is_empty() {
        1.0
    } else {
        hits as f64 / expected.len() as f64
    };
    Ok((precision, recall))
}

/// Height of the centered delta-volume histogram at average volume
/// (task-value: visual scaling on the sub-pane zero axis; strictly inside the
/// `[-100, +100]` oscillator viewport).
pub const BOUNDED_DELTA_UNIT: f64 = 80.0;
/// MA-health oscillator gain (reference `i_ma_health_k`; task value: display
/// fitness of the health fold, never producer confidence).
pub const MA_HEALTH_K: f64 = 1.2;
/// ALMA length behind the MA-health primary leg (reference context ALMA-8).
const MA_PRIMARY_LENGTH: i64 = 8;
/// ALMA offset behind the MA-health primary leg.
const MA_PRIMARY_OFFSET: f64 = 0.85;
/// ALMA sigma behind the MA-health primary leg.
const MA_PRIMARY_SIGMA: f64 = 6.0;
/// VWMA length behind the MA-health signal leg (reference context VWMA-12).
const MA_SIGNAL_LENGTH: i64 = 12;

fn default_eit_alpha() -> f64 {
    0.14
}

fn default_fill_alpha() -> f64 {
    0.5
}

fn default_ribbon_health_k() -> f64 {
    RIBBON_HEALTH_K
}

fn default_ma_health_k() -> f64 {
    MA_HEALTH_K
}

fn default_delta_unit() -> f64 {
    BOUNDED_DELTA_UNIT
}

fn default_pivot_threshold() -> f64 {
    CALIBRATED_PIVOT_THRESHOLD
}

fn default_fast_kind() -> String {
    "rsi".to_owned()
}

fn default_slow_kind() -> String {
    "cci".to_owned()
}

fn default_show_ribbon() -> bool {
    true
}

/// One IntegrationLite indicator's configuration: the single schema-driven
/// control truth behind its settings pane.
///
/// Every field is a `Component` field, so the settings modal renders from the
/// `describe()` schema and a new knob appears without a UI edit. `eit_alpha`
/// and `fill_alpha` route to the Style tab (name carries `alpha`),
/// `ribbon_health_k` / `ma_health_k` / `delta_unit` to Inputs, and
/// `show_ribbon` to Visibility.
#[derive(
    Debug, Clone, bon::Builder, serde::Serialize, serde::Deserialize, FieldAccess, Describable,
)]
#[builder(start_fn = new)]
pub struct IntegrationLiteConfig {
    /// EIT adaptive smoothing of the ribbon source.
    #[field(desc = "EIT adaptive smoothing", min = 0.03, max = 1.0)]
    #[builder(default = default_eit_alpha())]
    pub eit_alpha: f64,
    /// Ribbon fill strength.
    #[field(desc = "Ribbon fill strength", min = 0.0, max = 1.0)]
    #[builder(default = default_fill_alpha())]
    pub fill_alpha: f64,
    /// Ribbon-health oscillator gain (task value: display fitness of the
    /// health fold, never producer confidence).
    #[field(desc = "Ribbon health gain", min = 0.2)]
    #[builder(default = default_ribbon_health_k())]
    pub ribbon_health_k: f64,
    /// MA-health oscillator gain (task value: display fitness of the health
    /// fold, never producer confidence).
    #[field(desc = "MA health gain", min = 0.2)]
    #[builder(default = default_ma_health_k())]
    pub ma_health_k: f64,
    /// Delta-volume histogram height at average volume (task value: visual
    /// scaling, never producer confidence — the split's self-doubt rides
    /// `delta_confidence` beside it, never inside it).
    #[field(desc = "Delta volume height at average volume", min = 0.0)]
    #[builder(default = default_delta_unit())]
    pub delta_unit: f64,
    /// Pivot threshold in Pine-scale (unbounded) units: the bounded fast leg
    /// fires at the squash of this value. Defaults to the frozen task value;
    /// the script input defaults higher on its gained leg, which would miss
    /// the frozen calibration windows, so the port keeps the frozen default
    /// and offers the script value as a reachable dial stop.
    #[field(desc = "Pivot threshold (Pine-scale)", min = 20.0)]
    #[builder(default = default_pivot_threshold())]
    pub pivot_threshold: f64,
    /// Whether the trend ribbon paints.
    #[field(desc = "Show the trend ribbon")]
    #[builder(default = default_show_ribbon())]
    pub show_ribbon: bool,
    /// Fast oscillator kind: the catalog leg behind the fast arm, resolved
    /// against the panel kind row for period, smoothing, and gain.
    #[field(
        desc = "Fast oscillator kind",
        variants = "crate::oscillator_panel::lite_leg_kind_ids"
    )]
    #[builder(default = default_fast_kind())]
    pub fast_kind: String,
    /// Fast-arm length in bars (blank follows the panel kind row).
    #[field(desc = "Fast arm length in bars", min = 2, max = 200)]
    pub fast_period: Option<i64>,
    /// Fast-arm smoothing family (`NA` follows a disarmed panel row).
    #[field(
        desc = "Fast-arm smoothing",
        variants = "protopyne_kernels::smoothing::length_smoothing_ids"
    )]
    #[builder(default = "na".to_owned())]
    pub fast_smooth_type: String,
    /// Fast-arm smoothing window in bars (blank follows the panel kind row).
    #[field(desc = "Fast-arm smoothing window in bars", min = 2, max = 200)]
    pub fast_smooth_period: Option<i64>,
    /// Fast-arm gain (blank follows the panel kind row).
    #[field(desc = "Fast-arm display gain", min = 0.0)]
    pub fast_k: Option<f64>,
    /// Slow oscillator kind: the catalog leg behind the slow arm, resolved
    /// against the panel kind row for period, smoothing, and gain.
    #[field(
        desc = "Slow oscillator kind",
        variants = "crate::oscillator_panel::lite_leg_kind_ids"
    )]
    #[builder(default = default_slow_kind())]
    pub slow_kind: String,
    /// Slow-arm length in bars (blank follows the panel kind row).
    #[field(desc = "Slow arm length in bars", min = 2, max = 200)]
    pub slow_period: Option<i64>,
    /// Slow-arm smoothing family (`NA` follows a disarmed panel row).
    #[field(
        desc = "Slow-arm smoothing",
        variants = "protopyne_kernels::smoothing::length_smoothing_ids"
    )]
    #[builder(default = "na".to_owned())]
    pub slow_smooth_type: String,
    /// Slow-arm smoothing window in bars (blank follows the panel kind row).
    #[field(desc = "Slow-arm smoothing window in bars", min = 2, max = 200)]
    pub slow_smooth_period: Option<i64>,
    /// Slow-arm gain (blank follows the panel kind row).
    #[field(desc = "Slow-arm display gain", min = 0.0)]
    pub slow_k: Option<f64>,
}

impl IntegrationLiteConfig {
    /// Fail-closed self-consistency check.
    pub fn validate(&self) -> Result<(), EguiError> {
        if !self.eit_alpha.is_finite() || self.eit_alpha <= 0.0 || self.eit_alpha > 1.0 {
            return Err(EguiError::InvalidParam {
                what: "eit_alpha".to_owned(),
                reason: "adaptive smoothing must sit within (0, 1]".to_owned(),
            });
        }
        if !self.fill_alpha.is_finite() || self.fill_alpha < 0.0 || self.fill_alpha > 1.0 {
            return Err(EguiError::InvalidParam {
                what: "fill_alpha".to_owned(),
                reason: "ribbon fill strength must sit within [0, 1]".to_owned(),
            });
        }
        if !self.ribbon_health_k.is_finite() || self.ribbon_health_k <= 0.0 {
            return Err(EguiError::InvalidParam {
                what: "ribbon_health_k".to_owned(),
                reason: "ribbon health gain must be positive and finite".to_owned(),
            });
        }
        if !self.ma_health_k.is_finite() || self.ma_health_k <= 0.0 {
            return Err(EguiError::InvalidParam {
                what: "ma_health_k".to_owned(),
                reason: "MA health gain must be positive and finite".to_owned(),
            });
        }
        if !self.delta_unit.is_finite() || self.delta_unit <= 0.0 {
            return Err(EguiError::InvalidParam {
                what: "delta_unit".to_owned(),
                reason: "delta volume unit must be positive and finite".to_owned(),
            });
        }
        if !self.pivot_threshold.is_finite() || self.pivot_threshold < 20.0 {
            return Err(EguiError::InvalidParam {
                what: "pivot_threshold".to_owned(),
                reason: format!(
                    "pivot threshold must be finite and at least 20.0, got {}",
                    self.pivot_threshold
                ),
            });
        }
        for (slot, kind) in [
            ("fast_kind", &self.fast_kind),
            ("slow_kind", &self.slow_kind),
        ] {
            if !crate::oscillator_panel::LITE_LEG_KINDS.contains(&kind.as_str()) {
                return Err(EguiError::InvalidParam {
                    what: (*slot).to_owned(),
                    reason: format!("{kind:?} is not a lite leg kind (rsi, cci, cmo, mfi, stoch)"),
                });
            }
        }
        for (slot, window) in [
            ("fast_period", self.fast_period),
            ("slow_period", self.slow_period),
            ("fast_smooth_period", self.fast_smooth_period),
            ("slow_smooth_period", self.slow_smooth_period),
        ] {
            if window.is_some_and(|value| !(MA_PERIOD_MIN..=MA_PERIOD_MAX).contains(&value)) {
                return Err(EguiError::InvalidParam {
                    what: slot.to_owned(),
                    reason: format!(
                        "leg length must sit within [{MA_PERIOD_MIN}, {MA_PERIOD_MAX}], got {window:?}"
                    ),
                });
            }
        }
        for (slot, smooth) in [
            ("fast_smooth_type", &self.fast_smooth_type),
            ("slow_smooth_type", &self.slow_smooth_type),
        ] {
            match SmoothingFamily::parse(smooth) {
                Some(SmoothingFamily::Na) => {}
                Some(family) if family.takes_length() => {}
                _ => {
                    return Err(EguiError::InvalidParam {
                        what: (*slot).to_owned(),
                        reason: format!(
                            "smoothing {smooth:?} is not a length-taking smoothing family"
                        ),
                    });
                }
            }
        }
        for (slot, gain) in [("fast_k", self.fast_k), ("slow_k", self.slow_k)] {
            if gain.is_some_and(|value| !value.is_finite() || value <= 0.0) {
                return Err(EguiError::InvalidParam {
                    what: slot.to_owned(),
                    reason: format!("leg gain must be positive and finite, got {gain:?}"),
                });
            }
        }
        Ok(())
    }
}

impl IntegrationLiteConfig {
    /// Parse one leg kind, refusing anything outside the normalized
    /// kernel-backed set instead of substituting.
    fn leg_kind(slot: &str, name: &str) -> Result<OscillatorKind, EguiError> {
        match name {
            "rsi" => Ok(OscillatorKind::Rsi),
            "cci" => Ok(OscillatorKind::Cci),
            "cmo" => Ok(OscillatorKind::Cmo),
            "mfi" => Ok(OscillatorKind::Mfi),
            "stoch" => Ok(OscillatorKind::Stoch),
            _ => Err(EguiError::InvalidParam {
                what: slot.to_owned(),
                reason: format!("{name:?} is not a lite leg kind (rsi, cci, cmo, mfi, stoch)"),
            }),
        }
    }

    /// Resolve blank leg dials against the panel kind rows: unset periods
    /// and gains take the kind row, and disarmed leg smoothing follows an
    /// armed kind. Explicit leg values win; the frozen catalog remains the
    /// last resort downstream. Infallible by design: an unknown kind keeps
    /// its blanks so `validate` refuses naming the field.
    #[must_use]
    pub fn resolve_panel_defaults(
        &self,
        panel: &crate::oscillator_panel::OscillatorPanelState,
    ) -> Self {
        let mut out = self.clone();
        for (slot, kind_name, period, smooth_type, smooth_period, gain) in [
            (
                "fast_kind",
                self.fast_kind.as_str(),
                &mut out.fast_period,
                &mut out.fast_smooth_type,
                &mut out.fast_smooth_period,
                &mut out.fast_k,
            ),
            (
                "slow_kind",
                self.slow_kind.as_str(),
                &mut out.slow_period,
                &mut out.slow_smooth_type,
                &mut out.slow_smooth_period,
                &mut out.slow_k,
            ),
        ] {
            let Ok(_kind) = Self::leg_kind(slot, kind_name) else {
                continue;
            };
            let Ok(row) = panel.entry(kind_name) else {
                continue;
            };
            if period.is_none() {
                *period = Some(row.period);
            }
            let slot_armed = matches!(
                SmoothingFamily::parse(smooth_type),
                Some(family) if family != SmoothingFamily::Na && family.takes_length()
            );
            let kind_armed = matches!(
                SmoothingFamily::parse(&row.smooth_type),
                Some(family) if family != SmoothingFamily::Na && family.takes_length()
            );
            if !slot_armed && kind_armed {
                smooth_type.clone_from(&row.smooth_type);
                *smooth_period = Some(row.smooth_period);
            } else if slot_armed && smooth_period.is_none() {
                *smooth_period = Some(row.smooth_period);
            }
            if gain.is_none() {
                *gain = Some(row.gain);
            }
        }
        out
    }
}

impl Default for IntegrationLiteConfig {
    fn default() -> Self {
        Self::new().build()
    }
}

impl Validate for IntegrationLiteConfig {
    type Err = EguiError;
    fn validate(&self) -> Result<(), Self::Err> {
        IntegrationLiteConfig::validate(self)
    }
}

impl WorkUnit for IntegrationLiteConfig {
    fn name(&self) -> &str {
        "integration_lite_config"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        check_for_execute(self, "integration lite config invalid")?;
        Ok(valid_output(format!(
            "integration lite ready (alpha {})",
            self.eit_alpha
        )))
    }
}

impl_component!(IntegrationLiteConfig);

/// Headless IntegrationLite sub-pane: four bounded oscillators, the centered
/// delta-volume histogram, its split confidence, and the swing pivots.
#[derive(Debug, Clone)]
pub struct IntegrationLiteSubPane {
    /// Bounded fast oscillator (RSI arm) on `[-100, +100]`.
    pub fast_bounded: Vec<Option<f64>>,
    /// Bounded slow oscillator (CCI arm) on `[-100, +100]`.
    pub slow_bounded: Vec<Option<f64>>,
    /// Bounded ribbon-health oscillator on `[-100, +100]`.
    pub ribbon_health_bounded: Vec<Option<f64>>,
    /// Bounded MA-health oscillator on `[-100, +100]`.
    pub ma_health_bounded: Vec<Option<f64>>,
    /// Centered delta-volume histogram around the zero axis.
    pub delta_volume: DeltaView,
    /// Lowest confidence any volume split earned (producer confidence, never
    /// a tradeable measurement).
    pub delta_confidence: DeltaConfidence,
    /// Threshold crossings on the bounded fast leg.
    pub pivots: Vec<PivotMark>,
    /// Price-space trend ribbon between the adaptive/signal legs; painted on
    /// the main price pane when [`show_ribbon`](Self::show_ribbon) is set.
    pub ribbon: RibbonView,
    /// Whether the trend ribbon paints over the main pane.
    pub show_ribbon: bool,
}

/// Build the four-readout bounded sub-pane plus the centered delta histogram.
///
/// Reuses the frozen kernel truth: the fast/slow oscillators squashed onto the
/// `[-100, +100]` band, ribbon health from the shared health fold, MA health
/// from [`ma_health_series`], and the shape-estimated delta volume through
/// [`delta_candle_view`]. Pivots fire on the bounded fast leg at the squash of
/// the configured Pine-scale threshold (the default squashes exactly onto the
/// frozen bounded threshold). Refuses a config its own validation refuses.
pub fn integration_lite_sub_pane(
    rows: &[ChartRow],
    config: &IntegrationLiteConfig,
) -> Result<IntegrationLiteSubPane, EguiError> {
    config.validate()?;
    let native = native_lite_series(rows, config.eit_alpha)?;
    let fast_bounded = lite_panel_leg(rows, config, LiteLeg::Fast)?;
    let slow_bounded = lite_panel_leg(rows, config, LiteLeg::Slow)?;
    let (ribbon_health, _vol_ratio) =
        ribbon_health_inputs(rows, config.eit_alpha, config.ribbon_health_k)?;
    let ribbon_health_bounded = squash_series_to_100(&ribbon_health);
    let ma_health_bounded = squash_series_to_100(&ma_health_series(rows, config.ma_health_k)?);
    let (buy, sell, delta_confidence) = delta_splits(rows);
    let delta_volume = delta_candle_view(rows, &buy, &sell, config.delta_unit)?;
    let pivots = pivot_marks(&fast_bounded, squash_to_100(config.pivot_threshold))?;
    let ribbon = ribbon_view(&native.adaptive, &native.signal, config.fill_alpha)?;
    Ok(IntegrationLiteSubPane {
        fast_bounded,
        slow_bounded,
        ribbon_health_bounded,
        ma_health_bounded,
        delta_volume,
        delta_confidence,
        pivots,
        ribbon,
        show_ribbon: config.show_ribbon,
    })
}

/// MA-health oscillator off the price pane, at gain `k`.
///
/// Folds the context's MA bundle — ALMA-8 / VWMA-12 / EMA-21 against ATR-14 —
/// through the shared [`health_mas`] scorer at the frozen [`HEALTH_K`], then
/// scales by `k` (the Pine `context.ma_health * i_ma_health_k` plot). Warmup
/// holes stay holes; output lines up with `rows`.
fn ma_health_series(rows: &[ChartRow], k: f64) -> Result<Vec<Option<f64>>, EguiError> {
    let frame = kernel_preview_frame(rows);
    let alma = kernel_series(
        "alma",
        &ParamSet::new()
            .with_int("length", MA_PRIMARY_LENGTH)
            .with_float("offset", MA_PRIMARY_OFFSET)
            .with_float("sigma", MA_PRIMARY_SIGMA),
        &frame,
    )?;
    let vwma = kernel_series(
        "vwma",
        &ParamSet::new().with_int("length", MA_SIGNAL_LENGTH),
        &frame,
    )?;
    let ema = kernel_series(
        "ema",
        &ParamSet::new().with_int("length", RIBBON_MEAN_LENGTH),
        &frame,
    )?;
    let atr = kernel_series(
        "atr",
        &ParamSet::new().with_int("length", ATR_LENGTH),
        &frame,
    )?;
    Ok((0..rows.len())
        .map(|index| {
            let leg = |series: &[Option<f64>]| series.get(index).and_then(|slot| *slot);
            health_mas(leg(&atr), leg(&alma), leg(&vwma), leg(&ema), HEALTH_K)
                .map(|score| score * k)
        })
        .collect())
}

/// Per-bar delta-volume splits from candle shape: buy share, complementary
/// sell share, and the lowest confidence any split earned.
///
/// Historical OHLCV carries no tick tape, so every shape split reads
/// [`DeltaConfidence::Low`] — producer confidence for the histogram, never a
/// measurement.
fn delta_splits(rows: &[ChartRow]) -> (Vec<Option<f64>>, Vec<Option<f64>>, DeltaConfidence) {
    let mut buy = Vec::with_capacity(rows.len());
    let mut sell = Vec::with_capacity(rows.len());
    let mut confidence = DeltaConfidence::High;
    for row in rows {
        let estimate = estimate_delta_volume_estimated(
            row.open,
            row.high,
            row.low,
            row.close,
            row.volume as f64,
        );
        confidence = lower_confidence(confidence, estimate.confidence);
        buy.push(Some(estimate.buy_volume));
        sell.push(Some(row.volume as f64 - estimate.buy_volume));
    }
    (buy, sell, confidence)
}

/// The lower of two split confidences (`High > Reduced > Low`).
fn lower_confidence(left: DeltaConfidence, right: DeltaConfidence) -> DeltaConfidence {
    if confidence_rank(left) <= confidence_rank(right) {
        left
    } else {
        right
    }
}

/// Ordering rank of a split confidence, `Low` lowest.
fn confidence_rank(confidence: DeltaConfidence) -> u8 {
    match confidence {
        DeltaConfidence::Low => 0,
        DeltaConfidence::Reduced => 1,
        DeltaConfidence::High => 2,
    }
}

#[cfg(test)]
#[path = "../tests/integration_lite.rs"]
mod tests;
