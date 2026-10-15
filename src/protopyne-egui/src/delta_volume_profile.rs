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
use protopyne_types::{Validate, check_for_execute, valid_output};

use fluent_egui::{
    EguiError, ReadoutValue, SlotAttachment, ThemeConfig, Viewport, paint_oscillator, resolve_color,
};

/// Plan-node namespace for the profile's per-bar primitive series.
pub const PROFILE_NODES: [&str; 6] =
    ["dvol:high", "dvol:low", "dvol:close", "dvol:volume", "dvol:buy", "dvol:vwap"];

/// Stable plugin id (also the hot-slot id).
pub const DELTA_VOLUME_PROFILE_ID: &str = "delta_volume_profile";

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
        VolumeBin { price_low: low, price_high: low, volume: 0.0, imbalance: 0.0 };
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
        let buy = if bar.buy.is_finite() { bar.buy.clamp(0.0, total) } else { total * 0.5 };
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
        bins_out.iter().enumerate().max_by(|a, b| a.1.volume.total_cmp(&b.1.volume)).map(|(i, _)| i)
    } else {
        None
    };
    let (vah, val) = poc.map_or((None, None), |index| value_area(&bins_out, index, value_area_pct));
    let vwap = (vwap_den > 0.0).then_some(vwap_num / vwap_den);
    DeltaVolumeProfile { bins: bins_out, poc, vah, val, vwap, max_volume }
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
        let below = if low > 0 { bins[low - 1].volume } else { f64::NEG_INFINITY };
        let above = if high + 1 < bins.len() { bins[high + 1].volume } else { f64::NEG_INFINITY };
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

/// One painted profile bar: a rectangle at the chart's right edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProfileBox {
    /// Rectangle in pane-local pixels.
    pub rect: egui::Rect,
    /// Theme style key (`bull` for buy-dominant, `bear` for sell-dominant).
    pub style: &'static str,
    /// Paint opacity within `[0, 1]`; thicker volume reads brighter.
    pub alpha: f32,
    /// Whether this is the point-of-control bin.
    pub poc: bool,
}

/// Lay a profile out as horizontal bars anchored at `anchor_index`,
/// extending leftward by up to `width_slots` bars.
///
/// `origin` is the pane's top-left. Off-window bins are skipped so the profile
/// never paints past the visible price range. A degenerate profile (no bins or
/// no volume) yields no boxes.
#[must_use]
pub fn profile_boxes(
    profile: &DeltaVolumeProfile,
    viewport: &Viewport,
    origin: egui::Pos2,
    anchor_index: usize,
    width_slots: i64,
) -> Vec<ProfileBox> {
    if profile.bins.is_empty() || profile.max_volume <= 0.0 || width_slots <= 0 {
        return Vec::new();
    }
    let anchor_x = origin.x + viewport.index_to_x(anchor_index);
    let max_width = width_slots as f32 * viewport.scale().bar_spacing();
    let mut boxes = Vec::with_capacity(profile.bins.len());
    for (index, bin) in profile.bins.iter().enumerate() {
        if bin.volume <= 0.0 {
            continue;
        }
        let (Some(y_high), Some(y_low)) =
            (viewport.windowed_y(bin.price_high), viewport.windowed_y(bin.price_low))
        else {
            continue;
        };
        let (top, bottom) = (y_high.min(y_low), y_high.max(y_low));
        let width = ((bin.volume / profile.max_volume) as f32 * max_width).max(1.0);
        let rect = egui::Rect::from_min_max(
            egui::pos2(anchor_x - width, origin.y + top),
            egui::pos2(anchor_x, origin.y + bottom),
        );
        let alpha = (0.2 + 0.6 * (bin.volume / profile.max_volume) as f32).clamp(0.0, 1.0);
        boxes.push(ProfileBox {
            rect,
            style: if bin.imbalance >= 0.0 { "bull" } else { "bear" },
            alpha,
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
    Debug, Clone, bon::Builder, serde::Serialize, serde::Deserialize, FieldAccess, Describable
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
        Ok(valid_output(format!("delta volume profile ready ({} bins)", self.bins)))
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

/// Rebuild the profile from a windowed evaluation's primitive series.
fn profile_from_series(
    evaluation: &crate::indicators::IndicatorEvaluation,
    bins: usize,
    value_area_pct: f64,
) -> DeltaVolumeProfile {
    let series = |node: &str| {
        evaluation
            .series
            .iter()
            .find(|series| series.node == node)
            .map(|series| series.values.as_slice())
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
    let count = high.len().min(low.len()).min(close.len()).min(volume.len()).min(buy.len());
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
        Ok(PROFILE_NODES.iter().map(|node| (*node).to_owned()).collect())
    }

    fn evaluate(
        &self,
        rows: &[ChartRow],
        config: &dyn Component,
    ) -> Result<crate::indicators::IndicatorEvaluation, EguiError> {
        let config = fluent_wvr::component_downcast_ref::<DeltaVolumeProfileConfig>(config)
            .ok_or_else(|| EguiError::InvalidParam {
                what: "config".to_owned(),
                reason: format!("{} expects a DeltaVolumeProfile configuration", self.id()),
            })?;
        let series = profile_series(rows, config)?;
        let fingerprint = common_core::hash::fnv1a64(
            &serde_json::to_vec(config).unwrap_or_default(),
        );
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

    fn readouts(
        &self,
        evaluation: &crate::indicators::IndicatorEvaluation,
    ) -> Vec<ReadoutValue> {
        let defaults = DeltaVolumeProfileConfig::default();
        let profile =
            profile_from_series(evaluation, defaults.bins as usize, defaults.value_area_pct);
        vec![
            ReadoutValue { label: "POC".to_owned(), value: profile.poc_price() },
            ReadoutValue { label: "VWAP".to_owned(), value: profile.vwap },
        ]
    }

    fn paint_main_overlay(
        &self,
        painter: &egui::Painter,
        evaluation: &crate::indicators::IndicatorEvaluation,
        viewport: &Viewport,
        origin: egui::Pos2,
        theme: &ThemeConfig,
        config: &dyn Component,
    ) -> Result<(), EguiError> {
        let config = fluent_wvr::component_downcast_ref::<DeltaVolumeProfileConfig>(config)
            .ok_or_else(|| EguiError::InvalidParam {
                what: "config".to_owned(),
                reason: "delta_volume_profile expects a DeltaVolumeProfile configuration".to_owned(),
            })?;
        let profile =
            profile_from_series(evaluation, config.bins as usize, config.value_area_pct);
        if profile.bins.is_empty() {
            return Ok(());
        }
        let count = evaluation.series.first().map_or(0, |series| series.values.len());
        let anchor = count.saturating_sub(1) + config.offset as usize;
        if config.show_profile {
            for profile_box in profile_boxes(&profile, viewport, origin, anchor, config.width) {
                let base = resolve_color(theme, profile_box.style)?;
                let color = base.gamma_multiply(profile_box.alpha);
                painter.rect_filled(profile_box.rect, 0.0, color);
                if profile_box.poc {
                    painter.rect_stroke(
                        profile_box.rect,
                        0.0,
                        egui::Stroke::new(1.0, base),
                        egui::StrokeKind::Inside,
                    );
                }
            }
        }
        let left = origin.x + viewport.index_to_x(anchor.saturating_sub(config.width as usize));
        let right = origin.x + viewport.index_to_x(anchor);
        if config.show_value_area {
            for level in [profile.vah, profile.val].into_iter().flatten() {
                if let Some(y) = viewport.windowed_y(level) {
                    painter.line_segment(
                        [egui::pos2(left, origin.y + y), egui::pos2(right, origin.y + y)],
                        egui::Stroke::new(1.0, resolve_color(theme, "slow")?),
                    );
                }
            }
        }
        if config.show_poc {
            if let Some(price) = profile.poc_price() {
                if let Some(y) = viewport.windowed_y(price) {
                    painter.line_segment(
                        [egui::pos2(left, origin.y + y), egui::pos2(right, origin.y + y)],
                        egui::Stroke::new(1.0, resolve_color(theme, "fast")?),
                    );
                }
            }
        }
        if config.show_vwap {
            if let Some(series) = evaluation.series.iter().find(|series| series.node == "dvol:vwap") {
                let points: Vec<Option<(f32, f32)>> = series
                    .values
                    .iter()
                    .enumerate()
                    .map(|(index, slot)| {
                        let value = (*slot)?;
                        let y = viewport.windowed_y(value)?;
                        Some((origin.x + viewport.index_to_x(index), origin.y + y))
                    })
                    .collect();
                paint_oscillator(painter, &points, "fast", theme, 1.0)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../tests/delta_volume_profile.rs"]
mod tests;
