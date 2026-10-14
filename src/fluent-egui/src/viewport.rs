//! Viewport geometry on the canonical scale, still headless.
//!
//! Pixels-in/pixels-out: a [`Viewport`] owns one [`PriceScale`] built only
//! through `PriceScale :: new`, so every degenerate geometry refuses at
//! construction instead of emitting substitute coordinates. Windowing slices
//! converted rows without copying; tick thinning caps label counts for
//! readability only — a thinned axis is never evidence of sparse data.
//! Domain overlays (session breaks, toolbar range windows) compose this
//! viewport in the terminal crate. No paint types enter here.

use protopyne_chart_model::ChartRow;
use protopyne_chart_scale::{PriceRange, PriceScale, PriceScaleMode, Tick, nice_ticks};

use crate::error::EguiError;

/// Visible pane geometry: one price window mapped onto fixed pixels.
///
/// Construct once per visible window; recompute only on scroll, zoom, or
/// range change — never per frame per bar. All mapping delegates to the
/// owned canonical scale, so there is exactly one price↔pixel truth.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    scale: PriceScale,
}

impl Viewport {
    /// Build a viewport over `[min, max]`; refuses zero/negative geometry,
    /// non-finite inputs, inverted ranges, non-positive log bounds, and
    /// zero-base percentage modes — the same refusals as the canonical
    /// scale, surfaced as paint-edge errors.
    pub fn new(
        mode: PriceScaleMode,
        min: f64,
        max: f64,
        height: f32,
        bar_spacing: f32,
        base_value: f64,
        inverted: bool,
    ) -> Result<Self, EguiError> {
        let range = PriceRange::new(min, max).map_err(describe_scale)?;
        let scale =
            PriceScale::new(mode, range, height, bar_spacing, base_value, inverted)
                .map_err(describe_scale)?;
        Ok(Self { scale })
    }

    /// The owned canonical scale.
    #[must_use]
    pub fn scale(self) -> PriceScale {
        self.scale
    }

    /// The price window on screen right now: the single answer to "what is
    /// on screen", read from the owned scale, never re-derived at a call
    /// site. Grid ticks, axis labels, and area baselines all read this; the
    /// data extent behind the window is a separate question (what the
    /// viewport was seeded from) and must never stand in for it. Ticks
    /// drawn from this window are part of the displayed price truth — their
    /// count and spacing are an output, not a confidence measure.
    #[must_use]
    pub fn visible_range(self) -> PriceRange {
        self.scale.range()
    }

    /// Map a price to a y pixel only when it sits inside the visible window;
    /// `None` for unmappable or off-window prices. The plain [`price_to_y`]
    /// extrapolates by design (the paint edge relies on it to clip lines and
    /// candles running off the pane), so emitters that must not draw
    /// off-pane read through here instead of re-deciding containment at
    /// every call site: one predicate, inherited everywhere.
    #[must_use]
    pub fn windowed_y(self, price: f64) -> Option<f32> {
        if !self.scale.range().contains(price) {
            return None;
        }
        self.price_to_y(price)
    }

    /// Map a price to a y pixel (`0` at the top); `None` on non-finite or
    /// unmappable input.
    #[must_use]
    pub fn price_to_y(self, price: f64) -> Option<f32> {
        self.scale.price_to_y(price)
    }

    /// Invert [`price_to_y`](Self::price_to_y); `None` off-pane or degenerate.
    #[must_use]
    pub fn y_to_price(self, y: f32) -> Option<f64> {
        self.scale.y_to_price(y)
    }

    /// Map a bar index to an x pixel.
    #[must_use]
    pub fn index_to_x(self, index: usize) -> f32 {
        self.scale.index_to_x(index)
    }

    /// Invert [`index_to_x`](Self::index_to_x); `None` on non-finite input.
    #[must_use]
    pub fn x_to_index(self, x: f32) -> Option<f64> {
        self.scale.x_to_index(x)
    }
}

/// Carry a scale refusal across the crate boundary without a second error type.
fn describe_scale(error: protopyne_chart_scale::ScaleError) -> EguiError {
    match error {
        protopyne_chart_scale::ScaleError::InvalidParam { what, reason } => {
            EguiError::InvalidParam { what, reason }
        }
    }
}

/// One session-break gridline at a break bar's x pixel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BreakLine {
    /// Index of the first bar in the new session.
    pub index: usize,
    /// X pixel of that bar.
    pub x: f32,
}

/// Window converted rows without copying; saturates out-of-range bounds
/// instead of panicking, so scroll positions past the data edge show an
/// empty (or trailing) window rather than aborting the frame.
#[must_use]
pub fn visible_rows(rows: &[ChartRow], start: usize, count: usize) -> &[ChartRow] {
    let start = start.min(rows.len());
    let end = start.saturating_add(count).min(rows.len());
    &rows[start..end]
}

/// Cap grid labels for readability; order-preserving stride thinning.
///
/// Returns every tick when the grid already fits `max_labels`, an empty
/// vector when `max_labels` is zero, and otherwise every Nth tick with
/// `N = ceil(len / max_labels)`. Counts only — the surviving values and
/// labels are byte-identical to the input grid.
#[must_use]
pub fn thin_ticks(ticks: Vec<Tick>, max_labels: usize) -> Vec<Tick> {
    if max_labels == 0 {
        return Vec::new();
    }
    if ticks.len() <= max_labels {
        return ticks;
    }
    let stride = ticks.len().div_ceil(max_labels).max(1);
    ticks.into_iter().step_by(stride).collect()
}

/// Round grid for `[min, max]` within `budget`, thinned to `max_labels`.
///
/// One call from window to paint-ready labels: the canonical grid generator
/// followed by the readability cap. Degenerate windows still yield no ticks
/// rather than a misleading grid.
#[must_use]
pub fn axis_ticks(min: f64, max: f64, budget: usize, max_labels: usize) -> Vec<Tick> {
    thin_ticks(nice_ticks(min, max, budget), max_labels)
}

#[cfg(test)]
#[path = "../tests/viewport.rs"]
mod tests;
