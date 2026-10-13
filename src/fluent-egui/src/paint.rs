//! Paint edge: headless geometry plus thin `egui` emitters.
//!
//! Two layers, one computation. The geometry builders below are pure
//! pixels-out over the headless views (no `Painter`, no screenshots needed
//! to test them): every non-finite or holed point is skipped, never
//! substituted, and every output lines up index-for-index with its input.
//!
//! Placement contract: geometry is pane-local with its origin at the pane's
//! top-left (`index_to_x` starts at bar zero, `price_to_y` starts at the
//! pane top). The `paint_*` emitters draw exactly the coordinates they are
//! given, so the caller shifts each layer by its pane rect's origin before
//! painting — nothing here knows where its pane sits on screen.
//! The `paint_*` emitters map that geometry onto an `egui::Painter`
//! with colors resolved from [`ThemeConfig`] style keys — display `f64`
//! only, no kernel calls, no money state. Candle tint
//! comes from the ribbon trend key, never from a decision.
//!
//! Pane mapping reuses the caller's [`Viewport`]: price panes map prices,
//! zero-based panes (delta, oscillators) map through a viewport whose range
//! spans zero. Painters clip to their rect so out-of-window coordinates can
//! never draw outside the pane.

use protopyne_chart_model::ChartRow;

use crate::config::ThemeConfig;
use crate::error::EguiError;
use crate::view::{DeltaView, PivotMark, PivotSide, RibbonView, ribbon_style_key};
use crate::viewport::{BreakLine, Viewport};

/// One candle's screen geometry plus its tint keys.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CandleGeom {
    /// Bar index.
    pub index: usize,
    /// X pixel of the bar center.
    pub x: f32,
    /// Y pixels of the four legs.
    pub open_y: f32,
    /// Y pixel of the high wick tip.
    pub high_y: f32,
    /// Y pixel of the low wick tip.
    pub low_y: f32,
    /// Y pixels of close.
    pub close_y: f32,
    /// Whether close sits at/above open.
    pub up: bool,
    /// Ribbon trend key at this bar (momentum tint source).
    pub trend_bullish: bool,
}

/// Build candle geometry for converted rows.
///
/// `ribbon_bullish` must line up with `rows` (holes read bearish); bars with
/// an unmappable leg are skipped. Refuses length mismatch.
pub fn candle_geoms(
    rows: &[ChartRow],
    ribbon_bullish: &[bool],
    viewport: &Viewport,
) -> Result<Vec<CandleGeom>, EguiError> {
    if rows.len() != ribbon_bullish.len() {
        return Err(EguiError::InvalidParam {
            what: "series".to_owned(),
            reason: "rows and ribbon keys lengths differ".to_owned(),
        });
    }
    let mut geoms = Vec::with_capacity(rows.len());
    for (index, (row, trend)) in rows.iter().zip(ribbon_bullish.iter()).enumerate() {
        let (Some(open_y), Some(high_y), Some(low_y), Some(close_y)) = (
            viewport.price_to_y(row.open),
            viewport.price_to_y(row.high),
            viewport.price_to_y(row.low),
            viewport.price_to_y(row.close),
        ) else {
            continue;
        };
        geoms.push(CandleGeom {
            index,
            x: viewport.index_to_x(index),
            open_y,
            high_y,
            low_y,
            close_y,
            up: row.close >= row.open,
            trend_bullish: *trend,
        });
    }
    Ok(geoms)
}

/// One ribbon fill strip between the band edges.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RibbonBand {
    /// Bar index.
    pub index: usize,
    /// X pixel of the bar center.
    pub x: f32,
    /// Y pixel of the upper edge (smaller y: screens grow downward).
    pub top_y: f32,
    /// Y pixel of the lower edge.
    pub bottom_y: f32,
}

/// Build ribbon fill strips; holes in either edge yield `None`.
///
/// Length matches the band inputs element-wise. `top_y <= bottom_y`
/// wherever both edges map.
pub fn ribbon_bands(ribbon: &RibbonView, viewport: &Viewport) -> Vec<Option<RibbonBand>> {
    ribbon
        .upper
        .iter()
        .zip(ribbon.lower.iter())
        .enumerate()
        .map(|(index, (up, lo))| {
            let (Some(upper), Some(lower)) = (*up, *lo) else {
                return None;
            };
            let (Some(top_y), Some(bottom_y)) =
                (viewport.price_to_y(upper), viewport.price_to_y(lower))
            else {
                return None;
            };
            Some(RibbonBand { index, x: viewport.index_to_x(index), top_y, bottom_y })
        })
        .collect()
}

/// One zero-based delta bar: heights above/below the zero line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeltaBarGeom {
    /// Bar index.
    pub index: usize,
    /// X pixel of the bar center.
    pub x: f32,
    /// Y pixel of the buy top (`<= zero_y`); `None` on holes.
    pub buy_y: Option<f32>,
    /// Y pixel of the sell bottom (`>= zero_y`); `None` on holes.
    pub sell_y: Option<f32>,
    /// Y pixel of the zero line.
    pub zero_y: f32,
}

/// Build zero-based delta bars; sell depths plot downward from zero.
///
/// Refuses split-length mismatch and a viewport whose range does not span
/// zero (bars need a zero line to hang from).
pub fn delta_bar_geoms(
    delta: &DeltaView,
    viewport: &Viewport,
) -> Result<Vec<DeltaBarGeom>, EguiError> {
    if delta.buy_top.len() != delta.sell_bottom.len() {
        return Err(EguiError::InvalidParam {
            what: "series".to_owned(),
            reason: "buy and sell split lengths differ".to_owned(),
        });
    }
    let Some(zero_y) = viewport.price_to_y(0.0) else {
        return Err(EguiError::InvalidParam {
            what: "range".to_owned(),
            reason: "delta pane range must span zero".to_owned(),
        });
    };
    // The canonical scale extrapolates, so an off-window zero still maps:
    // refuse unless zero sits inside the visible range.
    let window = viewport.scale().range();
    if window.min > 0.0 || window.max < 0.0 {
        return Err(EguiError::InvalidParam {
            what: "range".to_owned(),
            reason: "delta pane range must span zero".to_owned(),
        });
    }
    let mut bars = Vec::with_capacity(delta.buy_top.len());
    for (index, (buy, sell)) in delta.buy_top.iter().zip(delta.sell_bottom.iter()).enumerate() {
        let buy_y = (*buy)
            .filter(|top| top.is_finite())
            .and_then(|top| viewport.price_to_y(top));
        let sell_y = (*sell)
            .filter(|depth| depth.is_finite())
            .and_then(|depth| viewport.price_to_y(-depth));
        bars.push(DeltaBarGeom { index, x: viewport.index_to_x(index), buy_y, sell_y, zero_y });
    }
    Ok(bars)
}

/// Map a zero-centered oscillator series to screen points; holes and
/// non-finite readings yield `None` so the emitter lifts the pen.
#[must_use]
pub fn oscillator_points(
    values: &[Option<f64>],
    viewport: &Viewport,
) -> Vec<Option<(f32, f32)>> {
    values
        .iter()
        .enumerate()
        .map(|(index, slot)| {
            let value = (*slot)?;
            if !value.is_finite() {
                return None;
            }
            let y = viewport.price_to_y(value)?;
            Some((viewport.index_to_x(index), y))
        })
        .collect()
}

/// One pivot dot on the oscillator pane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PivotDot {
    /// Bar index of the crossing.
    pub index: usize,
    /// X pixel of the bar.
    pub x: f32,
    /// Y pixel of the fast value at the crossing.
    pub y: f32,
    /// Which side crossed.
    pub side: PivotSide,
}

/// Place pivot dots from crossing marks onto fast-oscillator values.
///
/// Marks whose value is missing, non-finite, out of range, or out of bounds
/// are skipped — dots decorate crossings only.
#[must_use]
pub fn pivot_dots(
    marks: &[PivotMark],
    fast: &[Option<f64>],
    viewport: &Viewport,
) -> Vec<PivotDot> {
    marks
        .iter()
        .filter_map(|mark| {
            let value = (*fast.get(mark.index)?)?;
            if !value.is_finite() {
                return None;
            }
            let y = viewport.price_to_y(value)?;
            Some(PivotDot { index: mark.index, x: viewport.index_to_x(mark.index), y, side: mark.side })
        })
        .collect()
}

/// Y pixel of a `±threshold` rule; `None` when the threshold is not a
/// positive finite value or sits outside the pane range.
#[must_use]
pub fn threshold_line(threshold: f64, viewport: &Viewport) -> Option<f32> {
    if !threshold.is_finite() || threshold <= 0.0 {
        return None;
    }
    viewport.price_to_y(threshold)
}

/// Resolve a theme style key to an `egui` color, parsing the validated
/// `#RRGGBB` slot. Unknown keys or unparsable slots fall back to gray —
/// paint stays fail-soft while validation upstream stays fail-closed.
#[must_use]
pub fn theme_color(theme: &ThemeConfig, key: &str) -> egui::Color32 {
    let Some(slot) = theme.style(key) else {
        return egui::Color32::GRAY;
    };
    parse_hex(slot).unwrap_or(egui::Color32::GRAY)
}

fn parse_hex(slot: &str) -> Option<egui::Color32> {
    let bytes = slot.as_bytes();
    if bytes.len() != 7 || bytes[0] != b'#' {
        return None;
    }
    let channel = |pair: &[u8]| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok();
    Some(egui::Color32::from_rgb(
        channel(&bytes[1..3])?,
        channel(&bytes[3..5])?,
        channel(&bytes[5..7])?,
    ))
}

/// Paint candles: wick line plus filled body (minimum one pixel tall, per
/// the reference min-visibility rule), tinted by the ribbon trend key.
pub fn paint_candles(
    painter: &egui::Painter,
    geoms: &[CandleGeom],
    body_width: f32,
    theme: &ThemeConfig,
) {
    for geom in geoms {
        let color = theme_color(theme, ribbon_style_key(geom.trend_bullish));
        painter.line_segment(
            [egui::Pos2::new(geom.x, geom.high_y), egui::Pos2::new(geom.x, geom.low_y)],
            egui::Stroke::new(1.0, color),
        );
        let top = geom.open_y.min(geom.close_y);
        let bottom = (geom.open_y.max(geom.close_y)).max(top + 1.0);
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::Pos2::new(geom.x - body_width / 2.0, top),
                egui::Pos2::new(geom.x + body_width / 2.0, bottom),
            ),
            0.0,
            color,
        );
    }
}

/// Paint ribbon fill strips between the band edges at `alpha` opacity.
pub fn paint_ribbon(
    painter: &egui::Painter,
    bands: &[Option<RibbonBand>],
    half_width: f32,
    theme: &ThemeConfig,
    bullish: &[bool],
    alpha: f32,
) {
    let clamped = alpha.clamp(0.0, 1.0);
    for (band, trend) in bands.iter().zip(bullish.iter()) {
        let Some(strip) = band else {
            continue;
        };
        let base = theme_color(theme, ribbon_style_key(*trend));
        let fill = egui::Color32::from_rgba_unmultiplied(
            base.r(),
            base.g(),
            base.b(),
            (clamped * 255.0).round().clamp(0.0, 255.0) as u8,
        );
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::Pos2::new(strip.x - half_width, strip.top_y),
                egui::Pos2::new(strip.x + half_width, strip.bottom_y),
            ),
            0.0,
            fill,
        );
    }
}

/// Paint zero-based delta bars: buys rise from zero, sells extend downward.
pub fn paint_delta_bars(
    painter: &egui::Painter,
    bars: &[DeltaBarGeom],
    half_width: f32,
    theme: &ThemeConfig,
) {
    for bar in bars {
        if let Some(buy_y) = bar.buy_y {
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::Pos2::new(bar.x - half_width, buy_y),
                    egui::Pos2::new(bar.x + half_width, bar.zero_y),
                ),
                0.0,
                theme_color(theme, "bull"),
            );
        }
        if let Some(sell_y) = bar.sell_y {
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::Pos2::new(bar.x - half_width, bar.zero_y),
                    egui::Pos2::new(bar.x + half_width, sell_y),
                ),
                0.0,
                theme_color(theme, "bear"),
            );
        }
    }
}

/// Paint a zero-centered oscillator polyline under one style key, lifting
/// the pen across holes.
pub fn paint_oscillator(
    painter: &egui::Painter,
    points: &[Option<(f32, f32)>],
    style_key: &str,
    theme: &ThemeConfig,
) {
    let color = theme_color(theme, style_key);
    let stroke = egui::Stroke::new(1.0, color);
    for window in points.windows(2) {
        if let (Some((x0, y0)), Some((x1, y1))) = (window[0], window[1]) {
            painter.line_segment([egui::Pos2::new(x0, y0), egui::Pos2::new(x1, y1)], stroke);
        }
    }
}

/// Paint the `±threshold` rules across `[x0, x1]`.
pub fn paint_thresholds(
    painter: &egui::Painter,
    x0: f32,
    x1: f32,
    threshold: f64,
    viewport: &Viewport,
    theme: &ThemeConfig,
) {
    let color = theme_color(theme, "grid");
    let stroke = egui::Stroke::new(1.0, color);
    for signed in [threshold, -threshold] {
        if let Some(y) = viewport.price_to_y(signed) {
            if y.is_finite() {
                painter.line_segment([egui::Pos2::new(x0, y), egui::Pos2::new(x1, y)], stroke);
            }
        }
    }
}

/// Paint pivot dots: bear color above, bull color below.
pub fn paint_pivot_dots(painter: &egui::Painter, dots: &[PivotDot], theme: &ThemeConfig) {
    for dot in dots {
        let key = match dot.side {
            PivotSide::High => "bear",
            PivotSide::Low => "bull",
        };
        painter.circle_filled(egui::Pos2::new(dot.x, dot.y), 4.0, theme_color(theme, key));
    }
}

/// Paint session-break gridlines over the pane's full height.
pub fn paint_session_breaks(
    painter: &egui::Painter,
    lines: &[BreakLine],
    top_y: f32,
    bottom_y: f32,
    theme: &ThemeConfig,
) {
    let stroke = egui::Stroke::new(1.0, theme_color(theme, "grid"));
    for line in lines {
        painter.line_segment(
            [egui::Pos2::new(line.x, top_y), egui::Pos2::new(line.x, bottom_y)],
            stroke,
        );
    }
}

#[cfg(test)]
#[path = "../tests/paint.rs"]
mod tests;
