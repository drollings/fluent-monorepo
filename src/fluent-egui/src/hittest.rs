//! Pure hit-test and crosshair mapping over the canonical scale, still headless.
//!
//! Two layers, one coordinate truth. [`crosshair_data`]/[`crosshair_pixel`]
//! translate between pointer pixels and `(bar index, price)` with no hit
//! radius — the readout answers anywhere in-pane. [`hit_test`] adds the
//! candle check: the pointer must sit within [`HIT_EPSILON_PX`] of the bar's
//! high-low wick segment (borrowed `point_to_segment_distance` math, not the
//! reference's `egui` coupling), so empty pane never grabs. Every mapping
//! delegates to the caller's [`Viewport`]: there is exactly one price↔pixel
//! truth, and indices are relative to the passed slice (callers pass the
//! visible window, so bar zero is the window start). No paint types enter
//! here; the windowing shell translates pointer events into pixel tuples.

use protopyne_chart_model::ChartRow;

use crate::viewport::Viewport;

/// Hit radius in pixels: how close is close enough to count the operator as
/// pointing at the candle.
///
/// This is doubt about the operator's intent, never about which bar is
/// right. The candidate bar comes from the pointer's slot alone; the radius
/// only decides whether that candidate counts as a hit. Once a hit returns,
/// the bar's data fixes every value read from it and the radius has no
/// further say. The radius must never select the bar and must never gate
/// whether a bar is readable — the crosshair readout answers without it.
pub const HIT_EPSILON_PX: f32 = 8.0;

/// One pointer hit: the bar under the pointer plus the price under it.
///
/// `x`/`y` are exactly what [`Viewport`] maps the stored `(index, price)`
/// back to, so `hit -> data -> pixel` round-trips bit-for-bit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hit {
    /// Bar index, relative to the passed slice.
    pub index: usize,
    /// Price under the pointer.
    pub price: f64,
    /// X pixel of the bar center (`index_to_x(index)`).
    pub x: f32,
    /// Y pixel of the price (`price_to_y(price)`).
    pub y: f32,
}

/// Crosshair readout: bar index plus price, with no hit radius.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Crosshair {
    /// Bar index, relative to the passed window length.
    pub index: usize,
    /// Price under the pointer.
    pub price: f64,
}

/// Distance from a point to a line segment, in pixels.
///
/// The projection clamps to the segment, so past-end pointers read the nearer
/// tip; a zero-length segment reads the point distance. Non-finite inputs
/// read infinite distance (fail-closed: never claim a hit).
#[must_use]
pub fn point_to_segment_distance(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> f32 {
    if !px.is_finite()
        || !py.is_finite()
        || !x0.is_finite()
        || !y0.is_finite()
        || !x1.is_finite()
        || !y1.is_finite()
    {
        return f32::INFINITY;
    }
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len_sq = dx * dx + dy * dy;
    if len_sq <= 0.0 {
        return (px - x0).hypot(py - y0);
    }
    let along = ((px - x0) * dx + (py - y0) * dy) / len_sq;
    let clamped = along.clamp(0.0, 1.0);
    let nearest_x = x0 + clamped * dx;
    let nearest_y = y0 + clamped * dy;
    (px - nearest_x).hypot(py - nearest_y)
}

/// Read the crosshair under a pointer: nearest bar plus price.
///
/// Slots below zero or past `row_count` name no bar (ties round up: a pointer
/// exactly between two bars reads the higher index, deterministically);
/// off-pane pixels name no price. The readout takes no epsilon — it answers
/// anywhere a bar and a price both resolve.
#[must_use]
pub fn crosshair_data(
    pointer: (f32, f32),
    viewport: &Viewport,
    row_count: usize,
) -> Option<Crosshair> {
    let price = viewport.y_to_price(pointer.1)?;
    let slot = viewport.x_to_index(pointer.0)?;
    if !slot.is_finite() || slot < 0.0 {
        return None;
    }
    let index = (slot + 0.5).floor() as usize;
    if index >= row_count {
        return None;
    }
    Some(Crosshair { index, price })
}

/// Map a crosshair back to pixels; `None` when the price is unmappable or
/// sits outside the visible window. The readout pixel is always in-pane, so
/// the edge draws exactly what the mapping resolves — never an
/// extrapolated point off the pane.
#[must_use]
pub fn crosshair_pixel(crosshair: &Crosshair, viewport: &Viewport) -> Option<(f32, f32)> {
    let y = viewport.windowed_y(crosshair.price)?;
    Some((viewport.index_to_x(crosshair.index), y))
}

/// Hit the candle under a pointer, or `None` for a miss.
///
/// The candidate bar comes from [`crosshair_data`]; the hit additionally
/// requires the pointer within [`HIT_EPSILON_PX`] of the bar's high-low wick
/// segment. A bar whose high/low leg is unmappable falls back to its close
/// point; a bar with no mappable price at all is a miss. Empty input is a
/// miss.
#[must_use]
pub fn hit_test(pointer: (f32, f32), viewport: &Viewport, rows: &[ChartRow]) -> Option<Hit> {
    if rows.is_empty() {
        return None;
    }
    let data = crosshair_data(pointer, viewport, rows.len())?;
    let row = rows.get(data.index)?;
    let x = viewport.index_to_x(data.index);
    let (top_y, bottom_y) = if let (Some(high_y), Some(low_y)) =
        (viewport.price_to_y(row.high), viewport.price_to_y(row.low))
    {
        (high_y, low_y)
    } else {
        let close_y = viewport.price_to_y(row.close)?;
        (close_y, close_y)
    };
    let distance = point_to_segment_distance(pointer.0, pointer.1, x, top_y, x, bottom_y);
    if !distance.is_finite() || distance > HIT_EPSILON_PX {
        return None;
    }
    let y = viewport.price_to_y(data.price)?;
    Some(Hit {
        index: data.index,
        price: data.price,
        x,
        y,
    })
}

#[cfg(test)]
#[path = "../tests/hittest.rs"]
mod tests;
