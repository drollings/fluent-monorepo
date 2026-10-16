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

use protopyne_chart_model::{Annotation, AnnotationStyle, ChartRow};

use crate::config::ThemeConfig;
use crate::error::EguiError;
use crate::hittest::{Crosshair, crosshair_pixel};
use crate::view::{
    DeltaView, PivotMark, PivotSide, RibbonView, ribbon_fill_style_key, ribbon_style_key,
};
use crate::viewport::{BreakLine, Viewport};
use crate::style::IndicatorStyle;

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
    // `windowed_y` refuses off-window prices, so a zero outside the visible
    // range refuses here instead of extrapolating a zero line off-pane.
    // The inverse, `y_to_price`, refuses off-pane pixels outright.
    let Some(zero_y) = viewport.windowed_y(0.0) else {
        return Err(EguiError::InvalidParam {
            what: "range".to_owned(),
            reason: "delta pane range must span zero".to_owned(),
        });
    };
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

/// Map a zero-centered oscillator series to screen points; holes,
/// non-finite readings, and off-window values yield `None` so the emitter
/// lifts the pen. Off-window legs refuse through the shared window
/// predicate rather than extrapolating off-pane.
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
            let y = viewport.windowed_y(value)?;
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
/// are skipped — dots decorate crossings only. Off-window values refuse
/// through the shared window predicate, so the dots lift the pen exactly
/// where [`oscillator_points`] does.
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
            let y = viewport.windowed_y(value)?;
            Some(PivotDot { index: mark.index, x: viewport.index_to_x(mark.index), y, side: mark.side })
        })
        .collect()
}

/// Place pivot dots at bar price extremes, mirroring the reference overlay
/// labels: `High` marks sit on the bar high, `Low` marks on the bar low.
///
/// Marks past the rows, on holes, or with unmappable extremes are skipped —
/// dots decorate confirmed extremes only.
#[must_use]
pub fn pivot_price_dots(
    marks: &[PivotMark],
    rows: &[ChartRow],
    viewport: &Viewport,
) -> Vec<PivotDot> {
    // `windowed_y` refuses off-window prices, so an extreme outside the
    // visible window places no dot; skip those, as promised.
    marks
        .iter()
        .filter_map(|mark| {
            let row = rows.get(mark.index)?;
            let extreme = match mark.side {
                PivotSide::High => row.high,
                PivotSide::Low => row.low,
            };
            if !extreme.is_finite() {
                return None;
            }
            let y = viewport.windowed_y(extreme)?;
            Some(PivotDot { index: mark.index, x: viewport.index_to_x(mark.index), y, side: mark.side })
        })
        .collect()
}

/// Y pixel of a `±threshold` rule; `None` when the threshold is not a
/// positive finite value or sits outside the pane range. The off-window
/// refusal rides the shared window predicate, so every rule emitter
/// inherits it rather than re-deciding containment.
#[must_use]
pub fn threshold_line(threshold: f64, viewport: &Viewport) -> Option<f32> {
    if !threshold.is_finite() || threshold <= 0.0 {
        return None;
    }
    viewport.windowed_y(threshold)
}

/// Resolve a theme style key to an `egui` color, parsing the `#RRGGBB`
/// slot. Unknown keys and unparsable slots refuse with the key named —
/// paint never renders a silent gray default (declared behavior change:
/// hand-built or drifted themes error visibly through
/// [`paint_error_chip`]).
pub fn resolve_color(theme: &ThemeConfig, key: &str) -> Result<egui::Color32, EguiError> {
    let Some(slot) = theme.style(key) else {
        return Err(EguiError::InvalidParam {
            what: key.to_owned(),
            reason: "unknown theme style key".to_owned(),
        });
    };
    parse_hex_color(key, slot)
}

/// Parse one `#RRGGBB` slot, naming `key` in the refusal. The one hex reader
/// in the crate: the theme resolver and a per-indicator style override both
/// go through it, so a color that paints from a theme and one that paints
/// from an override are read by the same rules.
pub fn parse_hex_color(key: &str, slot: &str) -> Result<egui::Color32, EguiError> {
    let invalid = || EguiError::InvalidParam {
        what: key.to_owned(),
        reason: format!("{slot:?} is not a #RRGGBB color"),
    };
    let bytes = slot.as_bytes();
    if bytes.len() != 7 || bytes[0] != b'#' {
        return Err(invalid());
    }
    let channel = |pair: &[u8]| {
        let text = std::str::from_utf8(pair).map_err(|_| invalid())?;
        u8::from_str_radix(text, 16).map_err(|_| invalid())
    };
    Ok(egui::Color32::from_rgb(channel(&bytes[1..3])?, channel(&bytes[3..5])?, channel(
        &bytes[5..7],
    )?))
}

/// Narrowest a candle body may paint, so a fully zoomed-out bar is still a
/// visible mark rather than nothing at all.
pub const CANDLE_MIN_BODY_PX: f32 = 1.0;
/// Time-bar width above which neighbouring candles stop reading as one solid
/// block and start reading as discrete bars, so the body takes a share of
/// the slot instead of the whole of it.
pub const CANDLE_GAP_THRESHOLD_PX: f32 = 20.0;
/// Share of the time bar a candle body takes once the bar is wide enough for
/// the gap between neighbours to read.
pub const CANDLE_BODY_FRACTION: f32 = 0.8;

/// The candle body width for a displayed time-bar width.
///
/// A candle is the width of the unit of time it draws, so the body *is* the
/// slot at ordinary zoom and the bars read as a solid tape. Past
/// [`CANDLE_GAP_THRESHOLD_PX`] the slot is wide enough that neighbours run
/// together, so the body drops to [`CANDLE_BODY_FRACTION`] of it and the gap
/// separates them. The width never falls below [`CANDLE_MIN_BODY_PX`]: a
/// fully zoomed-out bar stays a visible mark rather than disappearing, which
/// would hide data by zooming.
///
/// `bar_spacing` is the *live* spacing from the pane's own viewport, so the
/// body follows the horizontal zoom the chart currently shows rather than the
/// configured default. Non-finite spacing reads the minimum.
#[must_use]
pub fn candle_body_width(bar_spacing: f32) -> f32 {
    if !bar_spacing.is_finite() {
        return CANDLE_MIN_BODY_PX;
    }
    let spaced =
        if bar_spacing > CANDLE_GAP_THRESHOLD_PX { bar_spacing * CANDLE_BODY_FRACTION } else { bar_spacing };
    spaced.max(CANDLE_MIN_BODY_PX)
}

/// One wick-plus-body candle in a resolved color: the single body behind
/// the trend-keyed, single-key, and per-candle-tint emitters (minimum one
/// pixel tall, per the reference min-visibility rule). Color resolution
/// stays at the call site — this draws exactly what it is given.
fn paint_one_candle(
    painter: &egui::Painter,
    geom: &CandleGeom,
    body_width: f32,
    color: egui::Color32,
) {
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

/// Paint candles: wick line plus filled body (minimum one pixel tall, per
/// the reference min-visibility rule), tinted by the ribbon trend key.
pub fn paint_candles(
    painter: &egui::Painter,
    geoms: &[CandleGeom],
    body_width: f32,
    theme: &ThemeConfig,
) -> Result<(), EguiError> {
    let bull = resolve_color(theme, ribbon_style_key(true))?;
    let bear = resolve_color(theme, ribbon_style_key(false))?;
    for geom in geoms {
        paint_one_candle(painter, geom, body_width, if geom.trend_bullish { bull } else { bear });
    }
    Ok(())
}

/// One candle plus its resolved paint color.
///
/// Pairs headless geometry with the display color the caller resolved for
/// it (theme key, momentum tint, or fallback): the emitter draws exactly
/// what it is given, like every painter here.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TintedCandle {
    /// Candle geometry in UI space (caller shifts pane-local geometry).
    pub geom: CandleGeom,
    /// Resolved body and wick color.
    pub color: egui::Color32,
}

/// Paint candles in individual colors.
///
/// Same min-visibility body rule as [`paint_candles`]; each candle carries
/// its own resolved color, so per-bar tints (momentum gradients, conviction
/// fades) need no grouping pass.
pub fn paint_candles_tinted(
    painter: &egui::Painter,
    candles: &[TintedCandle],
    body_width: f32,
) {
    for candle in candles {
        paint_one_candle(painter, &candle.geom, body_width, candle.color);
    }
}

/// Paint candles under one explicit style key.
///
/// Same min-visibility body rule as [`paint_candles`]; the caller groups
/// geometry by display key (for example the four momentum tints) and paints
/// one pass per key.
pub fn paint_candles_as(
    painter: &egui::Painter,
    geoms: &[CandleGeom],
    style_key: &str,
    body_width: f32,
    theme: &ThemeConfig,
) -> Result<(), EguiError> {
    let color = resolve_color(theme, style_key)?;
    for geom in geoms {
        paint_one_candle(painter, geom, body_width, color);
    }
    Ok(())
}

/// Paint ribbon fill strips between the band edges at `alpha` opacity.
pub fn paint_ribbon(
    painter: &egui::Painter,
    bands: &[Option<RibbonBand>],
    half_width: f32,
    theme: &ThemeConfig,
    bullish: &[bool],
    alpha: f32,
) -> Result<(), EguiError> {
    let opacity = alpha_u8(alpha);
    let bull = resolve_color(theme, ribbon_fill_style_key(true))?;
    let bear = resolve_color(theme, ribbon_fill_style_key(false))?;
    for (band, trend) in bands.iter().zip(bullish.iter()) {
        let Some(strip) = band else {
            continue;
        };
        let base = if *trend { bull } else { bear };
        let fill = egui::Color32::from_rgba_unmultiplied(
            base.r(),
            base.g(),
            base.b(),
            opacity,
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
    Ok(())
}

/// One ribbon fill run: a closed polygon between the band edges over a
/// contiguous stretch of bars sharing one trend key.
///
/// `points` walks the upper edge forward, then the lower edge reversed, so
/// the emitter fills exactly the band the reference `fill()` shades —
/// a continuous area, never per-bar candle lookalikes. Pane-local, like
/// every geometry here: the caller shifts points by the pane origin.
#[derive(Debug, Clone, PartialEq)]
pub struct RibbonPolygon {
    /// Bar indices covered by this run (contiguous).
    pub indices: Vec<usize>,
    /// Closed polygon points in pane-local pixels.
    pub points: Vec<(f32, f32)>,
    /// Trend key shared by the whole run.
    pub bullish: bool,
}

/// Build ribbon fill polygons: maximal runs of contiguous bars whose edges
/// both map and whose trend key is constant.
///
/// Holes, non-finite legs, and unmappable edges break runs (never bridged);
/// a trend flip starts a new run so each polygon paints one flat color.
/// Runs shorter than two bars still emit (degenerate but harmless); fully
/// holed inputs yield no polygons.
#[must_use]
pub fn ribbon_polygons(ribbon: &RibbonView, viewport: &Viewport) -> Vec<RibbonPolygon> {
    close_runs(
        ribbon
            .upper
            .iter()
            .zip(ribbon.lower.iter())
            .zip(ribbon.bullish.iter())
            .map(|((upper, lower), trend)| (*upper, *lower, *trend)),
        viewport,
    )
}

/// Build area fill polygons: maximal runs of contiguous bars whose values
/// map, closed against one constant baseline.
///
/// Same run-closing core as [`ribbon_polygons`] — no second polygon code —
/// with the lower edge fixed at `baseline` and the trend key fixed at
/// `false`, so no per-bar lower/trend vectors are materialized. Holes,
/// non-finite values, and unmappable edges break runs (never bridged). A
/// non-finite baseline maps nowhere, so every run breaks and nothing
/// emits; callers wanting a named refusal validate through their own
/// boundary (the terminal's `area_view` refuses it as `baseline`).
#[must_use]
pub fn area_polygons(
    values: &[Option<f64>],
    baseline: f64,
    viewport: &Viewport,
) -> Vec<RibbonPolygon> {
    let lower = baseline.is_finite().then_some(baseline);
    close_runs(values.iter().map(|upper| (*upper, lower, false)), viewport)
}

/// One run-closing core behind ribbon and area fills: maximal runs of
/// contiguous mappable `(upper, lower, trend)` legs. Holes, non-finite
/// legs, and unmappable or off-window edges break runs; a trend flip starts
/// a new run. Off-window legs refuse through the shared window predicate
/// rather than extrapolating off-pane, so fills lift the pen at the window
/// edge exactly where oscillator and threshold emitters do.
fn close_runs(
    legs: impl IntoIterator<Item = (Option<f64>, Option<f64>, bool)>,
    viewport: &Viewport,
) -> Vec<RibbonPolygon> {
    let mut polygons = Vec::new();
    let mut upper_run: Vec<(f32, f32)> = Vec::new();
    let mut lower_run: Vec<(f32, f32)> = Vec::new();
    let mut run_indices: Vec<usize> = Vec::new();
    let mut run_trend = false;
    let mut open = false;

    let flush = |polygons: &mut Vec<RibbonPolygon>,
                 upper_run: &mut Vec<(f32, f32)>,
                 lower_run: &mut Vec<(f32, f32)>,
                 run_indices: &mut Vec<usize>,
                 run_trend: bool,
                 open: &mut bool| {
        if *open && !run_indices.is_empty() {
            let mut points = std::mem::take(upper_run);
            let mut lower: Vec<(f32, f32)> = std::mem::take(lower_run);
            lower.reverse();
            points.extend(lower);
            polygons.push(RibbonPolygon {
                indices: std::mem::take(run_indices),
                points,
                bullish: run_trend,
            });
        } else {
            upper_run.clear();
            lower_run.clear();
            run_indices.clear();
        }
        *open = false;
    };

    for (index, (upper, lower, trend)) in legs.into_iter().enumerate() {
        let mappable = match (upper, lower) {
            (Some(up), Some(lo)) if up.is_finite() && lo.is_finite() => {
                match (viewport.windowed_y(up), viewport.windowed_y(lo)) {
                    (Some(top_y), Some(bottom_y)) => {
                        Some((viewport.index_to_x(index), top_y, bottom_y))
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        let Some((x, top_y, bottom_y)) = mappable else {
            flush(&mut polygons, &mut upper_run, &mut lower_run, &mut run_indices, run_trend, &mut open);
            continue;
        };
        if !open || run_trend != trend {
            flush(&mut polygons, &mut upper_run, &mut lower_run, &mut run_indices, run_trend, &mut open);
            run_trend = trend;
            open = true;
        }
        upper_run.push((x, top_y));
        lower_run.push((x, bottom_y));
        run_indices.push(index);
    }
    flush(&mut polygons, &mut upper_run, &mut lower_run, &mut run_indices, run_trend, &mut open);
    polygons
}

/// Paintable points of one fill run: `None` for degenerate polygons, which
/// paint nothing. The one `< 3 points` guard every polygon emitter reads,
/// so a second hand-rolled guard cannot drift from it.
fn paintable_points(polygon: &RibbonPolygon) -> Option<Vec<egui::Pos2>> {
    if polygon.points.len() < 3 {
        return None;
    }
    Some(polygon.points.iter().map(|(x, y)| egui::Pos2::new(*x, *y)).collect())
}

/// Paint ribbon fill runs as translucent areas between the band edges.
///
/// One flat color per run (`ribbon_bull`/`ribbon_bear` theme keys at
/// `alpha`), matching the reference shaded band. Single-bar runs paint as
/// thin quads through the same polygon path — no special case.
pub fn paint_ribbon_fill(
    painter: &egui::Painter,
    polygons: &[RibbonPolygon],
    theme: &ThemeConfig,
    alpha: f32,
) -> Result<(), EguiError> {
    let opacity = alpha_u8(alpha);
    let bull = resolve_color(theme, ribbon_fill_style_key(true))?;
    let bear = resolve_color(theme, ribbon_fill_style_key(false))?;
    for polygon in polygons {
        let Some(points) = paintable_points(polygon) else {
            continue;
        };
        let base = if polygon.bullish { bull } else { bear };
        let fill = egui::Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), opacity);
        painter.add(egui::Shape::convex_polygon(points, fill, egui::Stroke::NONE));
    }
    Ok(())
}

/// Blend one gradient vertex color between the top and bottom fill colors by
/// its y pixel: `top_y` reads exactly `top`, `bottom_y` exactly `bottom`,
/// between lerps per channel (alpha included), outside clamps to the nearer
/// end. A degenerate range reads `top` for every vertex instead of dividing
/// by zero. Non-finite `y`, `top_y`, or `bottom_y` refuse (`None`) exactly
/// as every sibling emitter skips non-finite input — a `NaN` must never
/// saturate into a black vertex. Blending runs in premultiplied space
/// (`Color32` storage), which is the correct composite for translucent
/// gradient stops. Pure color math over reused polygon points — no new
/// polygon code.
#[must_use]
pub fn area_vertex_color(
    top: egui::Color32,
    bottom: egui::Color32,
    y: f32,
    top_y: f32,
    bottom_y: f32,
) -> Option<egui::Color32> {
    if !y.is_finite() || !top_y.is_finite() || !bottom_y.is_finite() {
        return None;
    }
    let span = bottom_y - top_y;
    let ratio = if span.abs() < f32::EPSILON { 0.0 } else { ((y - top_y) / span).clamp(0.0, 1.0) };
    let blend = |high: u8, low: u8| {
        (f32::from(high) + (f32::from(low) - f32::from(high)) * ratio).round().clamp(0.0, 255.0)
            as u8
    };
    Some(egui::Color32::from_rgba_premultiplied(
        blend(top.r(), bottom.r()),
        blend(top.g(), bottom.g()),
        blend(top.b(), bottom.b()),
        blend(top.a(), bottom.a()),
    ))
}

/// Paint filled areas with a vertical color gradient: one mesh per polygon
/// over the shared polygon path (the per-vertex gradient needs a `Mesh`,
/// which the flat fill's `convex_polygon` cannot carry), vertex colors from
/// [`area_vertex_color`] over the polygon's own y span. Degenerate polygons
/// (< 3 points) paint nothing, like the flat fill, and a polygon with an
/// unblendable vertex paints nothing rather than a substituted color.
/// Colors arrive resolved — the caller composes [`resolve_color`] and
/// [`opacity_from_transparency`], so this emitter cannot fail.
pub fn paint_area_fill(
    painter: &egui::Painter,
    polygons: &[RibbonPolygon],
    top: egui::Color32,
    bottom: egui::Color32,
) {
    for polygon in polygons {
        let Some(vertices) = paintable_points(polygon) else {
            continue;
        };
        let top_y = polygon.points.iter().map(|(_, y)| *y).fold(f32::INFINITY, f32::min);
        let bottom_y =
            polygon.points.iter().map(|(_, y)| *y).fold(f32::NEG_INFINITY, f32::max);
        // Any unblendable vertex skips the polygon: a gradient mesh needs a
        // color per vertex, and no substituted color may stand in.
        let Some(colors): Option<Vec<egui::Color32>> = polygon
            .points
            .iter()
            .map(|(_, y)| area_vertex_color(top, bottom, *y, top_y, bottom_y))
            .collect()
        else {
            continue;
        };
        let mut mesh = egui::Mesh::default();
        mesh.vertices.reserve(vertices.len());
        mesh.indices.reserve(vertices.len().saturating_sub(2) * 3);
        for (point, color) in vertices.iter().zip(colors.iter()) {
            mesh.vertices.push(egui::epaint::Vertex {
                pos: *point,
                uv: egui::Pos2::ZERO,
                color: *color,
            });
        }
        for index in 1..vertices.len() - 1 {
            mesh.indices.extend_from_slice(&[0, index as u32, index as u32 + 1]);
        }
        painter.add(mesh);
    }
}

/// Paint zero-based delta bars: buys rise from zero, sells extend downward.
pub fn paint_delta_bars(
    painter: &egui::Painter,
    bars: &[DeltaBarGeom],
    half_width: f32,
    theme: &ThemeConfig,
) -> Result<(), EguiError> {
    let bull = resolve_color(theme, "bull")?;
    let bear = resolve_color(theme, "bear")?;
    for bar in bars {
        if let Some(buy_y) = bar.buy_y {
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::Pos2::new(bar.x - half_width, buy_y),
                    egui::Pos2::new(bar.x + half_width, bar.zero_y),
                ),
                0.0,
                bull,
            );
        }
        if let Some(sell_y) = bar.sell_y {
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::Pos2::new(bar.x - half_width, bar.zero_y),
                    egui::Pos2::new(bar.x + half_width, sell_y),
                ),
                0.0,
                bear,
            );
        }
    }
    Ok(())
}

/// One delta bar paired with the display color the caller resolved for it
/// (theme key, momentum tint, or fallback): the emitter draws exactly what it
/// is given, like every painter here.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TintedDeltaBar {
    /// Bar geometry in UI space (caller shifts pane-local geometry).
    pub geom: DeltaBarGeom,
    /// Body color applied to both legs.
    pub color: egui::Color32,
}

/// Paint zero-based delta bars in individual colors.
///
/// Both legs carry the same per-bar color, so a trend/conviction tint reads
/// across the histogram; call [`paint_delta_bars`] for the fixed bull/bear
/// split. Same leg rule as that emitter: buys rise from zero, sells extend
/// downward, holes lift the pen.
pub fn paint_delta_bars_tinted(
    painter: &egui::Painter,
    bars: &[TintedDeltaBar],
    half_width: f32,
) {
    for bar in bars {
        if let Some(buy_y) = bar.geom.buy_y {
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::Pos2::new(bar.geom.x - half_width, buy_y),
                    egui::Pos2::new(bar.geom.x + half_width, bar.geom.zero_y),
                ),
                0.0,
                bar.color,
            );
        }
        if let Some(sell_y) = bar.geom.sell_y {
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::Pos2::new(bar.geom.x - half_width, bar.geom.zero_y),
                    egui::Pos2::new(bar.geom.x + half_width, sell_y),
                ),
                0.0,
                bar.color,
            );
        }
    }
}

/// Map a unit alpha fraction to a byte opacity for premultiplied
/// `Color32` blending: `clamp(0,1) * 255`, half-away rounding, then a
/// second clamp into byte range before the float-to-int conversion (Rust
/// saturation, never wrapping: NaN reads 0, out-of-range reads the nearer
/// edge). The one spelling behind the ribbon fill, the ribbon polygons,
/// and the stroked oscillator emitters — emitters pass their `alpha`
/// straight through and never restate the arithmetic.
#[must_use]
pub fn alpha_u8(alpha: f32) -> u8 {
    (alpha.clamp(0.0, 1.0) * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Map a Pine-style transparency percent to a byte opacity.
///
/// `color.new(c, t)` semantics: `0` is fully opaque, `100` fully invisible.
/// Out-of-range inputs clamp; non-finite input paints opaque (fail-soft —
/// paint stays visible and debuggable rather than silently vanishing).
#[must_use]
pub fn opacity_from_transparency(transparency: f64) -> u8 {
    if !transparency.is_finite() {
        return 255;
    }
    let opacity = (100.0 - transparency) / 100.0 * 255.0;
    opacity.round().clamp(0.0, 255.0) as u8
}

/// Paint a zero-centered oscillator polyline under one style key, lifting
/// the pen across holes.
///
/// `alpha` is the line opacity (`1` opaque): series whose specification
/// carries transparency (for example dimmed oscillator legs) resolve it
/// caller-side through [`opacity_from_transparency`] and pass the fraction.
/// The stroke is the default hairline; use
/// [`paint_oscillator_styled`] when the series belongs to an indicator whose
/// Style tab sets a width or overrides the key's color.
pub fn paint_oscillator(
    painter: &egui::Painter,
    points: &[Option<(f32, f32)>],
    style_key: &str,
    theme: &ThemeConfig,
    alpha: f32,
) -> Result<(), EguiError> {
    paint_stroked_oscillator(painter, points, style_key, theme, &IndicatorStyle::default(), alpha)
}

/// Paint a zero-centered oscillator polyline under one indicator's style: the
/// style key's color (override first, theme second) at the style's stroke
/// width.
///
/// This is [`paint_oscillator`] plus the two things the Style tab edits, and
/// it resolves nothing itself: the color comes from
/// [`IndicatorStyle::color`] and the width from [`IndicatorStyle::stroke`], so
/// a styled line and an unstyled one differ only in those two values and
/// never in how they are drawn. Holes still lift the pen.
pub fn paint_oscillator_styled(
    painter: &egui::Painter,
    points: &[Option<(f32, f32)>],
    style_key: &str,
    theme: &ThemeConfig,
    style: &IndicatorStyle,
    alpha: f32,
) -> Result<(), EguiError> {
    paint_stroked_oscillator(painter, points, style_key, theme, style, alpha)
}

/// The one polyline emitter behind both the plain and the styled entry
/// points: resolve the color through the style, stroke it at the style's
/// width, and lift the pen across holes.
fn paint_stroked_oscillator(
    painter: &egui::Painter,
    points: &[Option<(f32, f32)>],
    style_key: &str,
    theme: &ThemeConfig,
    style: &IndicatorStyle,
    alpha: f32,
) -> Result<(), EguiError> {
    let base = style.color(theme, style_key)?;
    let opacity = alpha_u8(alpha);
    let color =
        egui::Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), opacity);
    let stroke = style.stroke(color);
    for window in points.windows(2) {
        if let (Some((x0, y0)), Some((x1, y1))) = (window[0], window[1]) {
            painter.line_segment([egui::Pos2::new(x0, y0), egui::Pos2::new(x1, y1)], stroke);
        }
    }
    Ok(())
}

/// Paint the `±threshold` rules across `[x0, x1]`.
///
/// Each signed leg reads through the same window predicate
/// [`threshold_line`] enforces, so an off-window rule draws nothing instead
/// of mapping off-pane: the legs are signed (`-threshold` is negative) and
/// [`threshold_line`] is positive-only, so the shared predicate — not a
/// second copy of it — is what both legs inherit.
pub fn paint_thresholds(
    painter: &egui::Painter,
    x0: f32,
    x1: f32,
    threshold: f64,
    viewport: &Viewport,
    theme: &ThemeConfig,
) -> Result<(), EguiError> {
    let color = resolve_color(theme, "grid")?;
    let stroke = egui::Stroke::new(1.0, color);
    for signed in [threshold, -threshold] {
        // `windowed_y` yields finite in-pane pixels only: an off-window leg
        // draws nothing, never an extrapolated line.
        if let Some(y) = viewport.windowed_y(signed) {
            painter.line_segment([egui::Pos2::new(x0, y), egui::Pos2::new(x1, y)], stroke);
        }
    }
    Ok(())
}

/// Pane-local crosshair spans for one readout: the snapped pixel plus the
/// full-pane extents the emitter draws through.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrosshairLines {
    /// Snapped x pixel (bar center).
    pub x: f32,
    /// Snapped y pixel (price).
    pub y: f32,
    /// Left edge of the horizontal line.
    pub left: f32,
    /// Right edge of the horizontal line.
    pub right: f32,
    /// Top edge of the vertical line.
    pub top: f32,
    /// Bottom edge of the vertical line.
    pub bottom: f32,
}

/// Build crosshair spans for a readout over a `width × height` pane.
///
/// The snapped pixel comes from the pure [`crosshair_pixel`], so the edge
/// draws exactly what the headless mapping resolves — `egui` translates
/// pointer events into readouts and paints these spans, never its own
/// coordinate math. `None` when the readout price is unmappable, sits
/// outside the window, or the extents are degenerate.
#[must_use]
pub fn crosshair_lines(
    crosshair: &Crosshair,
    viewport: &Viewport,
    width: f32,
    height: f32,
) -> Option<CrosshairLines> {
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let (x, y) = crosshair_pixel(crosshair, viewport)?;
    Some(CrosshairLines { x, y, left: 0.0, right: width, top: 0.0, bottom: height })
}

/// Paint the crosshair readout: full-pane horizontal + vertical lines
/// through the snapped pixel under the `"grid"` key.
pub fn paint_crosshair(
    painter: &egui::Painter,
    lines: &CrosshairLines,
    theme: &ThemeConfig,
) -> Result<(), EguiError> {
    let stroke = egui::Stroke::new(1.0, resolve_color(theme, "grid")?);
    painter.line_segment(
        [egui::Pos2::new(lines.left, lines.y), egui::Pos2::new(lines.right, lines.y)],
        stroke,
    );
    painter.line_segment(
        [egui::Pos2::new(lines.x, lines.top), egui::Pos2::new(lines.x, lines.bottom)],
        stroke,
    );
    Ok(())
}

/// One resolved annotation marker: bar position plus display role.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MarkerDot {
    /// Bar index of the annotated bar.
    pub index: usize,
    /// X pixel of the bar.
    pub x: f32,
    /// Y pixel of the annotated price.
    pub y: f32,
    /// Display role (theme key source).
    pub style: AnnotationStyle,
}

impl MarkerDot {
    /// One pivot crossing as an annotation marker: geometry carried over,
    /// side lowered to the shared display polarity (highs are bearish —
    /// a top is resistance — lows bullish), so pivots and annotations can
    /// never disagree about what "up" paints. This is the only pivot path
    /// to pixels; the retired pivot emitter read the same mapping.
    #[must_use]
    pub fn from_pivot(dot: &PivotDot) -> Self {
        Self {
            index: dot.index,
            x: dot.x,
            y: dot.y,
            style: match dot.side {
                PivotSide::High => AnnotationStyle::Bear,
                PivotSide::Low => AnnotationStyle::Bull,
            },
        }
    }
}

/// Resolve markers to pixels over the caller's window.
///
/// Markers ride the append-only set (the render-side journal): operator
/// clicks append through [`crate::annotations::annotate_at`], Pine drawings
/// arrive through the terminal bridge, and both paint here alike. One
/// [`crate::annotations::TimeIndex`] serves the whole batch, so the window
/// is walked once per frame no matter how many markers resolve; bars
/// outside the window, duplicate timestamps, or off-window/unmappable prices
/// are skipped — markers decorate published annotations only, never
/// extrapolated points off the pane.
#[must_use]
pub fn marker_dots(
    markers: &[Annotation],
    rows: &[ChartRow],
    viewport: &Viewport,
) -> Vec<MarkerDot> {
    let index = crate::annotations::TimeIndex::of(rows);
    markers
        .iter()
        .filter_map(|marker| {
            let bar = index.lookup(marker.key.bar_timestamp)?;
            let y = viewport.windowed_y(marker.key.price_float()?)?;
            Some(MarkerDot {
                index: bar,
                x: viewport.index_to_x(bar),
                y,
                style: marker.style,
            })
        })
        .collect()
}

/// Display polarity for one trend bit: bullish reads the bull key, bearish
/// the bear key. Pivot highs are bearish (a top is resistance) and lows
/// bullish (a bottom is support), while a named bull/bear annotation keeps
/// its literal key — both dot emitters read this one mapping, so the two
/// can never disagree about what "up" paints again.
fn polarity_color(bullish: bool, bull: egui::Color32, bear: egui::Color32) -> egui::Color32 {
    if bullish { bull } else { bear }
}

/// Paint annotation markers: filled dots in the style's theme color.
/// Pivots paint here too, lowered through [`MarkerDot::from_pivot`] — one
/// emitter, one shape, one radius, one polarity mapping.
pub fn paint_markers(
    painter: &egui::Painter,
    dots: &[MarkerDot],
    theme: &ThemeConfig,
) -> Result<(), EguiError> {
    let grid = resolve_color(theme, "grid")?;
    let bull = resolve_color(theme, "bull")?;
    let bear = resolve_color(theme, "bear")?;
    for dot in dots {
        let color = match dot.style {
            AnnotationStyle::Note => grid,
            AnnotationStyle::Bull => polarity_color(true, bull, bear),
            AnnotationStyle::Bear => polarity_color(false, bull, bear),
        };
        painter.circle_filled(egui::Pos2::new(dot.x, dot.y), 4.0, color);
    }
    Ok(())
}

/// Render a refused paint step as a visible error chip carrying the reason:
/// the failure surfaces in the frame instead of rendering a silent default.
pub fn paint_error_chip(ui: &mut egui::Ui, error: &EguiError) {
    ui.colored_label(egui::Color32::RED, format!("paint refused: {error}"));
}

/// Paint session-break gridlines over the pane's full height.
pub fn paint_session_breaks(
    painter: &egui::Painter,
    lines: &[BreakLine],
    top_y: f32,
    bottom_y: f32,
    theme: &ThemeConfig,
) -> Result<(), EguiError> {
    let stroke = egui::Stroke::new(1.0, resolve_color(theme, "grid")?);
    for line in lines {
        painter.line_segment(
            [egui::Pos2::new(line.x, top_y), egui::Pos2::new(line.x, bottom_y)],
            stroke,
        );
    }
    Ok(())
}

/// Shift pane-local candle geometry into UI space by a pane origin.
///
/// Geometry here is pane-local (origin at the pane's top-left); every emitter
/// draws exactly the coordinates it is given, so a frame that lays panes side
/// by side shifts each layer by its own origin first. `origin.0` moves x,
/// `origin.1` moves every y leg.
#[must_use]
pub fn translate_candles(geoms: Vec<CandleGeom>, origin: (f32, f32)) -> Vec<CandleGeom> {
    geoms
        .into_iter()
        .map(|mut geom| {
            geom.x += origin.0;
            geom.open_y += origin.1;
            geom.high_y += origin.1;
            geom.low_y += origin.1;
            geom.close_y += origin.1;
            geom
        })
        .collect()
}

/// Shift pane-local fill runs into UI space (see [`translate_candles`]).
#[must_use]
pub fn translate_polygons(polygons: Vec<RibbonPolygon>, origin: (f32, f32)) -> Vec<RibbonPolygon> {
    polygons
        .into_iter()
        .map(|mut polygon| {
            for point in &mut polygon.points {
                point.0 += origin.0;
                point.1 += origin.1;
            }
            polygon
        })
        .collect()
}

/// Shift pane-local session gridlines into UI space: a gridline has one x
/// coordinate and spans the pane height at paint time, so only `dx` moves it.
#[must_use]
pub fn translate_breaks(lines: Vec<BreakLine>, dx: f32) -> Vec<BreakLine> {
    lines
        .into_iter()
        .map(|mut line| {
            line.x += dx;
            line
        })
        .collect()
}

/// Shift pane-local series points into UI space (see [`translate_candles`]).
/// Holes stay holes.
#[must_use]
pub fn translate_points(
    points: Vec<Option<(f32, f32)>>,
    origin: (f32, f32),
) -> Vec<Option<(f32, f32)>> {
    points
        .into_iter()
        .map(|point| point.map(|(x, y)| (x + origin.0, y + origin.1)))
        .collect()
}

/// Shift pane-local pivot dots into UI space (see [`translate_candles`]).
#[must_use]
pub fn translate_dots(dots: Vec<PivotDot>, origin: (f32, f32)) -> Vec<PivotDot> {
    dots
        .into_iter()
        .map(|mut dot| {
            dot.x += origin.0;
            dot.y += origin.1;
            dot
        })
        .collect()
}

/// Shift pane-local markers into UI space (see [`translate_candles`]).
#[must_use]
pub fn translate_markers(dots: Vec<MarkerDot>, origin: (f32, f32)) -> Vec<MarkerDot> {
    dots
        .into_iter()
        .map(|mut dot| {
            dot.x += origin.0;
            dot.y += origin.1;
            dot
        })
        .collect()
}

/// Shift pane-local delta bars into UI space (see [`translate_candles`]).
/// Both legs and the zero line share the vertical shift; holed legs stay
/// holed.
#[must_use]
pub fn translate_delta_bars(bars: Vec<DeltaBarGeom>, origin: (f32, f32)) -> Vec<DeltaBarGeom> {
    bars
        .into_iter()
        .map(|mut bar| {
            bar.x += origin.0;
            bar.buy_y = bar.buy_y.map(|y| y + origin.1);
            bar.sell_y = bar.sell_y.map(|y| y + origin.1);
            bar.zero_y += origin.1;
            bar
        })
        .collect()
}

/// Samples per eye-lid arc. Fixed so identical inputs build identical
/// geometry on every frame.
const LID_ARC_SAMPLES: usize = 9;

/// One eye control icon: lid arcs above and below center, a pupil on
/// center, and a slash exactly when the slot it toggles is hidden.
#[derive(Debug, Clone, PartialEq)]
pub struct EyeIconGeom {
    /// Icon center in UI space.
    pub center: (f32, f32),
    /// Half-width of the lids; the lids' vertical semi-axis is `0.6` of this.
    pub radius: f32,
    /// Upper lid polyline, left corner to right corner, never below center.
    pub upper_arc: Vec<(f32, f32)>,
    /// Lower lid polyline, left corner to right corner, never above center.
    pub lower_arc: Vec<(f32, f32)>,
    /// Pupil center (always the icon center).
    pub pupil_center: (f32, f32),
    /// Pupil radius (`0.35` of the lid radius).
    pub pupil_radius: f32,
    /// Slash across the lids; present exactly when `visible` is `false`.
    pub slash: Option<((f32, f32), (f32, f32))>,
}

/// Build eye geometry around `center` at `radius`.
///
/// The lids are ellipse arcs (`0.6` vertical squash) pinned exactly to the
/// horizontal through center at both corners, so the upper lid never dips
/// below center and the lower lid never rises above it. Refuses non-finite
/// centers, non-finite radii, and radii at or below zero.
#[must_use]
pub fn eye_icon_geom(center: (f32, f32), radius: f32, visible: bool) -> Option<EyeIconGeom> {
    if !center.0.is_finite() || !center.1.is_finite() || !radius.is_finite() || radius <= 0.0 {
        return None;
    }
    let lid = |start: f64, end: f64| {
        let mut arc: Vec<(f32, f32)> = (0..LID_ARC_SAMPLES)
            .map(|step| {
                let angle =
                    start + (end - start) * step as f64 / (LID_ARC_SAMPLES - 1) as f64;
                (
                    center.0 + radius * angle.cos() as f32,
                    center.1 + 0.6 * radius * angle.sin() as f32,
                )
            })
            .collect();
        // Pin both corners exactly onto the horizontal: the trigonometric
        // endpoints land within a rounding quantum of center, never on it.
        let last = arc.len() - 1;
        arc[0] = (center.0 - radius, center.1);
        arc[last] = (center.0 + radius, center.1);
        arc
    };
    let slant = 0.7 * radius;
    Some(EyeIconGeom {
        center,
        radius,
        upper_arc: lid(std::f64::consts::PI, 2.0 * std::f64::consts::PI),
        lower_arc: lid(std::f64::consts::PI, 0.0),
        pupil_center: center,
        pupil_radius: 0.35 * radius,
        slash: (!visible).then_some((
            (center.0 - slant, center.1 + slant),
            (center.0 + slant, center.1 - slant),
        )),
    })
}

/// One six-sided bolt control icon: six rim vertices plus an inner bore.
#[derive(Debug, Clone, PartialEq)]
pub struct HexBoltIconGeom {
    /// Icon center in UI space.
    pub center: (f32, f32),
    /// Rim distance each vertex sits at.
    pub radius: f32,
    /// Six hexagon vertices at `radius` from center, every 60 degrees.
    pub vertices: [(f32, f32); 6],
    /// Bore center (always the icon center).
    pub bore_center: (f32, f32),
    /// Bore radius (`0.45` of the rim radius).
    pub bore_radius: f32,
}

/// Build six-sided bolt geometry around `center` at `radius`.
///
/// Refuses non-finite centers, non-finite radii, and radii at or below zero.
#[must_use]
pub fn hex_bolt_icon_geom(center: (f32, f32), radius: f32) -> Option<HexBoltIconGeom> {
    if !center.0.is_finite() || !center.1.is_finite() || !radius.is_finite() || radius <= 0.0 {
        return None;
    }
    let mut vertices = [(0.0f32, 0.0f32); 6];
    for (step, vertex) in vertices.iter_mut().enumerate() {
        let angle = step as f64 * std::f64::consts::PI / 3.0;
        *vertex = (
            center.0 + radius * angle.cos() as f32,
            center.1 + radius * angle.sin() as f32,
        );
    }
    Some(HexBoltIconGeom {
        center,
        radius,
        vertices,
        bore_center: center,
        bore_radius: 0.45 * radius,
    })
}

/// One search control icon: a lens circle plus a handle leaving its rim.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchIconGeom {
    /// Icon center in UI space (also the lens center).
    pub center: (f32, f32),
    /// Outer icon bound; the lens and handle stay inside it.
    pub radius: f32,
    /// Lens center (always the icon center).
    pub lens_center: (f32, f32),
    /// Lens radius (`0.55` of the icon radius).
    pub lens_radius: f32,
    /// Handle segment: heel on the lens rim, tip toward the icon bound.
    pub handle: ((f32, f32), (f32, f32)),
}

/// Build search geometry around `center` at `radius`.
///
/// The handle leaves the lens rim at 45 degrees down-right. Refuses
/// non-finite centers, non-finite radii, and radii at or below zero.
#[must_use]
pub fn search_icon_geom(center: (f32, f32), radius: f32) -> Option<SearchIconGeom> {
    if !center.0.is_finite() || !center.1.is_finite() || !radius.is_finite() || radius <= 0.0 {
        return None;
    }
    let lens_radius = 0.55 * radius;
    let unit = std::f64::consts::FRAC_1_SQRT_2 as f32;
    let heel = (center.0 + unit * lens_radius, center.1 + unit * lens_radius);
    let reach = lens_radius + 0.6 * radius;
    Some(SearchIconGeom {
        center,
        radius,
        lens_center: center,
        lens_radius,
        handle: (heel, (center.0 + unit * reach, center.1 + unit * reach)),
    })
}

/// Stroke one polyline through `points`; an empty run draws nothing.
fn stroke_polyline(painter: &egui::Painter, points: &[(f32, f32)], stroke: egui::Stroke) {
    for pair in points.windows(2) {
        painter.line_segment(
            [egui::Pos2::new(pair[0].0, pair[0].1), egui::Pos2::new(pair[1].0, pair[1].1)],
            stroke,
        );
    }
}

/// Paint an eye icon through one theme key: lid outlines, a filled pupil,
/// and the slash when the geometry carries one.
///
/// A theme that cannot resolve `style_key` refuses with the key named —
/// icons never paint a silent default color.
pub fn paint_eye_icon(
    painter: &egui::Painter,
    geom: &EyeIconGeom,
    style_key: &str,
    theme: &ThemeConfig,
) -> Result<(), EguiError> {
    let color = resolve_color(theme, style_key)?;
    let stroke = egui::Stroke::new(1.5, color);
    stroke_polyline(painter, &geom.upper_arc, stroke);
    stroke_polyline(painter, &geom.lower_arc, stroke);
    painter.circle_filled(
        egui::Pos2::new(geom.pupil_center.0, geom.pupil_center.1),
        geom.pupil_radius,
        color,
    );
    if let Some((from, to)) = geom.slash {
        painter.line_segment(
            [egui::Pos2::new(from.0, from.1), egui::Pos2::new(to.0, to.1)],
            stroke,
        );
    }
    Ok(())
}

/// Paint a six-sided bolt icon through one theme key: a hex outline plus a
/// bore ring. Refuses exactly like [`paint_eye_icon`].
pub fn paint_hex_bolt_icon(
    painter: &egui::Painter,
    geom: &HexBoltIconGeom,
    style_key: &str,
    theme: &ThemeConfig,
) -> Result<(), EguiError> {
    let color = resolve_color(theme, style_key)?;
    let stroke = egui::Stroke::new(1.5, color);
    let mut rim: Vec<(f32, f32)> = geom.vertices.to_vec();
    if let Some(first) = geom.vertices.first() {
        rim.push(*first);
    }
    stroke_polyline(painter, &rim, stroke);
    painter.circle_stroke(
        egui::Pos2::new(geom.bore_center.0, geom.bore_center.1),
        geom.bore_radius,
        stroke,
    );
    Ok(())
}

/// Paint a search icon through one theme key: a lens ring plus its handle.
/// Refuses exactly like [`paint_eye_icon`].
pub fn paint_search_icon(
    painter: &egui::Painter,
    geom: &SearchIconGeom,
    style_key: &str,
    theme: &ThemeConfig,
) -> Result<(), EguiError> {
    let color = resolve_color(theme, style_key)?;
    let stroke = egui::Stroke::new(1.5, color);
    painter.circle_stroke(
        egui::Pos2::new(geom.lens_center.0, geom.lens_center.1),
        geom.lens_radius,
        stroke,
    );
    painter.line_segment(
        [
            egui::Pos2::new(geom.handle.0.0, geom.handle.0.1),
            egui::Pos2::new(geom.handle.1.0, geom.handle.1.1),
        ],
        stroke,
    );
    Ok(())
}

#[cfg(test)]
#[path = "../tests/paint.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/alpha_opacity.rs"]
mod alpha_opacity_tests;
