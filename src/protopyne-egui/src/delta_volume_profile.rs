//! Native Rust equivalency for `_DeltaVolumeProfileProtopyne.pine`.
//!
//! The Pine indicator bins traded volume by price over a rolling lookback,
//! tracks the buy/sell imbalance per bin, and marks the point of control
//! (POC), the value area, and an anchored VWAP. This module reproduces that
//! display truth from converted rows with native kernels: each bar's volume is
//! distributed across the price bins its high/low range overlaps (proportional
//! overlap, mirroring the Pine `distributeBarVolumeDelta`), the buy share
//! comes from the shared shape estimator, and the POC / value-area / VWAP
//! reductions run over the accumulated bins. Nothing here decides a trade; it
//! is a profile picture, and every output is display `f64`.
//!
//! The plugin exposes per-bar primitive series (high, low, close, volume,
//! buy, and the anchored VWAP) so the generic pane windowing can slice it like
//! any other overlay; the profile is rebuilt from those windowed primitives at
//! paint time, which keeps the profile pinned to the visible range.

use fluent_wvr::prelude::*;
use fluent_wvr::{Describable, FieldAccess};
use protopyne_chart_model::ChartRow;
use protopyne_kernels::bars::estimate_delta_volume_estimated;
use protopyne_types::{check_for_execute, valid_output, Validate};

use fluent_egui::{
    needs_column_aggregation, opacity_from_transparency, paint_oscillator_styled,
    proportional_line_points, sampled_line_points, translate_points, visible_rows, EguiError,
    IndicatorStyle, ReadoutValue, SlotAttachment, ThemeConfig, Viewport,
};

/// Plan-node namespace for the profile's per-bar primitive series.
pub const PROFILE_NODES: [&str; 6] = [
    "dvol:high",
    "dvol:low",
    "dvol:close",
    "dvol:volume",
    "dvol:buy",
    "dvol:vwap",
];

/// Stable plugin id (also the hot-slot id).
pub const DELTA_VOLUME_PROFILE_ID: &str = "delta_volume_profile";

/// Below this the reference treats a bin as empty (`VOLUME_EPSILON` in
/// `_DeltaVolumeProfileProtopyne.pine`): it paints nothing, its imbalance
/// reads as neutral, and its volume ratio falls back to `1.0`.
pub const VOLUME_EPSILON: f64 = 0.0001;
/// Imbalance exponent behind the ported gradient (`math.pow(|ratio|, 0.7)`).
/// Below one, so a mildly lopsided bin already shifts visibly off neutral
/// while a saturated one saturates before the top of the band.
const AMPLIFY_EXPONENT: f64 = 0.7;
/// Imbalance gain around the neutral midpoint (`... * 1.5`).
const AMPLIFY_GAIN: f64 = 1.5;
/// Divisor turning a volume ratio into a shading strength (`vol_ratio / 4.0`).
const VOLUME_STRENGTH_DIVISOR: f64 = 4.0;
/// Transparency a zero-volume bin would carry (`75 - ...`).
const TRANSPARENCY_BASE: f64 = 75.0;
/// How much a thick bin gains opacity, up to the base.
const TRANSPARENCY_SPAN: f64 = 70.0;
/// Transparency floor: even the thickest bin never paints fully opaque, so
/// the profile always reads as a wash behind the candles.
const MIN_TRANSPARENCY: f64 = 5.0;
/// Transparency ceiling: a bin never vanishes entirely.
const MAX_TRANSPARENCY: f64 = 95.0;
/// Lower bound of the ported gradient band (`lo` in `from_gradient`).
const GRADIENT_LOW: f64 = 0.2;
/// Upper bound of the ported gradient band (`hi` in `from_gradient`).
const GRADIENT_HIGH: f64 = 0.8;
/// Transparency of the point-of-control outline, matching the reference
/// `color.new(color_support, 20)` border.
const POC_BORDER_TRANSPARENCY: f64 = 20.0;

/// One bin of a volume profile: a price slice and the volume/imbalance that
/// traded inside it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VolumeBin {
    /// Lower price edge of the bin.
    pub price_low: f64,
    /// Upper price edge of the bin.
    pub price_high: f64,
    /// Total volume distributed into the bin.
    pub volume: f64,
    /// Net buy-minus-neutral imbalance in the bin.
    pub imbalance: f64,
}

/// One bar's display primitives behind the profile.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProfileBar {
    /// Bar high.
    pub high: f64,
    /// Bar low.
    pub low: f64,
    /// Bar close.
    pub close: f64,
    /// Bar volume.
    pub volume: f64,
    /// Estimated buy share of the volume.
    pub buy: f64,
}

/// A complete volume profile over a window.
#[derive(Debug, Clone, PartialEq)]
pub struct DeltaVolumeProfile {
    /// Bins in ascending price order.
    pub bins: Vec<VolumeBin>,
    /// Index of the point of control (highest-volume bin), if any.
    pub poc: Option<usize>,
    /// Value-area high (upper edge of the smallest high-volume band reaching
    /// the configured share).
    pub vah: Option<f64>,
    /// Value-area low.
    pub val: Option<f64>,
    /// Anchored VWAP over the window.
    pub vwap: Option<f64>,
    /// Volume in the largest bin (normalizer for bar widths).
    pub max_volume: f64,
    /// Mean volume per bin across every bin, empty ones included
    /// (`avg_bin_volume`). The ported bin shading compares each bin against
    /// this rather than against the peak, so an evenly-traded profile and one
    /// with a single spike both read as evenly shaded.
    pub avg_volume: f64,
}

impl DeltaVolumeProfile {
    /// The price of the point of control, if any.
    #[must_use]
    pub fn poc_price(&self) -> Option<f64> {
        self.poc
            .and_then(|index| self.bins.get(index))
            .map(|bin| f64::midpoint(bin.price_low, bin.price_high))
    }
}

/// Build a volume profile over `bars` with `bins` price slices and a value
/// area covering `value_area_pct` of the traded volume.
///
/// Each bar's volume is spread across the bins its `[low, high]` range
/// overlaps in proportion to the overlap (a zero-range bar lands entirely in
/// its single bin). The buy share is taken as given in [`ProfileBar::buy`].
/// Degenerate inputs (no bars, `bins == 0`, a flat or non-finite range, a
/// non-finite `value_area_pct`) yield an empty profile instead of a guessed
/// one.
#[must_use]
pub fn build_profile(bars: &[ProfileBar], bins: usize, value_area_pct: f64) -> DeltaVolumeProfile {
    let empty = DeltaVolumeProfile {
        bins: Vec::new(),
        poc: None,
        vah: None,
        val: None,
        vwap: None,
        max_volume: 0.0,
        avg_volume: 0.0,
    };
    if bars.is_empty() || bins == 0 || !value_area_pct.is_finite() {
        return empty;
    }
    let mut low = f64::INFINITY;
    let mut high = f64::NEG_INFINITY;
    for bar in bars {
        if !bar.high.is_finite() || !bar.low.is_finite() {
            return empty;
        }
        low = low.min(bar.low);
        high = high.max(bar.high);
    }
    if !low.is_finite() || !high.is_finite() || high <= low {
        return empty;
    }
    let bin_height = (high - low) / bins as f64;
    if !bin_height.is_finite() || bin_height <= 0.0 {
        return empty;
    }
    let mut bins_out = vec![
        VolumeBin {
            price_low: low,
            price_high: low,
            volume: 0.0,
            imbalance: 0.0
        };
        bins
    ];
    for (index, bin) in bins_out.iter_mut().enumerate() {
        bin.price_low = low + index as f64 * bin_height;
        bin.price_high = bin.price_low + bin_height;
    }
    let mut vwap_num = 0.0;
    let mut vwap_den = 0.0;
    for bar in bars {
        let total = bar.volume;
        if !total.is_finite() || total <= 0.0 {
            continue;
        }
        let buy = if bar.buy.is_finite() {
            bar.buy.clamp(0.0, total)
        } else {
            total * 0.5
        };
        let typical = (bar.high + bar.low + bar.close) / 3.0;
        if typical.is_finite() {
            vwap_num += typical * total;
            vwap_den += total;
        }
        let range = bar.high - bar.low;
        if range <= f64::EPSILON * high.max(1.0) {
            let idx = bin_index(bar.close, low, bin_height, bins);
            add_to_bin(&mut bins_out[idx], total, buy);
            continue;
        }
        let first = bin_index(bar.low, low, bin_height, bins);
        let last = bin_index(bar.high, low, bin_height, bins);
        for bin in bins_out.iter_mut().take(last + 1).skip(first) {
            let overlap_low = bar.low.max(bin.price_low);
            let overlap_high = bar.high.min(bin.price_high);
            if overlap_high <= overlap_low {
                continue;
            }
            let frac = (overlap_high - overlap_low) / range;
            add_to_bin(bin, total * frac, buy * frac);
        }
    }
    let max_volume = bins_out.iter().map(|bin| bin.volume).fold(0.0, f64::max);
    let poc = if max_volume > 0.0 {
        bins_out
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.volume.total_cmp(&b.1.volume))
            .map(|(i, _)| i)
    } else {
        None
    };
    let (vah, val) = poc.map_or((None, None), |index| {
        value_area(&bins_out, index, value_area_pct)
    });
    let vwap = (vwap_den > 0.0).then_some(vwap_num / vwap_den);
    // The mean is over every requested bin, empty ones included, exactly as
    // the reference divides its total by the array size.
    let total: f64 = bins_out.iter().map(|bin| bin.volume).sum();
    let avg_volume = total / bins_out.len() as f64;
    DeltaVolumeProfile {
        bins: bins_out,
        poc,
        vah,
        val,
        vwap,
        max_volume,
        avg_volume,
    }
}

/// The bin index a price falls in, clamped to the valid range.
fn bin_index(price: f64, anchor: f64, bin_height: f64, bins: usize) -> usize {
    let raw = ((price - anchor) / bin_height).floor();
    if !raw.is_finite() || raw <= 0.0 {
        return 0;
    }
    (raw as usize).min(bins - 1)
}

/// Add a volume/buy pair into one bin, accumulating the neutral-differenced
/// imbalance.
fn add_to_bin(bin: &mut VolumeBin, volume: f64, buy: f64) {
    bin.volume += volume;
    bin.imbalance += buy - volume / 2.0;
}

/// The value-area high/low around the POC: expand from the POC toward the
/// larger neighbour until the accumulated share reaches `pct`.
fn value_area(bins: &[VolumeBin], poc: usize, pct: f64) -> (Option<f64>, Option<f64>) {
    let total: f64 = bins.iter().map(|bin| bin.volume).sum();
    if total <= 0.0 {
        return (None, None);
    }
    let target = total * pct.clamp(0.0, 1.0);
    let mut low = poc;
    let mut high = poc;
    let mut acc = bins[poc].volume;
    while acc < target && (low > 0 || high + 1 < bins.len()) {
        let below = if low > 0 {
            bins[low - 1].volume
        } else {
            f64::NEG_INFINITY
        };
        let above = if high + 1 < bins.len() {
            bins[high + 1].volume
        } else {
            f64::NEG_INFINITY
        };
        if above >= below {
            high += 1;
            acc += bins[high].volume;
        } else {
            low -= 1;
            acc += bins[low].volume;
        }
    }
    (Some(bins[high].price_high), Some(bins[low].price_low))
}

/// The two endpoints a profile bin's gradient interpolates between: the
/// support end a buy-dominant bin reads as, and the resistance end a
/// sell-dominant bin reads as.
///
/// The reference defaults these to Pine's `color_support` / `color_resistance`
///; a terminal that paints through a theme resolves them from its bull and
/// bear slots instead (see [`bin_palette`]), and the Style tab's `color_bull`
/// / `color_bear` overrides reach the profile through the same two slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BinPalette {
    /// Buy-side color; the gradient's upper end.
    pub support: [u8; 3],
    /// Sell-side color; the gradient's lower end.
    pub resistance: [u8; 3],
}

/// One bin's ported fill: gradient RGB plus the Pine transparency percent that
/// the painter turns into byte opacity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BinFill {
    /// Gradient red channel.
    pub red: u8,
    /// Gradient green channel.
    pub green: u8,
    /// Gradient blue channel.
    pub blue: u8,
    /// `color.new` transparency percent within `[0, 100]`.
    pub transparency: f64,
}

impl BinFill {
    /// The fill as an `egui` color, resolving its Pine transparency through
    /// the shared opacity mapping so profile shading and candle tints use one
    /// transparency rule.
    #[must_use]
    pub fn color(&self) -> egui::Color32 {
        egui::Color32::from_rgba_unmultiplied(
            self.red,
            self.green,
            self.blue,
            opacity_from_transparency(self.transparency),
        )
    }
}

/// Resolve the profile's two gradient endpoints for one indicator: the bull
/// and bear slots, through that slot's [`IndicatorStyle`] so the Style tab can
/// restyle the profile exactly as it restyles the indicator's lines.
pub fn bin_palette(style: &IndicatorStyle, theme: &ThemeConfig) -> Result<BinPalette, EguiError> {
    let channel = |color: egui::Color32| [color.r(), color.g(), color.b()];
    Ok(BinPalette {
        support: channel(style.color(theme, "bull")?),
        resistance: channel(style.color(theme, "bear")?),
    })
}

/// The position a gradient value maps to between `low` and `high` bounds,
/// clamped to `[0, 1]` — the continuous form of Pine's
/// `color.from_gradient(value, low, high, …)`.
#[must_use]
fn gradient_position(value: f64, low: f64, high: f64) -> f64 {
    if !value.is_finite() || (high - low).abs() < f64::EPSILON {
        return 0.0;
    }
    ((value - low) / (high - low)).clamp(0.0, 1.0)
}

/// One bin's buy/sell imbalance as a share of the most it could hold, clamped
/// to `[-1, +1]` — a port of the reference `getImbalanceRatio`.
///
/// A bin below [`VOLUME_EPSILON`] and a bin whose maximum possible imbalance
/// is itself negligible both read as exactly neutral (`0.0`), mirroring the
/// reference guards, and a non-finite bin reads neutral rather than painting a
/// color nobody measured.
#[must_use]
pub fn imbalance_ratio(bin: &VolumeBin) -> f64 {
    if !bin.volume.is_finite() || !bin.imbalance.is_finite() || bin.volume < VOLUME_EPSILON {
        return 0.0;
    }
    let max_imbalance = bin.volume / 2.0;
    if max_imbalance <= VOLUME_EPSILON {
        return 0.0;
    }
    (bin.imbalance / max_imbalance).clamp(-1.0, 1.0)
}

/// The ported `_DeltaVolumeProfileProtopyne.pine :: getBinColors`: one bin's
/// gradient color and its Pine transparency.
///
/// The reference reads a bin as two coupled signals instead of one up/down
/// flip, and so does this:
///
/// - **Color** follows the *imbalance*, amplified non-linearly around a
///   neutral midpoint — `0.5 + |ratio|^0.7 * sign(ratio) * 1.5`, clamped —
///   and mapped across the `0.2..0.8` gradient band from the resistance end to
///   the support end. Because the exponent is below one, a mildly lopsided bin
///   already reads off-neutral, and adjacent bins move continuously instead of
///   alternating between two flat colors.
/// - **Transparency** follows the *volume* relative to the mean bin
///   (`75 - (ratio / 4)^2 * 70`, clamped to `5..95`), so thickness reads as
///   density and a thin bin stays faint rather than recolored.
///
/// `None` for an index past the last bin or a bin under [`VOLUME_EPSILON`],
/// exactly as the reference returns `COLOR_NONE`; the caller then paints
/// nothing for that bin rather than substituting neutral gray.
#[must_use]
pub fn bin_fill(
    profile: &DeltaVolumeProfile,
    index: usize,
    palette: &BinPalette,
) -> Option<BinFill> {
    let bin = profile.bins.get(index)?;
    if !bin.volume.is_finite() || bin.volume < VOLUME_EPSILON {
        return None;
    }
    let imbalance = imbalance_ratio(bin);
    let avg_volume = profile.avg_volume;
    let volume_ratio = if avg_volume.is_finite() && avg_volume > VOLUME_EPSILON {
        bin.volume / avg_volume
    } else {
        1.0
    };
    let amplified = (0.5
        + imbalance.abs().powf(AMPLIFY_EXPONENT) * imbalance.signum() * AMPLIFY_GAIN)
        .clamp(0.0, 1.0);
    let volume_strength = (volume_ratio / VOLUME_STRENGTH_DIVISOR).powi(2);
    // Pine `int()` truncates toward zero before the clamp.
    let transparency = (TRANSPARENCY_BASE - volume_strength * TRANSPARENCY_SPAN)
        .trunc()
        .clamp(MIN_TRANSPARENCY, MAX_TRANSPARENCY);
    let position = gradient_position(amplified, GRADIENT_LOW, GRADIENT_HIGH);
    // Resistance sits at position 0, support at 1, so the gradient runs from
    // the sell end into the buy end exactly as `from_gradient` is called.
    let channel = |low: u8, high: u8| {
        (f64::from(low) + (f64::from(high) - f64::from(low)) * position)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Some(BinFill {
        red: channel(palette.resistance[0], palette.support[0]),
        green: channel(palette.resistance[1], palette.support[1]),
        blue: channel(palette.resistance[2], palette.support[2]),
        transparency,
    })
}

/// One painted profile bar: a rectangle at the chart's right edge plus the
/// ported fill it carries.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProfileBox {
    /// Rectangle in pane-local pixels.
    pub rect: egui::Rect,
    /// Index of the bin this box draws, so the painter can read that bin's
    /// imbalance back off the profile without re-walking it.
    pub bin: usize,
    /// Gradient color and Pine transparency for this bin.
    pub fill: BinFill,
    /// Whether this is the point-of-control bin.
    pub poc: bool,
}

/// Lay a profile out as horizontal bars anchored at `anchor_index`,
/// extending leftward by up to `width_slots` bars.
///
/// `origin` is the pane's top-left. Off-window bins are skipped so the profile
/// never paints past the visible price range, as are bins under
/// [`VOLUME_EPSILON`], which the ported shading refuses. A degenerate profile
/// (no bins or no volume) yields no boxes.
#[must_use]
pub fn profile_boxes(
    profile: &DeltaVolumeProfile,
    viewport: &Viewport,
    origin: egui::Pos2,
    anchor_index: usize,
    width_slots: i64,
    palette: &BinPalette,
) -> Vec<ProfileBox> {
    if profile.bins.is_empty() || profile.max_volume <= 0.0 || width_slots <= 0 {
        return Vec::new();
    }
    let anchor_x = origin.x + viewport.index_to_x(anchor_index);
    let max_width = width_slots as f32 * viewport.scale().bar_spacing();
    let mut boxes = Vec::with_capacity(profile.bins.len());
    for (index, bin) in profile.bins.iter().enumerate() {
        let Some(fill) = bin_fill(profile, index, palette) else {
            continue;
        };
        let (Some(y_high), Some(y_low)) = (
            viewport.windowed_y(bin.price_high),
            viewport.windowed_y(bin.price_low),
        ) else {
            continue;
        };
        let (top, bottom) = (y_high.min(y_low), y_high.max(y_low));
        let width = ((bin.volume / profile.max_volume) as f32 * max_width).max(1.0);
        let rect = egui::Rect::from_min_max(
            egui::pos2(anchor_x - width, origin.y + top),
            egui::pos2(anchor_x, origin.y + bottom),
        );
        boxes.push(ProfileBox {
            rect,
            bin: index,
            fill,
            poc: profile.poc == Some(index),
        });
    }
    boxes
}

/// The native plugin configuration, schema-driven from these fields.
///
/// The reference indicator exposes four independent visibility toggles
/// (profile, VWAP, value area, POC); splitting them into sub-structs would
/// fragment the one settings schema the modal renders from.
#[allow(clippy::struct_excessive_bools)]
#[derive(
    Debug, Clone, bon::Builder, serde::Serialize, serde::Deserialize, FieldAccess, Describable,
)]
#[builder(start_fn = new)]
pub struct DeltaVolumeProfileConfig {
    /// Number of price bins across the lookback range.
    #[field(desc = "Number of price bins", min = 8, max = 600)]
    #[builder(default = default_bins())]
    pub bins: i64,
    /// Bars folded into the rolling profile.
    #[field(desc = "Lookback bars", min = 10, max = 2000)]
    #[builder(default = default_lookback())]
    pub lookback: i64,
    /// Profile bar width in chart bars.
    #[field(desc = "Profile width in bars", min = 4, max = 120)]
    #[builder(default = default_width())]
    pub width: i64,
    /// Offset of the profile from the last bar, in chart bars.
    #[field(desc = "Profile offset in bars", min = 0, max = 60)]
    #[builder(default = default_offset())]
    pub offset: i64,
    /// Fraction of traded volume the value area must cover.
    #[field(desc = "Value-area share", min = 0.1, max = 0.95)]
    #[builder(default = default_value_area_pct())]
    pub value_area_pct: f64,
    /// Whether the volume profile bars paint.
    #[field(desc = "Show the volume profile")]
    #[builder(default = true)]
    pub show_profile: bool,
    /// Whether the anchored VWAP paints.
    #[field(desc = "Show the anchored VWAP")]
    #[builder(default = true)]
    pub show_vwap: bool,
    /// Whether the value-area high/low rules paint.
    #[field(desc = "Show the value area")]
    #[builder(default = true)]
    pub show_value_area: bool,
    /// Whether the point-of-control rule paints.
    #[field(desc = "Show the point of control")]
    #[builder(default = true)]
    pub show_poc: bool,
}

fn default_bins() -> i64 {
    60
}
fn default_lookback() -> i64 {
    100
}
fn default_width() -> i64 {
    30
}
fn default_offset() -> i64 {
    5
}
fn default_value_area_pct() -> f64 {
    0.7
}

impl DeltaVolumeProfileConfig {
    /// Fail-closed self-consistency check.
    pub fn validate(&self) -> Result<(), EguiError> {
        if !(8..=600).contains(&self.bins) {
            return Err(EguiError::InvalidParam {
                what: "bins".to_owned(),
                reason: "bin count must sit within [8, 600]".to_owned(),
            });
        }
        if !(2..=2000).contains(&self.lookback) {
            return Err(EguiError::InvalidParam {
                what: "lookback".to_owned(),
                reason: "lookback must sit within [2, 2000]".to_owned(),
            });
        }
        if !(1..=120).contains(&self.width) {
            return Err(EguiError::InvalidParam {
                what: "width".to_owned(),
                reason: "profile width must sit within [1, 120] bars".to_owned(),
            });
        }
        if self.offset < 0 {
            return Err(EguiError::InvalidParam {
                what: "offset".to_owned(),
                reason: "profile offset must not be negative".to_owned(),
            });
        }
        if !self.value_area_pct.is_finite()
            || self.value_area_pct <= 0.0
            || self.value_area_pct > 1.0
        {
            return Err(EguiError::InvalidParam {
                what: "value_area_pct".to_owned(),
                reason: "value-area share must sit within (0, 1]".to_owned(),
            });
        }
        Ok(())
    }
}

impl Default for DeltaVolumeProfileConfig {
    fn default() -> Self {
        Self::new().build()
    }
}

impl Validate for DeltaVolumeProfileConfig {
    type Err = EguiError;
    fn validate(&self) -> Result<(), Self::Err> {
        DeltaVolumeProfileConfig::validate(self)
    }
}

impl WorkUnit for DeltaVolumeProfileConfig {
    fn name(&self) -> &str {
        "delta_volume_profile_config"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        check_for_execute(self, "delta volume profile config invalid")?;
        Ok(valid_output(format!(
            "delta volume profile ready ({} bins)",
            self.bins
        )))
    }
}

impl_component!(DeltaVolumeProfileConfig);

/// Build the profile primitives for one row window: high, low, close, volume,
/// buy, and the causal anchored VWAP, each `None` before the lookback start.
pub fn profile_series(
    rows: &[ChartRow],
    config: &DeltaVolumeProfileConfig,
) -> Result<Vec<Vec<Option<f64>>>, EguiError> {
    config.validate()?;
    let count = rows.len();
    let start = count.saturating_sub(config.lookback as usize);
    let mut high = vec![None; count];
    let mut low = vec![None; count];
    let mut close = vec![None; count];
    let mut volume = vec![None; count];
    let mut buy = vec![None; count];
    let mut vwap = vec![None; count];
    let mut num = 0.0;
    let mut den = 0.0;
    for index in start..count {
        let row = &rows[index];
        let estimate = estimate_delta_volume_estimated(
            row.open,
            row.high,
            row.low,
            row.close,
            row.volume as f64,
        );
        high[index] = Some(row.high);
        low[index] = Some(row.low);
        close[index] = Some(row.close);
        volume[index] = Some(row.volume as f64);
        buy[index] = Some(estimate.buy_volume);
        let typical = (row.high + row.low + row.close) / 3.0;
        if row.volume > 0 && typical.is_finite() {
            num += typical * row.volume as f64;
            den += row.volume as f64;
            if den > 0.0 {
                vwap[index] = Some(num / den);
            }
        }
    }
    Ok(vec![high, low, close, volume, buy, vwap])
}

/// Rebuild the profile from the trailing window of an evaluation's primitive
/// series.
///
/// `base` is the global index of the window's first bar within the
/// full-history evaluation, so the primitives borrow through the shared
/// slicer (no per-frame copy) — the same trailing window the canvas paints.
fn profile_from_series(
    evaluation: &crate::indicators::IndicatorEvaluation,
    base: usize,
    bins: usize,
    value_area_pct: f64,
) -> DeltaVolumeProfile {
    let series = |node: &str| {
        evaluation
            .series
            .iter()
            .find(|series| series.node == node)
            .map(|series| {
                let len = series.values.len().saturating_sub(base);
                visible_rows(series.values.as_slice(), base, len)
            })
    };
    let (Some(high), Some(low), Some(close), Some(volume), Some(buy)) = (
        series("dvol:high"),
        series("dvol:low"),
        series("dvol:close"),
        series("dvol:volume"),
        series("dvol:buy"),
    ) else {
        return build_profile(&[], bins, value_area_pct);
    };
    let count = high
        .len()
        .min(low.len())
        .min(close.len())
        .min(volume.len())
        .min(buy.len());
    let bars: Vec<ProfileBar> = (0..count)
        .filter_map(|index| {
            Some(ProfileBar {
                high: high.get(index).copied().flatten()?,
                low: low.get(index).copied().flatten()?,
                close: close.get(index).copied().flatten()?,
                volume: volume.get(index).copied().flatten()?,
                buy: buy.get(index).copied().flatten()?,
            })
        })
        .collect();
    build_profile(&bars, bins, value_area_pct)
}

/// The Delta Volume Profile plugin: per-bar primitives plus the painted
/// profile, POC, value area, and anchored VWAP.
pub struct DeltaVolumeProfileIndicator;

impl DeltaVolumeProfileIndicator {
    /// Create the plugin.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for DeltaVolumeProfileIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::indicators::ChartIndicator for DeltaVolumeProfileIndicator {
    fn id(&self) -> &str {
        DELTA_VOLUME_PROFILE_ID
    }

    fn title(&self) -> &str {
        "Delta Volume Profile"
    }

    fn attachment(&self) -> SlotAttachment {
        SlotAttachment::MainOverlay
    }

    fn default_config(&self) -> Box<dyn Component> {
        Box::new(DeltaVolumeProfileConfig::default())
    }

    fn plan_nodes(&self, _config: &dyn Component) -> Result<Vec<String>, EguiError> {
        Ok(PROFILE_NODES
            .iter()
            .map(|node| (*node).to_owned())
            .collect())
    }

    fn config_key(&self, config: &dyn Component) -> Result<u64, EguiError> {
        let config = fluent_wvr::component_downcast_ref::<DeltaVolumeProfileConfig>(config)
            .ok_or_else(|| EguiError::InvalidParam {
                what: "config".to_owned(),
                reason: format!("{} expects a DeltaVolumeProfile configuration", self.id()),
            })?;
        Ok(common_core::hash::fingerprint_json(config))
    }

    fn evaluate(
        &self,
        rows: &[ChartRow],
        config: &dyn Component,
        _series: &crate::topology::SeriesCatalog,
    ) -> Result<crate::indicators::IndicatorEvaluation, EguiError> {
        let config = fluent_wvr::component_downcast_ref::<DeltaVolumeProfileConfig>(config)
            .ok_or_else(|| EguiError::InvalidParam {
                what: "config".to_owned(),
                reason: format!("{} expects a DeltaVolumeProfile configuration", self.id()),
            })?;
        let series = profile_series(rows, config)?;
        let fingerprint = common_core::hash::fingerprint_json(config);
        let built = PROFILE_NODES
            .iter()
            .zip(series)
            .map(|(node, values)| crate::indicators::IndicatorSeries {
                node: (*node).to_owned(),
                name: node.trim_start_matches("dvol:").to_owned(),
                values,
                style: "fast".to_owned(),
                attachment: SlotAttachment::MainOverlay,
                key: fluent_egui::cache_key(node, fingerprint),
            })
            .collect();
        Ok(crate::indicators::IndicatorEvaluation { series: built })
    }

    fn readouts(&self, evaluation: &crate::indicators::IndicatorEvaluation) -> Vec<ReadoutValue> {
        let defaults = DeltaVolumeProfileConfig::default();
        let profile = profile_from_series(
            evaluation,
            0,
            defaults.bins as usize,
            defaults.value_area_pct,
        );
        vec![
            ReadoutValue {
                label: "POC".to_owned(),
                value: profile.poc_price(),
            },
            ReadoutValue {
                label: "VWAP".to_owned(),
                value: profile.vwap,
            },
        ]
    }

    fn paint_main_overlay(
        &self,
        painter: &egui::Painter,
        evaluation: &crate::indicators::IndicatorEvaluation,
        viewport: &Viewport,
        origin: egui::Pos2,
        base: usize,
        width_px: f32,
        theme: &ThemeConfig,
        config: &dyn Component,
        style: &IndicatorStyle,
    ) -> Result<(), EguiError> {
        let config = fluent_wvr::component_downcast_ref::<DeltaVolumeProfileConfig>(config)
            .ok_or_else(|| EguiError::InvalidParam {
                what: "config".to_owned(),
                reason: "delta_volume_profile expects a DeltaVolumeProfile configuration"
                    .to_owned(),
            })?;
        let profile = profile_from_series(
            evaluation,
            base,
            config.bins as usize,
            config.value_area_pct,
        );
        if profile.bins.is_empty() {
            return Ok(());
        }
        let palette = bin_palette(style, theme)?;
        let full = evaluation
            .series
            .first()
            .map_or(0, |series| series.values.len());
        let count = full.saturating_sub(base);
        let anchor = count.saturating_sub(1) + config.offset as usize;
        if config.show_profile {
            for profile_box in
                profile_boxes(&profile, viewport, origin, anchor, config.width, &palette)
            {
                painter.rect_filled(profile_box.rect, 0.0, profile_box.fill.color());
                if profile_box.poc {
                    // The reference outlines the point of control on the side
                    // that formed it, so the ring says *which* imbalance made
                    // the POC rather than just marking it.
                    let edge = if profile
                        .bins
                        .get(profile_box.bin)
                        .is_none_or(|bin| imbalance_ratio(bin) >= 0.0)
                    {
                        palette.support
                    } else {
                        palette.resistance
                    };
                    let border = egui::Color32::from_rgba_unmultiplied(
                        edge[0],
                        edge[1],
                        edge[2],
                        opacity_from_transparency(POC_BORDER_TRANSPARENCY),
                    );
                    painter.rect_stroke(
                        profile_box.rect,
                        0.0,
                        egui::Stroke::new(1.0, border),
                        egui::StrokeKind::Inside,
                    );
                }
            }
        }
        let left = origin.x + viewport.index_to_x(anchor.saturating_sub(config.width as usize));
        let right = origin.x + viewport.index_to_x(anchor);
        if config.show_value_area {
            let stroke = egui::Stroke::new(style.line_width(), style.color(theme, "slow")?);
            for level in [profile.vah, profile.val].into_iter().flatten() {
                if let Some(y) = viewport.windowed_y(level) {
                    painter.line_segment(
                        [
                            egui::pos2(left, origin.y + y),
                            egui::pos2(right, origin.y + y),
                        ],
                        stroke,
                    );
                }
            }
        }
        if config.show_poc {
            if let Some(price) = profile.poc_price() {
                if let Some(y) = viewport.windowed_y(price) {
                    painter.line_segment(
                        [
                            egui::pos2(left, origin.y + y),
                            egui::pos2(right, origin.y + y),
                        ],
                        egui::Stroke::new(style.line_width(), style.color(theme, "fast")?),
                    );
                }
            }
        }
        if config.show_vwap {
            if let Some(series) = evaluation
                .series
                .iter()
                .find(|series| series.node == "dvol:vwap")
            {
                let len = series.values.len().saturating_sub(base);
                let window = visible_rows(series.values.as_slice(), base, len);
                // The anchored VWAP paints through the shared sampler pair
                // like every line-like series: one vertex per column run at
                // sub-pixel zoom, the twice-visible borrow above it
                // (display-only cull, same pixels fewer shapes).
                let points = if needs_column_aggregation(viewport.scale().bar_spacing()) {
                    sampled_line_points(window, viewport, base)
                } else {
                    proportional_line_points(window, viewport, width_px)
                };
                let placed = translate_points(points, (origin.x, origin.y));
                paint_oscillator_styled(painter, &placed, "fast", theme, style, 1.0)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../tests/delta_volume_profile.rs"]
mod tests;
