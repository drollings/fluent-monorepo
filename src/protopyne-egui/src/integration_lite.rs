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

use protopyne_chart_model::{ChartRow, DisplayOnly};
use protopyne_kernels::bars::fast_tanh;
use protopyne_kernels::indicator::{OwnedOhlcv, ParamSet, builtin_registry};
use protopyne_kernels::market_context::health_mas;

use fluent_egui::EguiError;
use fluent_egui::{
    PivotMark, RibbonView, ThemeConfig, pivot_marks, resolve_color, ribbon_view,
};

use crate::oscillators::{OscillatorKind, normalize_series};

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

/// Carry a kernel refusal across the crate boundary without a second error type.
fn describe_kernel(error: &protopyne_kernels::KernelError) -> EguiError {
    EguiError::Dependency { reason: error.to_string() }
}

/// Run one kernel batch over a frame, or refuse a missing kernel.
fn kernel_series(
    kernel_id: &str,
    params: &ParamSet,
    frame: &OwnedOhlcv,
) -> Result<Vec<Option<f64>>, EguiError> {
    let registry = builtin_registry().map_err(|e| describe_kernel(&e))?;
    let kernel = registry.get(kernel_id).ok_or_else(|| EguiError::Dependency {
        reason: format!("kernel not registered: {kernel_id}"),
    })?;
    kernel.batch(&frame.views(), params).map(|result| result.values).map_err(|e| describe_kernel(&e))
}

/// Frame of display legs for kernel batch evaluation.
///
/// Display provenance is fenced: every leg enters as [`DisplayOnly`] and is
/// revealed only to feed the kernel preview — the `reveal` call is the
/// auditable escape, and nothing here converts toward a decision type.
fn frame_of_rows(rows: &[ChartRow]) -> OwnedOhlcv {
    let open: Vec<DisplayOnly<f64>> = rows.iter().map(|row| DisplayOnly::new(row.open)).collect();
    let high: Vec<DisplayOnly<f64>> = rows.iter().map(|row| DisplayOnly::new(row.high)).collect();
    let low: Vec<DisplayOnly<f64>> = rows.iter().map(|row| DisplayOnly::new(row.low)).collect();
    let close: Vec<DisplayOnly<f64>> = rows.iter().map(|row| DisplayOnly::new(row.close)).collect();
    let volume: Vec<DisplayOnly<f64>> =
        rows.iter().map(|row| DisplayOnly::new(row.volume as f64)).collect();
    OwnedOhlcv {
        open: open.into_iter().map(DisplayOnly::reveal).collect(),
        high: high.into_iter().map(DisplayOnly::reveal).collect(),
        low: low.into_iter().map(DisplayOnly::reveal).collect(),
        close: close.into_iter().map(DisplayOnly::reveal).collect(),
        volume: volume.into_iter().map(DisplayOnly::reveal).collect(),
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
    let frame = frame_of_rows(rows);
    let length = ParamSet::new().with_int("length", RAW_OSCILLATOR_LENGTH);
    let fast_raw = kernel_series("rsi", &length, &frame)?;
    let slow_raw = kernel_series("cci", &length, &frame)?;
    let adaptive =
        kernel_series("eit", &ParamSet::new().with_float("alpha", eit_alpha), &frame)?;
    // Relayed adaptive legs descend from the fenced frame above, so they
    // stay fenced until the same reveal; the synthetic zero volume is a
    // literal, not display-descended, and needs no fence.
    let relayed: Vec<DisplayOnly<f64>> =
        adaptive.iter().map(|slot| DisplayOnly::new(slot.unwrap_or(f64::NAN))).collect();
    let synth = OwnedOhlcv {
        open: relayed.iter().map(|leg| leg.reveal()).collect(),
        high: relayed.iter().map(|leg| leg.reveal()).collect(),
        low: relayed.iter().map(|leg| leg.reveal()).collect(),
        close: relayed.iter().map(|leg| leg.reveal()).collect(),
        volume: vec![0.0; rows.len()],
    };
    let signal =
        kernel_series("eit", &ParamSet::new().with_float("alpha", SIGNAL_EIT_ALPHA), &synth)?;
    Ok(LiteNative { fast_raw, slow_raw, adaptive, signal })
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
    let fast = normalize_series(&native.fast_raw, OscillatorKind::Rsi);
    let slow = normalize_series(&native.slow_raw, OscillatorKind::Cci);
    let ribbon = ribbon_view(&native.adaptive, &native.signal, fill_alpha)?;
    let pivots = pivot_marks(&fast, threshold)?;
    Ok(IntegrationLiteView { fast, slow, ribbon, pivots, threshold })
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
        .map(|(row, trend)| match (row.close >= row.open, *trend) {
            (true, true) => "bull",
            (false, true) => "bull_down",
            (false, false) => "bear",
            (true, false) => "bear_up",
        })
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
/// Ribbon health gain (reference `i_ribbon_health_k`).
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
        let channel = |key: &str| {
            resolve_color(theme, key).map(|color| [color.r(), color.g(), color.b()])
        };
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
/// pair, an EMA-21 mean relayed through the same synth-frame trick as the
/// signal leg, and [`health_mas`] with a per-bar gain of
/// `vol_ratio * (alpha + 1.8)`, all × 1.8 — while `vol_ratio` mirrors the
/// context fetch (`ta.sma(volume, 20)`, ratio guarded by `sma > 0`, else
/// `0`). Warmup holes stay holes; every output lines up with `rows`.
/// Refuses non-positive or non-finite `eit_alpha`, like the pane build.
pub fn candle_momentum_series(
    rows: &[ChartRow],
    eit_alpha: f64,
) -> Result<CandleMomentum, EguiError> {
    if !eit_alpha.is_finite() || eit_alpha <= 0.0 || eit_alpha > 1.0 {
        return Err(EguiError::InvalidParam {
            what: "eit_alpha".to_owned(),
            reason: "adaptive smoothing must sit within (0, 1]".to_owned(),
        });
    }
    let native = native_lite_series(rows, eit_alpha)?;
    let relayed: Vec<f64> =
        native.adaptive.iter().map(|slot| slot.unwrap_or(f64::NAN)).collect();
    let synth = OwnedOhlcv {
        open: relayed.clone(),
        high: relayed.clone(),
        low: relayed.clone(),
        close: relayed,
        volume: vec![0.0; rows.len()],
    };
    let mean = kernel_series(
        "ema",
        &ParamSet::new().with_int("length", RIBBON_MEAN_LENGTH),
        &synth,
    )?;
    let frame = frame_of_rows(rows);
    let atr = kernel_series("atr", &ParamSet::new().with_int("length", ATR_LENGTH), &frame)?;
    let volumes: Vec<f64> = rows.iter().map(|row| row.volume as f64).collect();
    let volume_synth = OwnedOhlcv {
        open: volumes.clone(),
        high: volumes.clone(),
        low: volumes.clone(),
        close: volumes,
        volume: vec![0.0; rows.len()],
    };
    let vol_sma = kernel_series(
        "sma",
        &ParamSet::new().with_int("length", VOL_SMA_LENGTH),
        &volume_synth,
    )?;
    let vol_ratio: Vec<f64> = rows
        .iter()
        .zip(vol_sma.iter())
        .map(|(row, sma)| match sma {
            Some(sma) if *sma > 0.0 => {
                let ratio = row.volume as f64 / sma;
                if ratio.is_finite() { ratio } else { 0.0 }
            }
            _ => 0.0,
        })
        .collect();
    let health: Vec<Option<f64>> = (0..rows.len())
        .map(|index| {
            let gain = vol_ratio[index] * (eit_alpha + RIBBON_HEALTH_K);
            let leg = |series: &[Option<f64>]| series.get(index).and_then(|slot| *slot);
            health_mas(leg(&atr), leg(&native.adaptive), leg(&native.signal), leg(&mean), gain)
                .map(|score| score * RIBBON_HEALTH_K)
        })
        .collect();
    Ok(CandleMomentum { health, vol_ratio })
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
            fast: normalize_series(&native.fast_raw, OscillatorKind::Rsi),
            slow: normalize_series(&native.slow_raw, OscillatorKind::Cci),
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
        FreezePair { max_abs_diff, compared }
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
    let precision =
        if got.is_empty() { f64::from(expected.is_empty()) } else { hits as f64 / got.len() as f64 };
    let recall =
        if expected.is_empty() { 1.0 } else { hits as f64 / expected.len() as f64 };
    Ok((precision, recall))
}

#[cfg(test)]
#[path = "../tests/integration_lite.rs"]
mod tests;
