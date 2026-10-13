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

use protopyne_chart_model::ChartRow;
use protopyne_kernels::indicator::{OwnedOhlcv, ParamSet, builtin_registry};

use fluent_egui::EguiError;
use fluent_egui::{PivotMark, RibbonView, pivot_marks, ribbon_view};

use crate::oscillators::{OscillatorKind, normalize_series};

/// Kernel length for the fast/slow raw oscillators (freeze-pinned: the Pine
/// freeze drivers evaluate `normalizeOsc` at length 14 on both arms).
pub const RAW_OSCILLATOR_LENGTH: i64 = 14;
/// EIT smoothing for the ribbon signal leg (script signal preset).
pub const SIGNAL_EIT_ALPHA: f64 = 0.12;
/// Frozen pivot threshold on the normalized band (see the calibration
/// report beside the fixtures for the measured evidence).
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
fn frame_of_rows(rows: &[ChartRow]) -> OwnedOhlcv {
    OwnedOhlcv {
        open: rows.iter().map(|row| row.open).collect(),
        high: rows.iter().map(|row| row.high).collect(),
        low: rows.iter().map(|row| row.low).collect(),
        close: rows.iter().map(|row| row.close).collect(),
        volume: rows.iter().map(|row| row.volume as f64).collect(),
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
    let relayed: Vec<f64> =
        adaptive.iter().map(|slot| slot.unwrap_or(f64::NAN)).collect();
    let synth = OwnedOhlcv {
        open: relayed.clone(),
        high: relayed.clone(),
        low: relayed.clone(),
        close: relayed,
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
