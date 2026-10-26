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
use protopyne_kernels::smoothing::{SmoothingArgs, SmoothingFamily, MA_PERIOD_MAX, MA_PERIOD_MIN};

use crate::topology::{describe_kernel, kernel_preview_frame, kernel_series, source_frame};
use protopyne_kernels::price_source::PriceSource;

use fluent_wvr::validate::{check_for_execute, valid_output, Validate};

use fluent_egui::EguiError;
use fluent_egui::{
    delta_candle_view, pivot_marks, resolve_color, ribbon_view, DeltaView, PivotMark, RibbonView,
    ThemeConfig,
};

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
///
/// The leg frame follows the source dials: the adaptive EIT relay when
/// Adaptive is armed, the selected price-source column otherwise, and the
/// legacy close frame when the source is Close (byte-identical to the
/// pre-source legs). Validation already refused resynthesized inputs on
/// multi-leg arms, so every frame here feeds a single-leg kernel.
fn lite_panel_leg(
    frame: &OwnedOhlcv,
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
    let registry =
        protopyne_kernels::indicator::shared_registry().map_err(|error| describe_kernel(&error))?;
    let arm = protopyne_kernels::lite::arm_leg(
        registry,
        frame,
        &row_volumes(rows),
        kind.id(),
        period.unwrap_or(RAW_OSCILLATOR_LENGTH),
        smooth_type,
        smooth_period.unwrap_or(3),
        gain.unwrap_or(1.0),
        smooth_slot,
    )
    .map_err(|error| describe_kernel(&error))?;
    // Display convention: the arm's leading holes and its smoothing warmup
    // are masked before the bounded compression, so a partial reading never
    // paints as a calibrated value.
    let holes = arm.raw.iter().take_while(|slot| slot.is_none()).count();
    let warmup = match SmoothingFamily::parse(smooth_type) {
        Some(SmoothingFamily::Na) => 0,
        Some(family) => family
            .warmup_for_length(smooth_period.unwrap_or(3))
            .map_err(|error| EguiError::InvalidParam {
                what: smooth_slot.to_owned(),
                reason: error.to_string(),
            })?,
        None => {
            return Err(EguiError::InvalidParam {
                what: smooth_slot.to_owned(),
                reason: format!("unknown smoothing family {smooth_type:?}"),
            });
        }
    };
    let mask = holes.saturating_add(warmup);
    let mut shaped = arm.shaped;
    let len = shaped.len();
    for slot in shaped.iter_mut().take(mask.min(len)) {
        *slot = None;
    }
    Ok(shaped
        .into_iter()
        .map(|slot| {
            slot.and_then(|value| {
                let shaped = squash_to_100(value);
                shaped.is_finite().then_some(shaped)
            })
        })
        .collect())
}

/// Fast arm of the lite pane (RSI).
const LITE_FAST_KIND: OscillatorKind = OscillatorKind::Rsi;
/// Slow arm of the lite pane (CCI).
const LITE_SLOW_KIND: OscillatorKind = OscillatorKind::Cci;

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

/// One synthetic frame from a display series: every OHLC leg carries the
/// value (holes as `NaN`), volume carries the real bar volume so
/// volume-reading smoothers (VWMA) see the tape instead of a silent zero.
/// The values enter through the [`DisplayOnly`] fence and are revealed
/// exactly here — the auditable escape for a display-descended preview
/// frame. This is the single construction the pane and the momentum inputs
/// share, so the fence is named once.
fn synth_close_frame(values: &[Option<f64>], volumes: &[f64]) -> OwnedOhlcv {
    let revealed: Vec<f64> = values
        .iter()
        .map(|slot| DisplayOnly::new(nan_hole(*slot)).reveal())
        .collect();
    let volume: Vec<f64> = values
        .iter()
        .enumerate()
        .map(|(index, _)| volumes.get(index).copied().unwrap_or(f64::NAN))
        .collect();
    OwnedOhlcv {
        open: revealed.clone(),
        high: revealed.clone(),
        low: revealed.clone(),
        close: revealed,
        volume,
    }
}

/// Real bar volumes for a relayed frame, index-aligned with `rows`.
fn row_volumes(rows: &[ChartRow]) -> Vec<f64> {
    rows.iter().map(|row| row.volume as f64).collect()
}

/// The frozen pane dials, expressed through the shared leg batch.
///
/// RSI/CCI at the freeze length, no smoothing, unit gain, EIT-0.12 signal
/// leg and EMA-21 mean: the values the Pine freeze drivers pinned.
fn frozen_lite_params(eit_alpha: f64) -> protopyne_kernels::lite::LiteParams {
    protopyne_kernels::lite::LiteParams {
        fast_kernel: "rsi".to_owned(),
        fast_length: RAW_OSCILLATOR_LENGTH,
        fast_smooth: "na".to_owned(),
        fast_smooth_length: 3,
        fast_gain: 1.0,
        slow_kernel: "cci".to_owned(),
        slow_length: RAW_OSCILLATOR_LENGTH,
        slow_smooth: "na".to_owned(),
        slow_smooth_length: 3,
        slow_gain: 1.0,
        eit_alpha,
        signal_type: "eit".to_owned(),
        signal_length: RAW_OSCILLATOR_LENGTH,
        signal_param1: SIGNAL_EIT_ALPHA,
        mean_type: "ema".to_owned(),
        mean_length: RIBBON_MEAN_LENGTH,
        mean_param1: 0.06,
        ribbon_health_k: RIBBON_HEALTH_K,
        price_source: "close".to_owned(),
        adaptive: false,
        session_filter: false,
        session: "0930-1600".to_owned(),
    }
}

/// Evaluate the frozen pane series over converted rows through the one
/// market-leg batch.
pub fn lite_native_series(rows: &[ChartRow], eit_alpha: f64) -> Result<LiteNative, EguiError> {
    let frame = kernel_preview_frame(rows);
    let closes = frame.close.clone();
    let params = frozen_lite_params(eit_alpha);
    let legs = protopyne_kernels::lite::lite_legs_from_frame(
        &frame,
        &closes,
        &params,
        protopyne_kernels::lite::ATR_LENGTH,
    )
    .map_err(|error| describe_kernel(&error))?;
    Ok(LiteNative {
        fast_raw: legs.fast_raw,
        slow_raw: legs.slow_raw,
        adaptive: legs.adaptive,
        signal: legs.signal,
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
    let native = lite_native_series(rows, eit_alpha)?;
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

/// EMA length of the ribbon mean leg, from the shared context bundle
/// (reference `i_ribbon_mean_period`; the `EMA` factory branch ignores
/// `param1`, so plain length-21 EMA).
pub const RIBBON_MEAN_LENGTH: i64 = protopyne_kernels::lite::MA_MEAN_LENGTH;
/// ATR length behind the health distances, from the shared context.
pub const ATR_LENGTH: i64 = protopyne_kernels::lite::ATR_LENGTH;
/// Ribbon health gain (reference `i_ribbon_health_k`; task value: display
/// fitness of the health fold, never producer confidence).
pub const RIBBON_HEALTH_K: f64 = protopyne_kernels::lite::RIBBON_HEALTH_K;

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
    // The tint path predates the dialed ribbon legs and stays on the
    // reference pair (EIT-0.12 signal, EMA-21 mean) until it reads
    // configuration: tint values must not move under an unrelated dial.
    let native = lite_native_series(rows, eit_alpha)?;
    let volumes = row_volumes(rows);
    let mean = kernel_series(
        "ema",
        &ParamSet::new().with_int("length", RIBBON_MEAN_LENGTH),
        &synth_close_frame(&native.adaptive, &volumes),
    )?;
    let (health, vol_ratio) =
        ribbon_health_inputs(rows, eit_alpha, RIBBON_HEALTH_K, &native.signal, &mean)?;
    Ok(CandleMomentum { health, vol_ratio })
}

/// One dialed ribbon leg over the relayed adaptive series: the configured
/// smoothing family evaluated with the configured window and `param1`
/// (EIT alpha, ALMA offset). Validation already ran the family contract,
/// so this only maps the kernel refusal onto the dial for direct callers.
fn ribbon_smoothing_leg(
    adaptive: &[Option<f64>],
    volumes: &[f64],
    family_id: &str,
    length: i64,
    param1: f64,
    slot: &str,
) -> Result<Vec<Option<f64>>, EguiError> {
    let family = SmoothingFamily::parse(family_id).ok_or_else(|| EguiError::InvalidParam {
        what: format!("{slot}_type"),
        reason: format!("unknown smoothing family {family_id:?}"),
    })?;
    let args = SmoothingArgs::length(length).with_n1(param1);
    family
        .evaluate(&synth_close_frame(adaptive, volumes), &args)
        .map_err(|error| describe_kernel(&error))
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
    signal: &[Option<f64>],
    mean: &[Option<f64>],
) -> Result<(Vec<Option<f64>>, Vec<f64>), EguiError> {
    if !eit_alpha.is_finite() || eit_alpha <= 0.0 || eit_alpha > 1.0 {
        return Err(EguiError::InvalidParam {
            what: "eit_alpha".to_owned(),
            reason: "adaptive smoothing must sit within (0, 1]".to_owned(),
        });
    }
    for (slot, series) in [("signal", signal), ("mean", mean)] {
        if series.len() != rows.len() {
            return Err(EguiError::InvalidParam {
                what: (*slot).to_owned(),
                reason: format!("{slot} leg must line up with rows"),
            });
        }
    }
    let native = lite_native_series(rows, eit_alpha)?;
    let frame = kernel_preview_frame(rows);
    let context =
        protopyne_kernels::lite::context_legs(&frame).map_err(|error| describe_kernel(&error))?;
    let health = protopyne_kernels::lite::ribbon_health(
        &context.atr,
        &native.adaptive,
        signal,
        mean,
        &context.vol_ratio,
        eit_alpha,
        health_k,
    )
    .into_iter()
    .map(|slot| slot.map(|score| score * health_k))
    .collect();
    Ok((health, context.vol_ratio))
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
pub const MA_HEALTH_K: f64 = protopyne_kernels::lite::MA_HEALTH_K;

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

fn default_delta_bull() -> String {
    "#00ff00".to_owned()
}

fn default_delta_bear() -> String {
    "#ff0000".to_owned()
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

fn default_ribbon_alpha_low() -> f64 {
    0.14
}

fn default_ribbon_alpha_high() -> f64 {
    0.8
}

fn default_ribbon_bull() -> String {
    "#00bcd4".to_owned()
}

fn default_ribbon_bear() -> String {
    "#7b1fa2".to_owned()
}

fn default_pivots_lookback() -> i64 {
    8
}

fn default_show() -> bool {
    true
}

fn default_brightness() -> i64 {
    30
}

fn default_source() -> String {
    "close".to_owned()
}

/// Pine `UTIL.Timeframes` members in native lowercase spelling: the full
/// chart/multi-timeframe vocabulary the LTF/fetch/HTF and MTF dials read.
/// Display-only until the multi-timeframe milestone wires them.
#[must_use]
pub fn lite_timeframe_ids() -> Vec<String> {
    [
        "c", "s5", "s10", "s15", "s30", "m1", "m3", "m5", "m10", "m15", "m30", "h1", "h2", "h4",
        "d1", "w1", "mo1", "mo3", "y", "na",
    ]
    .iter()
    .map(|id| (*id).to_owned())
    .collect()
}

/// Pine `UTIL.Positions` members in native lowercase spelling.
#[must_use]
pub fn lite_position_ids() -> Vec<String> {
    [
        "top_left",
        "top_center",
        "top_right",
        "middle_left",
        "middle_center",
        "middle_right",
        "bottom_left",
        "bottom_center",
        "bottom_right",
        "top_left_pane",
        "top_right_pane",
        "bottom_left_pane",
    ]
    .iter()
    .map(|id| (*id).to_owned())
    .collect()
}

/// Pine `UTIL.Sizes` members in native lowercase spelling.
#[must_use]
pub fn lite_size_ids() -> Vec<String> {
    ["auto", "tiny", "small", "normal", "large", "huge"]
        .iter()
        .map(|id| (*id).to_owned())
        .collect()
}

fn default_signal_type() -> String {
    "eit".to_owned()
}

fn default_signal_period() -> i64 {
    10
}

fn default_signal_param1() -> f64 {
    0.12
}

fn default_mean_type() -> String {
    "ema".to_owned()
}

fn default_mean_period() -> i64 {
    21
}

fn default_mean_param1() -> f64 {
    0.06
}

fn default_ltf_timeframe() -> String {
    "m1".to_owned()
}

fn default_fetch_timeframe() -> String {
    "c".to_owned()
}

fn default_htf_timeframe() -> String {
    "w1".to_owned()
}

fn default_session() -> String {
    "0930-1600".to_owned()
}

fn default_mtf_location() -> String {
    "middle_left".to_owned()
}

fn default_mtf_size() -> String {
    "small".to_owned()
}

fn default_mtf_text() -> String {
    "#dbdbdb".to_owned()
}

fn default_mtf_background() -> String {
    "#000000".to_owned()
}

fn default_mtf_timeframe_1() -> String {
    "m5".to_owned()
}

fn default_mtf_timeframe_2() -> String {
    "m15".to_owned()
}

fn default_mtf_timeframe_3() -> String {
    "h1".to_owned()
}

fn default_mtf_timeframe_4() -> String {
    "h4".to_owned()
}

/// One IntegrationLite indicator's configuration: the single schema-driven
/// control truth behind its settings pane.
///
/// Every field is a `Component` field, so the settings modal renders from the
/// `describe()` schema and a new knob appears without a UI edit. Tabs ride
/// explicit `x-tab` pins on the show flags (the name heuristic only
/// recognizes leading `show_`), groups ride `x-group` (the Pine section),
/// tooltips ride `x-tooltip`, and drag steps ride `x-step`. `eit_alpha` and
/// `fill_alpha` route to the Style tab by name; paint-only toggles never
/// move the evaluation, only the emitter.
#[derive(
    Debug, Clone, bon::Builder, serde::Serialize, serde::Deserialize, FieldAccess, Describable,
)]
#[builder(start_fn = new)]
#[allow(clippy::struct_excessive_bools)]
pub struct IntegrationLiteConfig {
    /// EIT adaptive smoothing of the ribbon source.
    /// Deprecated alias: old documents carry this single value; new
    /// documents carry the adaptive span (`ribbon_alpha_low`/`high`) whose
    /// midpoint this reproduces. Evaluation still reads this field until
    /// per-bar adaptive alpha lands with the multi-timeframe tape.
    #[field(
        desc = "EIT adaptive smoothing (deprecated alias of the alpha span midpoint)",
        min = 0.03,
        max = 1.0
    )]
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
    #[field(
        desc = "Delta volume height at average volume",
        min = 0.0,
        group = "Delta Volume Candles",
        tooltip = "Oscillator distance from zero when delta volume equals the volume average"
    )]
    #[builder(default = default_delta_unit())]
    pub delta_unit: f64,
    /// Whether the delta-volume histogram paints (script `display` switch).
    #[field(
        desc = "Show delta candles",
        tab = "visibility",
        group = "Delta Volume Candles",
        inline = "adaptive_eit",
        tooltip = "Paint the buy/sell delta histogram around the zero axis"
    )]
    #[builder(default = default_show())]
    #[serde(default = "default_show")]
    pub delta_show: bool,
    /// Buy-side delta candle color (script bull volume color, transparency
    /// 60). Paint-only; the split confidence still rides `delta_confidence`.
    #[field(
        desc = "Buy volume color",
        format = "color",
        group = "Delta Volume Candles",
        inline = "adaptive_eit",
        tooltip = "Bull-side delta histogram color"
    )]
    #[builder(default = default_delta_bull())]
    #[serde(default = "default_delta_bull")]
    pub delta_bull: String,
    /// Sell-side delta candle color (script bear volume color,
    /// transparency 60). Paint-only.
    #[field(
        desc = "Sell volume color",
        format = "color",
        group = "Delta Volume Candles",
        inline = "adaptive_eit",
        tooltip = "Bear-side delta histogram color"
    )]
    #[builder(default = default_delta_bear())]
    #[serde(default = "default_delta_bear")]
    pub delta_bear: String,
    /// Pivot threshold in Pine-scale (unbounded) units: the bounded fast leg
    /// fires at the squash of this value. Defaults to the frozen task value;
    /// the script input defaults higher on its gained leg, which would miss
    /// the frozen calibration windows, so the port keeps the frozen default
    /// and offers the script value as a reachable dial stop.
    #[field(desc = "Pivot threshold (Pine-scale)", min = 20.0)]
    #[builder(default = default_pivot_threshold())]
    pub pivot_threshold: f64,
    /// Whether the trend ribbon paints.
    #[field(
        desc = "Show the trend ribbon",
        tab = "visibility",
        group = "Trend Ribbon",
        inline = "adaptive_eit",
        tooltip = "Fill the band between the adaptive and signal legs"
    )]
    #[builder(default = default_show_ribbon())]
    pub show_ribbon: bool,
    /// Adaptive alpha span low end (script `Low`): the per-bar alpha
    /// interpolates from here toward `ribbon_alpha_high` once the
    /// multi-timeframe tape lands; until then stored, validated, and
    /// rendered, never read by a decision.
    #[field(
        desc = "Adaptive alpha low",
        min = 0.03,
        step = 0.02,
        group = "Trend Ribbon",
        inline = "adaptive_eit",
        tooltip = "Low end of the adaptive alpha span"
    )]
    #[builder(default = default_ribbon_alpha_low())]
    #[serde(default = "default_ribbon_alpha_low")]
    pub ribbon_alpha_low: f64,
    /// Adaptive alpha span high end (script `High`): display-only until
    /// per-bar adaptive alpha lands with the multi-timeframe tape.
    #[field(
        desc = "Adaptive alpha high",
        min = 0.1,
        step = 0.05,
        group = "Trend Ribbon",
        inline = "adaptive_eit",
        tooltip = "High end of the adaptive alpha span"
    )]
    #[builder(default = default_ribbon_alpha_high())]
    #[serde(default = "default_ribbon_alpha_high")]
    pub ribbon_alpha_high: f64,
    /// Ribbon bull color (script bull ribbon color at transparency 60).
    /// Paint-only; the band math never reads it.
    #[field(
        desc = "Ribbon bull color",
        format = "color",
        tab = "style",
        group = "Trend Ribbon",
        inline = "adaptive_eit",
        tooltip = "Bull-side ribbon fill color"
    )]
    #[builder(default = default_ribbon_bull())]
    #[serde(default = "default_ribbon_bull")]
    pub ribbon_bull: String,
    /// Ribbon bear color (script bear ribbon color at transparency 60).
    /// Paint-only.
    #[field(
        desc = "Ribbon bear color",
        format = "color",
        tab = "style",
        group = "Trend Ribbon",
        inline = "adaptive_eit",
        tooltip = "Bear-side ribbon fill color"
    )]
    #[builder(default = default_ribbon_bear())]
    #[serde(default = "default_ribbon_bear")]
    pub ribbon_bear: String,
    /// Whether the ribbon-health oscillator paints on the sub-pane.
    #[field(
        desc = "Ribbon health oscillator",
        tab = "visibility",
        group = "Trend Ribbon",
        inline = "osc1",
        tooltip = "Health of the ribbon legs, gained by volume conviction"
    )]
    #[builder(default = default_show())]
    #[serde(default = "default_show")]
    pub ribbon_health_show: bool,
    /// Whether the MA-health oscillator paints on the sub-pane.
    #[field(
        desc = "MA health oscillator",
        tab = "visibility",
        group = "Trend Ribbon",
        inline = "osc2",
        tooltip = "Health of the moving-average bundle, gained for display"
    )]
    #[builder(default = default_show())]
    #[serde(default = "default_show")]
    pub ma_health_show: bool,
    /// Whether pivot dots paint at threshold crossings of the fast leg.
    #[field(
        desc = "Pivots",
        tab = "visibility",
        group = "Trend Ribbon",
        inline = "pivots",
        tooltip = "Mark trend-exhaustion pivots where the fast leg crosses the threshold"
    )]
    #[builder(default = default_show())]
    #[serde(default = "default_show")]
    pub pivots_show: bool,
    /// Pivot confirmation lookback in bars (script `Lookback`): stored,
    /// validated, and rendered; the extreme-labeling detector that reads
    /// it lands with the exact-arms follow-up, so evaluation ignores it
    /// until then.
    #[field(
        desc = "Pivot lookback (bars)",
        min = 2,
        max = 40,
        step = 1,
        group = "Trend Ribbon",
        inline = "pivots",
        tooltip = "Bars confirming a pivot extreme"
    )]
    #[builder(default = default_pivots_lookback())]
    #[serde(default = "default_pivots_lookback")]
    pub pivots_lookback: i64,
    /// Ribbon signal-leg smoothing family (script Signal row; EIT default
    /// reads `signal_param1` as alpha, length-taking families read
    /// `signal_period`).
    #[field(
        desc = "Signal smoothing",
        group = "Trend Ribbon",
        inline = "signal",
        tooltip = "Smoothing family of the ribbon signal leg",
        variants = "protopyne_kernels::smoothing::smoothing_ids"
    )]
    #[builder(default = default_signal_type())]
    #[serde(default = "default_signal_type")]
    pub signal_type: String,
    /// Ribbon signal-leg window in bars (ignored by alpha-only families,
    /// like the script).
    #[field(
        desc = "Signal period",
        min = 2,
        max = 200,
        step = 2,
        group = "Trend Ribbon",
        inline = "signal",
        tooltip = "Window of the ribbon signal leg (script window 4 to 200)"
    )]
    #[builder(default = default_signal_period())]
    #[serde(default = "default_signal_period")]
    pub signal_period: i64,
    /// Ribbon signal-leg parameter (EIT alpha / ALMA offset, like the
    /// script `param1` slot; ignored by length-only families).
    #[field(
        desc = "Signal parameter",
        min = 0.0,
        step = 0.2,
        group = "Trend Ribbon",
        inline = "signal",
        tooltip = "EIT alpha or ALMA offset of the signal leg"
    )]
    #[builder(default = default_signal_param1())]
    #[serde(default = "default_signal_param1")]
    pub signal_param1: f64,
    /// Ribbon mean-leg smoothing family (script Mean row; EMA default
    /// reads `mean_period`).
    #[field(
        desc = "Mean smoothing",
        group = "Trend Ribbon",
        inline = "mean",
        tooltip = "Smoothing family of the ribbon mean leg",
        variants = "protopyne_kernels::smoothing::smoothing_ids"
    )]
    #[builder(default = default_mean_type())]
    #[serde(default = "default_mean_type")]
    pub mean_type: String,
    /// Ribbon mean-leg window in bars.
    #[field(
        desc = "Mean period",
        min = 2,
        max = 200,
        step = 4,
        group = "Trend Ribbon",
        inline = "mean",
        tooltip = "Window of the ribbon mean leg (script window 8 to 200)"
    )]
    #[builder(default = default_mean_period())]
    #[serde(default = "default_mean_period")]
    pub mean_period: i64,
    /// Ribbon mean-leg parameter (EIT alpha / ALMA offset, like the script
    /// `param1` slot; ignored by length-only families).
    #[field(
        desc = "Mean parameter",
        min = 0.0,
        step = 0.2,
        group = "Trend Ribbon",
        inline = "mean",
        tooltip = "EIT alpha or ALMA offset of the mean leg"
    )]
    #[builder(default = default_mean_param1())]
    #[serde(default = "default_mean_param1")]
    pub mean_param1: f64,
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
    /// Whether the fast/slow oscillator legs paint on the sub-pane.
    #[field(
        desc = "Show the oscillator legs",
        tab = "visibility",
        group = "Oscillator 1 Settings",
        inline = "osc1",
        tooltip = "Paint the fast and slow oscillator legs"
    )]
    #[builder(default = default_show())]
    #[serde(default = "default_show")]
    pub osc_show: bool,
    /// Oscillator leg brightness: the script dims both legs by
    /// `90 - brightness` transparency. Paint-only; values never move.
    #[field(
        desc = "Oscillator brightness",
        min = 0,
        max = 100,
        step = 10,
        group = "Oscillator 1 Settings",
        inline = "osc1",
        tooltip = "Control oscillator brightness"
    )]
    #[builder(default = default_brightness())]
    #[serde(default = "default_brightness")]
    pub brightness: i64,
    /// Follow the adaptive EIT source instead of the price source, like the
    /// script default. Native default stays off: the per-bar adaptive alpha
    /// needs lower-timeframe bars the history feed does not carry, so the
    /// relay reproduces the script shape only once multi-timeframe alpha
    /// lands; until then the close-frame legs stay the default truth.
    #[field(
        desc = "Adaptive source",
        tab = "visibility",
        group = "Oscillator 1 Settings",
        inline = "osc1",
        tooltip = "Read the adaptive EIT series instead of the price source"
    )]
    #[builder(default = false)]
    #[serde(default)]
    pub adaptive: bool,
    /// Price source the oscillator arms read when Adaptive is off.
    #[field(
        desc = "Source",
        group = "Oscillator 1 Settings",
        tooltip = "OHLC column the oscillator arms read when Adaptive is off",
        variants = "crate::topology::price_source_ids"
    )]
    #[builder(default = default_source())]
    #[serde(default = "default_source")]
    pub price_source: String,
    /// Whether candles tint by momentum (script `barcolor` switch). Off
    /// keeps the discrete trend keys; the gradient never gates a decision.
    #[field(
        desc = "Color code candles",
        tab = "visibility",
        group = "Colors",
        inline = "colorcode",
        tooltip = "Tint candles by momentum and volume conviction"
    )]
    #[builder(default = default_show())]
    #[serde(default = "default_show")]
    pub color_code: bool,
    /// Lower-timeframe bar source for the adaptive alpha (script LTF
    /// Timeframe). Display-only until the multi-timeframe milestone wires
    /// it: stored, validated, and rendered, never read by a decision.
    #[field(
        desc = "LTF timeframe",
        group = "Configuration",
        tooltip = "Lower timeframe feeding the adaptive alpha",
        variants = "crate::integration_lite::lite_timeframe_ids"
    )]
    #[builder(default = default_ltf_timeframe())]
    #[serde(default = "default_ltf_timeframe")]
    pub ltf_timeframe: String,
    /// Fetch timeframe: the chart timeframe the legs evaluate on.
    /// Display-only until the multi-timeframe milestone wires it.
    #[field(
        desc = "Fetch timeframe",
        group = "Configuration",
        tooltip = "Timeframe the indicator legs evaluate on",
        variants = "crate::integration_lite::lite_timeframe_ids"
    )]
    #[builder(default = default_fetch_timeframe())]
    #[serde(default = "default_fetch_timeframe")]
    pub fetch_timeframe: String,
    /// Higher-timeframe context for the adaptive alpha (script HTF
    /// Timeframe). Display-only until the multi-timeframe milestone wires
    /// it.
    #[field(
        desc = "HTF timeframe",
        group = "Configuration",
        tooltip = "Higher timeframe feeding the adaptive alpha",
        variants = "crate::integration_lite::lite_timeframe_ids"
    )]
    #[builder(default = default_htf_timeframe())]
    #[serde(default = "default_htf_timeframe")]
    pub htf_timeframe: String,
    /// Whether bars outside the trading session are filtered.
    #[field(
        desc = "Enable session filter",
        group = "Session Filter",
        tooltip = "Skip bars outside the trading session"
    )]
    #[builder(default = default_show())]
    #[serde(default = "default_show")]
    pub session_filter: bool,
    /// Trading session in `HHMM-HHMM` form (script session input).
    /// Display-only until the session gate lands with the native signal.
    #[field(
        desc = "Trading session",
        group = "Session Filter",
        tooltip = "Session window in HHMM-HHMM form"
    )]
    #[builder(default = default_session())]
    #[serde(default = "default_session")]
    pub session: String,
    /// Whether the multi-timeframe table paints.
    #[field(
        desc = "Multi-timeframe table",
        tab = "visibility",
        group = "Multi-Timeframe Table",
        inline = "mtftable",
        tooltip = "Paint the multi-timeframe trend table"
    )]
    #[builder(default = default_show())]
    #[serde(default = "default_show")]
    pub mtf_table_show: bool,
    /// Multi-timeframe table location.
    #[field(
        desc = "Location",
        group = "Multi-Timeframe Table",
        inline = "mtftable",
        tooltip = "Table location",
        variants = "crate::integration_lite::lite_position_ids"
    )]
    #[builder(default = default_mtf_location())]
    #[serde(default = "default_mtf_location")]
    pub mtf_table_position: String,
    /// Multi-timeframe table size.
    #[field(
        desc = "Size",
        group = "Multi-Timeframe Table",
        inline = "mtftable",
        tooltip = "Table size",
        variants = "crate::integration_lite::lite_size_ids"
    )]
    #[builder(default = default_mtf_size())]
    #[serde(default = "default_mtf_size")]
    pub mtf_table_size: String,
    /// Multi-timeframe table foreground color.
    #[field(
        desc = "Foreground",
        format = "color",
        group = "Multi-Timeframe Table",
        inline = "mtftable",
        tooltip = "Table foreground color"
    )]
    #[builder(default = default_mtf_text())]
    #[serde(default = "default_mtf_text")]
    pub mtf_table_text: String,
    /// Multi-timeframe table background color.
    #[field(
        desc = "Background",
        format = "color",
        group = "Multi-Timeframe Table",
        inline = "mtftable",
        tooltip = "Table background color"
    )]
    #[builder(default = default_mtf_background())]
    #[serde(default = "default_mtf_background")]
    pub mtf_table_bg: String,
    /// First multi-timeframe selection (script Timeframe 1).
    #[field(
        desc = "Timeframe 1",
        group = "Multi-Timeframe Selection",
        tooltip = "First compared timeframe",
        variants = "crate::integration_lite::lite_timeframe_ids"
    )]
    #[builder(default = default_mtf_timeframe_1())]
    #[serde(default = "default_mtf_timeframe_1")]
    pub mtf_timeframe_1: String,
    /// Second multi-timeframe selection (script Timeframe 2).
    #[field(
        desc = "Timeframe 2",
        group = "Multi-Timeframe Selection",
        tooltip = "Second compared timeframe",
        variants = "crate::integration_lite::lite_timeframe_ids"
    )]
    #[builder(default = default_mtf_timeframe_2())]
    #[serde(default = "default_mtf_timeframe_2")]
    pub mtf_timeframe_2: String,
    /// Third multi-timeframe selection (script Timeframe 3).
    #[field(
        desc = "Timeframe 3",
        group = "Multi-Timeframe Selection",
        tooltip = "Third compared timeframe",
        variants = "crate::integration_lite::lite_timeframe_ids"
    )]
    #[builder(default = default_mtf_timeframe_3())]
    #[serde(default = "default_mtf_timeframe_3")]
    pub mtf_timeframe_3: String,
    /// Fourth multi-timeframe selection (script Timeframe 4).
    #[field(
        desc = "Timeframe 4",
        group = "Multi-Timeframe Selection",
        tooltip = "Fourth compared timeframe",
        variants = "crate::integration_lite::lite_timeframe_ids"
    )]
    #[builder(default = default_mtf_timeframe_4())]
    #[serde(default = "default_mtf_timeframe_4")]
    pub mtf_timeframe_4: String,
}

/// Paint-only dials: fields that never reach an evaluated value. They ride
/// the settings pane and the painters, never a series, so the plan
/// fingerprint — which must move exactly when the evaluations would —
/// folds every field but these.
const PAINT_ONLY_FIELDS: &[&str] = &[
    "delta_show",
    "ribbon_health_show",
    "ma_health_show",
    "pivots_show",
    "osc_show",
    "brightness",
    "color_code",
    "show_ribbon",
    "ribbon_bull",
    "ribbon_bear",
];

impl IntegrationLiteConfig {
    /// Cache identity over the evaluated values only: the whole config
    /// serialized minus the paint-only dials, hashed through the shared
    /// framer. A brightness or visibility edit keeps the key (values
    /// identical, no recompute); any dial that moves a series moves the key.
    #[must_use]
    pub fn evaluation_key(&self) -> u64 {
        let mut value =
            serde_json::to_value(self).expect("the config serializes its own schema fields");
        if let Some(object) = value.as_object_mut() {
            for key in PAINT_ONLY_FIELDS {
                object.remove(*key);
            }
        }
        common_core::hash::fingerprint_json(&value)
    }

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
        for (slot, color) in [
            ("delta_bull", self.delta_bull.as_str()),
            ("delta_bear", self.delta_bear.as_str()),
        ] {
            fluent_egui::parse_hex_color(slot, color).map_err(|error| EguiError::InvalidParam {
                what: (*slot).to_owned(),
                reason: error.to_string(),
            })?;
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
        for (slot, value, min) in [
            ("ribbon_alpha_low", self.ribbon_alpha_low, 0.03),
            ("ribbon_alpha_high", self.ribbon_alpha_high, 0.1),
        ] {
            if !value.is_finite() || value <= 0.0 || value > 1.0 || value < min {
                return Err(EguiError::InvalidParam {
                    what: (*slot).to_owned(),
                    reason: format!(
                        "adaptive alpha span end must sit within [{min}, 1], got {value}"
                    ),
                });
            }
        }
        if self.ribbon_alpha_low > self.ribbon_alpha_high {
            return Err(EguiError::InvalidParam {
                what: "ribbon_alpha_high".to_owned(),
                reason: format!(
                    "alpha span is inverted: low {} exceeds high {}",
                    self.ribbon_alpha_low, self.ribbon_alpha_high
                ),
            });
        }
        for (slot, color) in [
            ("ribbon_bull", self.ribbon_bull.as_str()),
            ("ribbon_bear", self.ribbon_bear.as_str()),
        ] {
            fluent_egui::parse_hex_color(slot, color).map_err(|error| EguiError::InvalidParam {
                what: (*slot).to_owned(),
                reason: error.to_string(),
            })?;
        }
        if !(2..=40).contains(&self.pivots_lookback) {
            return Err(EguiError::InvalidParam {
                what: "pivots_lookback".to_owned(),
                reason: format!(
                    "pivot lookback must sit within [2, 40], got {}",
                    self.pivots_lookback
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
        if !(0..=100).contains(&self.brightness) {
            return Err(EguiError::InvalidParam {
                what: "brightness".to_owned(),
                reason: format!(
                    "oscillator brightness must sit within [0, 100], got {}",
                    self.brightness
                ),
            });
        }
        for (slot, type_, period, param1) in [
            (
                "signal",
                self.signal_type.as_str(),
                self.signal_period,
                self.signal_param1,
            ),
            (
                "mean",
                self.mean_type.as_str(),
                self.mean_period,
                self.mean_param1,
            ),
        ] {
            let family = SmoothingFamily::parse(type_).ok_or_else(|| EguiError::InvalidParam {
                what: format!("{slot}_type"),
                reason: format!("unknown smoothing family {type_:?}"),
            })?;
            if !(MA_PERIOD_MIN..=MA_PERIOD_MAX).contains(&period) {
                return Err(EguiError::InvalidParam {
                    what: format!("{slot}_period"),
                    reason: format!(
                        "smoothing window must sit within [{MA_PERIOD_MIN}, {MA_PERIOD_MAX}], got {period}"
                    ),
                });
            }
            if !param1.is_finite() || param1 < 0.0 {
                return Err(EguiError::InvalidParam {
                    what: format!("{slot}_param1"),
                    reason: format!(
                        "smoothing parameter must be finite and non-negative, got {param1}"
                    ),
                });
            }
            // Family-precise contract (EIT alpha within (0, 1], ALMA offset
            // and sigma bounds, positive lengths): the kernel catalog owns
            // the rule, validation only names the dial it fired on.
            family
                .params_for_args(&SmoothingArgs::length(period).with_n1(param1))
                .map_err(|error| EguiError::InvalidParam {
                    what: format!("{slot}_param1"),
                    reason: error.to_string(),
                })?;
        }
        let source = PriceSource::parse(&self.price_source).ok_or_else(|| EguiError::InvalidParam {
            what: "price_source".to_owned(),
            reason: format!(
                "unknown input source {:?}: expected a price source (close, hl2, ...) or the adaptive relay",
                self.price_source
            ),
        })?;
        // Multi-leg kernels read high/low/volume legs no single-column
        // resynthesis supplies, so resynthesized inputs refuse on those arms
        // instead of miscomputing. Single-leg arms follow the column.
        if source != PriceSource::Close || self.adaptive {
            for (slot, kind) in [
                ("fast_kind", self.fast_kind.as_str()),
                ("slow_kind", self.slow_kind.as_str()),
            ] {
                if matches!(kind, "mfi" | "stoch") {
                    return Err(EguiError::InvalidParam {
                        what: (*slot).to_owned(),
                        reason: format!(
                            "{kind:?} reads full-OHLC legs, so it refuses resynthesized inputs; exact multi-leg resynthesis lands with the exact-arms milestone"
                        ),
                    });
                }
            }
        }
        for (slot, value) in [
            ("ltf_timeframe", self.ltf_timeframe.as_str()),
            ("fetch_timeframe", self.fetch_timeframe.as_str()),
            ("htf_timeframe", self.htf_timeframe.as_str()),
            ("mtf_timeframe_1", self.mtf_timeframe_1.as_str()),
            ("mtf_timeframe_2", self.mtf_timeframe_2.as_str()),
            ("mtf_timeframe_3", self.mtf_timeframe_3.as_str()),
            ("mtf_timeframe_4", self.mtf_timeframe_4.as_str()),
        ] {
            if !lite_timeframe_ids().iter().any(|id| id == value) {
                return Err(EguiError::InvalidParam {
                    what: (*slot).to_owned(),
                    reason: format!("{value:?} is not a listed timeframe"),
                });
            }
        }
        if !lite_position_ids()
            .iter()
            .any(|id| id == &self.mtf_table_position)
        {
            return Err(EguiError::InvalidParam {
                what: "mtf_table_position".to_owned(),
                reason: format!(
                    "{:?} is not a listed table location",
                    self.mtf_table_position
                ),
            });
        }
        if !lite_size_ids().iter().any(|id| id == &self.mtf_table_size) {
            return Err(EguiError::InvalidParam {
                what: "mtf_table_size".to_owned(),
                reason: format!("{:?} is not a listed table size", self.mtf_table_size),
            });
        }
        if self.session.trim().is_empty() {
            return Err(EguiError::InvalidParam {
                what: "session".to_owned(),
                reason: "trading session must not be empty".to_owned(),
            });
        }
        for (slot, color) in [
            ("mtf_table_text", self.mtf_table_text.as_str()),
            ("mtf_table_bg", self.mtf_table_bg.as_str()),
        ] {
            fluent_egui::parse_hex_color(slot, color).map_err(|error| EguiError::InvalidParam {
                what: (*slot).to_owned(),
                reason: error.to_string(),
            })?;
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
    /// Whether the delta-volume histogram paints on the sub-pane.
    pub show_delta: bool,
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
    let native = lite_native_series(rows, config.eit_alpha)?;
    let volumes = row_volumes(rows);
    let leg_frame = lite_leg_frame(rows, config, &native.adaptive)?;
    let fast_bounded = lite_panel_leg(&leg_frame, rows, config, LiteLeg::Fast)?;
    let slow_bounded = lite_panel_leg(&leg_frame, rows, config, LiteLeg::Slow)?;
    // The dialed ribbon pair replaces the legacy fixed EIT/EMA legs. At
    // the script-matching defaults (EIT-0.12, EMA-21) the families read
    // the same recurrences over the same relay, so the pane is unchanged
    // until a dial moves.
    let signal = ribbon_smoothing_leg(
        &native.adaptive,
        &volumes,
        &config.signal_type,
        config.signal_period,
        config.signal_param1,
        "signal",
    )?;
    let mean = ribbon_smoothing_leg(
        &native.adaptive,
        &volumes,
        &config.mean_type,
        config.mean_period,
        config.mean_param1,
        "mean",
    )?;
    let (ribbon_health, _vol_ratio) = ribbon_health_inputs(
        rows,
        config.eit_alpha,
        config.ribbon_health_k,
        &signal,
        &mean,
    )?;
    let ribbon_health_bounded = squash_series_to_100(&ribbon_health);
    let ma_health_bounded = squash_series_to_100(&ma_health_series(rows, config.ma_health_k)?);
    let (buy, sell, delta_confidence) = delta_splits(rows);
    let delta_volume = delta_candle_view(rows, &buy, &sell, config.delta_unit)?;
    let pivots = pivot_marks(&fast_bounded, squash_to_100(config.pivot_threshold))?;
    let ribbon = ribbon_view(&native.adaptive, &signal, config.fill_alpha)?;
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
        show_delta: config.delta_show,
    })
}

/// Leg frame behind one sub-pane evaluation: the adaptive EIT relay when
/// Adaptive is armed (holes as `NaN` through the shared fenced
/// constructor), the selected price-source column otherwise, and the legacy
/// full-OHLC close frame when the source is Close — byte-identical to the
/// pre-source legs. Validation already refused resynthesized inputs on
/// multi-leg arms and unknown sources, so the parse below cannot fail
/// open; the refusal repeats the field name for the frame path.
fn lite_leg_frame(
    rows: &[ChartRow],
    config: &IntegrationLiteConfig,
    adaptive: &[Option<f64>],
) -> Result<OwnedOhlcv, EguiError> {
    if config.adaptive {
        return Ok(synth_close_frame(adaptive, &row_volumes(rows)));
    }
    match PriceSource::parse(&config.price_source) {
        Some(PriceSource::Close) => Ok(kernel_preview_frame(rows)),
        Some(source) => Ok(source_frame(rows, source)),
        None => Err(EguiError::InvalidParam {
            what: "price_source".to_owned(),
            reason: format!(
                "unknown input source {:?}: expected a price source (close, hl2, ...)",
                config.price_source
            ),
        }),
    }
}

/// MA-health oscillator off the price pane, at gain `k`.
///
/// Folds the context's MA bundle — ALMA-8 / VWMA-12 / EMA-21 against ATR-14 —
/// through the shared [`health_mas`] scorer at the frozen [`HEALTH_K`], then
/// scales by `k` (the Pine `context.ma_health * i_ma_health_k` plot). Warmup
/// holes stay holes; output lines up with `rows`.
fn ma_health_series(rows: &[ChartRow], k: f64) -> Result<Vec<Option<f64>>, EguiError> {
    let frame = kernel_preview_frame(rows);
    let raw = protopyne_kernels::lite::ma_health_series(&frame)
        .map_err(|error| describe_kernel(&error))?;
    Ok(raw
        .into_iter()
        .map(|slot| slot.map(|score| score * k))
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

#[cfg(test)]
#[path = "../tests/lite_display_flags.rs"]
mod lite_display_flags_tests;

#[cfg(test)]
#[path = "../tests/pine_input_coverage.rs"]
mod pine_input_coverage_tests;
