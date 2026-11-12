//! Pine overlay: engine primitives become pane-local pixels, then paint.
//!
//! [`overlay_geoms`] maps [`PlotPrimitive`] values through the shared
//! [`Viewport`](fluent_egui::Viewport) — series reuse
//! [`oscillator_points`](fluent_egui::oscillator_points), so no second
//! bar-to-pixel math lives here — and [`paint_overlay`] blits the result
//! with thin emitters. Pine colors stay unmapped (opaque handles with no
//! theme-key table), so paint assigns one style key per primitive kind;
//! those keys are display-only and never flow back into a decision.
//!
//! A refused projection renders as an "unsupported" chip through
//! [`paint_unsupported_chip`], never as a partial overlay.

use fluent_egui::{
    oscillator_points, paint_oscillator, resolve_color, translate_points, EguiError, ThemeConfig,
    Viewport,
};
use protopyne_kernels::{PlotPrimitive, ProjectionRefusal};

/// Style keys per primitive kind (display-only; Pine colors unmapped).
const SERIES_KEY: &str = "fast";
const LEVEL_KEY: &str = "grid";
const MARKER_KEY: &str = "bull";
const LABEL_KEY: &str = "slow";
const AREA_KEY: &str = "slow";

/// One projected series as pane-local points, holes preserved.
#[derive(Debug, Clone, PartialEq)]
/// One projected series as pane-local points, holes preserved.
pub struct SeriesGeom {
    /// One point per cursor-space bar (`None` holes stay holes — including
    /// the warmup holes the projection pads, so the series aligns
    /// slot-for-slot with the bar window the run covered).
    pub points: Vec<Option<(f32, f32)>>,
}

/// One placed label: pane-local anchor plus its text.
#[derive(Debug, Clone, PartialEq)]
/// One placed label: pane-local anchor plus its text.
pub struct PlacedLabel {
    /// X pixel of the anchor bar.
    pub x: f32,
    /// Y pixel of the anchor price.
    pub y: f32,
    /// Label text.
    pub text: String,
}

/// Pane-local overlay geometry: every pixel the overlay paints.
#[derive(Debug, Clone, PartialEq, Default)]
/// Pane-local overlay geometry: every pixel the overlay paints.
pub struct OverlayGeoms {
    /// Projected series, in primitive order.
    pub series: Vec<SeriesGeom>,
    /// Level y pixels, in primitive order.
    pub levels: Vec<f32>,
    /// Marker pixels, in primitive order.
    pub markers: Vec<(f32, f32)>,
    /// Placed labels, in primitive order.
    pub labels: Vec<PlacedLabel>,
    /// Area corners `(x0, y0, x1, y1)`, in primitive order.
    pub areas: Vec<(f32, f32, f32, f32)>,
}

/// Map primitives onto pane-local pixels. Holes and unmappable prices stay
/// unpainted — the overlay never invents a point.
#[must_use]
/// Map primitives onto pane-local pixels. Holes and unmappable prices stay
/// unpainted — the overlay never invents a point.
pub fn overlay_geoms(primitives: &[PlotPrimitive], viewport: &Viewport) -> OverlayGeoms {
    let mut geoms = OverlayGeoms::default();
    for primitive in primitives {
        match primitive {
            PlotPrimitive::Series { values, .. } => geoms.series.push(SeriesGeom {
                points: oscillator_points(values, viewport),
            }),
            PlotPrimitive::Level { price, .. } => {
                geoms.levels.extend(viewport.price_to_y(*price));
            }
            PlotPrimitive::Marker { bar, price, .. } => {
                if let Some(y) = viewport.price_to_y(*price) {
                    geoms.markers.push((viewport.index_to_x(*bar), y));
                }
            }
            PlotPrimitive::Label { bar, price, text } => {
                if let Some(y) = viewport.price_to_y(*price) {
                    geoms.labels.push(PlacedLabel {
                        x: viewport.index_to_x(*bar),
                        y,
                        text: text.clone(),
                    });
                }
            }
            PlotPrimitive::Area {
                left,
                top,
                right,
                bottom,
            } => {
                if let (Some(y0), Some(y1)) =
                    (viewport.price_to_y(*top), viewport.price_to_y(*bottom))
                {
                    geoms.areas.push((
                        viewport.index_to_x(*left),
                        y0,
                        viewport.index_to_x(*right),
                        y1,
                    ));
                }
            }
        }
    }
    geoms
}

/// Blit precomputed overlay geometry. `x1` is the pane-local right edge the
/// levels rule across; every other coordinate arrives pane-local, so the
/// caller shifts by the pane origin first (like every other paint layer).
/// Each theme key resolves inside the branch that needs it, in series /
/// level / marker / label / area order: a theme missing an unused key
/// paints the branches it can, and the first missing key that is actually
/// needed names itself in the refusal. A broken theme refuses here instead
/// of painting silent gray.
pub fn paint_overlay(
    painter: &egui::Painter,
    geoms: &OverlayGeoms,
    theme: &ThemeConfig,
    x1: f32,
) -> Result<(), EguiError> {
    for run in &geoms.series {
        paint_oscillator(painter, &run.points, SERIES_KEY, theme, 1.0)?;
    }
    if !geoms.levels.is_empty() {
        let level_stroke = egui::Stroke::new(1.0, resolve_color(theme, LEVEL_KEY)?);
        for y in &geoms.levels {
            painter.line_segment(
                [egui::Pos2::new(0.0, *y), egui::Pos2::new(x1, *y)],
                level_stroke,
            );
        }
    }
    if !geoms.markers.is_empty() {
        let marker_color = resolve_color(theme, MARKER_KEY)?;
        for (x, y) in &geoms.markers {
            painter.circle_filled(egui::Pos2::new(*x, *y), 3.0, marker_color);
        }
    }
    if !geoms.labels.is_empty() {
        let label_color = resolve_color(theme, LABEL_KEY)?;
        let font = egui::FontId::proportional(11.0);
        for label in &geoms.labels {
            painter.text(
                egui::Pos2::new(label.x, label.y),
                egui::Align2::CENTER_BOTTOM,
                &label.text,
                font.clone(),
                label_color,
            );
        }
    }
    if !geoms.areas.is_empty() {
        let area_stroke = egui::Stroke::new(1.0, resolve_color(theme, AREA_KEY)?);
        for (x0, y0, x1, y1) in &geoms.areas {
            painter.rect_stroke(
                egui::Rect::from_min_max(egui::Pos2::new(*x0, *y0), egui::Pos2::new(*x1, *y1)),
                0.0,
                area_stroke,
                egui::StrokeKind::Middle,
            );
        }
    }
    Ok(())
}

/// Render a refused projection as an "unsupported" chip carrying the typed
/// reason — the overlay paints nothing partial.
pub fn paint_unsupported_chip(ui: &mut egui::Ui, refusal: &ProjectionRefusal) {
    ui.colored_label(
        egui::Color32::RED,
        format!("pine overlay unsupported: {refusal}"),
    );
}

/// Shift pane-local overlay geometry into UI space by a pane origin.
///
/// Every primitive kind is pane-local ([`overlay_geoms`] maps against the
/// pane's own viewport); the overlay emitter draws exactly these pixels, so
/// the caller shifts the whole batch by its pane rect's origin first. Series
/// points ride the shared [`translate_points`], so the overlay never keeps a
/// second coordinate translation.
#[must_use]
/// Shift pane-local overlay geometry into UI space by a pane origin.
/// 
/// Every primitive kind is pane-local ([`overlay_geoms`] maps against the
/// pane's own viewport); the overlay emitter draws exactly these pixels, so
/// the caller shifts the whole batch by its pane rect's origin first. Series
/// points ride the shared [`translate_points`], so the overlay never keeps a
/// second coordinate translation.
pub fn translate_overlay(mut geoms: OverlayGeoms, origin: (f32, f32)) -> OverlayGeoms {
    for run in &mut geoms.series {
        run.points = translate_points(std::mem::take(&mut run.points), origin);
    }
    for y in &mut geoms.levels {
        *y += origin.1;
    }
    for (x, y) in &mut geoms.markers {
        *x += origin.0;
        *y += origin.1;
    }
    for label in &mut geoms.labels {
        label.x += origin.0;
        label.y += origin.1;
    }
    for (x0, y0, x1, y1) in &mut geoms.areas {
        *x0 += origin.0;
        *y0 += origin.1;
        *x1 += origin.0;
        *y1 += origin.1;
    }
    geoms
}

#[cfg(test)]
#[path = "../tests/pine_overlay.rs"]
mod tests;