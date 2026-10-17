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
/// owned canonical scale, so there is exactly one price↔pixel truth. The
/// horizontal `x_offset_px` is an additive pixel shift on top of the scale's
/// bar spacing (default `0.0`, so every existing caller is unchanged): it
/// carries time-axis pan and cursor-anchored zoom without touching the
/// integer bar window the data slice is taken from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    scale: PriceScale,
    x_offset_px: f32,
}

/// Bars of overscan kept each side of the visible window when culling
/// candle geometry. A candle whose center just left the pane still paints
/// half its body and its wick inside it, so a hard clamp makes edge candles
/// flicker in and out while panning. The margin suppresses that edge pop —
/// a task-value choice pinned by the pan-sweep golden, not a confidence
/// signal — and stays constant-bounded so zoomed-in views cost O(visible).
pub const CULL_OVERSCAN_BARS: usize = 4;

/// Visible widths covered by the proportional cull: at most twice the visible
/// bar count, always covering the visible range. The factor bounds per-frame
/// work while keeping edge-crossing segments exact — a task-value choice, not
/// a confidence signal — so native overlays stay O(visible) instead of
/// O(history) at native zoom.
pub const PROPORTIONAL_CULL_FACTOR: usize = 2;

impl Viewport {
    /// Build a viewport over `[min, max]`; refuses zero/negative geometry,
    /// non-finite inputs, inverted ranges, non-positive log bounds, and
    /// zero-base percentage modes — the same refusals as the canonical
    /// scale, surfaced as paint-edge errors. The horizontal offset starts at
    /// `0.0`.
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
        Ok(Self { scale, x_offset_px: 0.0 })
    }

    /// The owned canonical scale.
    #[must_use]
    pub fn scale(self) -> PriceScale {
        self.scale
    }

    /// The horizontal pixel offset added to every bar slot; `0.0` unless the
    /// time axis has been panned or zoomed.
    #[must_use]
    pub fn x_offset_px(self) -> f32 {
        self.x_offset_px
    }

    /// Set the horizontal pixel offset; refuses non-finite offsets with the
    /// parameter named.
    pub fn with_x_offset(self, x_offset_px: f32) -> Result<Self, EguiError> {
        if !x_offset_px.is_finite() {
            return Err(EguiError::InvalidParam {
                what: "x_offset_px".to_owned(),
                reason: "horizontal offset must be finite".to_owned(),
            });
        }
        Ok(Self { x_offset_px, ..self })
    }

    /// Adopt another viewport's time axis: copy its `bar_spacing` and
    /// `x_offset_px`, keeping this viewport's own price range, mode, height,
    /// base value, and inversion. A sub-pane calls this each frame to lock
    /// its time axis to the main chart's while keeping its own price scale.
    pub fn sync_time_axis(self, source: &Viewport) -> Result<Self, EguiError> {
        let scale = PriceScale::new(
            self.scale.mode(),
            self.scale.range(),
            self.scale.height(),
            source.scale.bar_spacing(),
            self.scale.base_value(),
            self.scale.inverted(),
        )
        .map_err(describe_scale)?;
        Ok(Self { scale, x_offset_px: source.x_offset_px })
    }

    /// The `[start, end)` bar indices whose centers fall inside the pane
    /// width `[0.0, width_px]`, clamped to `[0, total_bars]`. Under a
    /// positive or negative `x_offset_px`, the returned slice is exactly the
    /// bars visible in the pane; a fully off-window axis returns an empty
    /// `(n, n)` range.
    #[must_use]
    pub fn visible_bar_range(self, width_px: f32, total_bars: usize) -> (usize, usize) {
        if !width_px.is_finite() || width_px < 0.0 || total_bars == 0 {
            return (0, 0);
        }
        let spacing = f64::from(self.scale.bar_spacing());
        if !spacing.is_finite() || spacing <= 0.0 {
            return (0, 0);
        }
        let offset = f64::from(self.x_offset_px);
        let total = total_bars as f64;
        let first = ((-offset) / spacing).ceil();
        let last = ((f64::from(width_px) - offset) / spacing).floor();
        let start = first.clamp(0.0, total) as usize;
        let end = (last + 1.0).clamp(0.0, total) as usize;
        (start.min(end), end)
    }

    /// The `[start, end)` bar indices actually painted: the visible range
    /// widened by [`CULL_OVERSCAN_BARS`] each side, clamped to
    /// `[0, total_bars]`. Composes [`visible_bar_range`](Self::visible_bar_range),
    /// never a second copy of its math. An empty visible range stays empty —
    /// off-window viewports paint nothing, never a margin of stale bars — and
    /// a pane with no area (`width_px <= 0.0`) paints nothing either: the one
    /// edge bar straddling x `0.0` would clip to nothing downstream.
    #[must_use]
    pub fn culled_bar_range(self, width_px: f32, total_bars: usize) -> (usize, usize) {        if width_px <= 0.0 {
            return (0, 0);
        }
        let (start, end) = self.visible_bar_range(width_px, total_bars);
        if start >= end {
            return (start, end);
        }
        let start = start.saturating_sub(CULL_OVERSCAN_BARS);
        let end = end.saturating_add(CULL_OVERSCAN_BARS).min(total_bars);
        (start, end)
    }

    /// The `[start, end)` bar indices kept by the proportional cull: the
    /// visible range widened to at most twice its width, clamped to
    /// `[0, total_bars]`. Composes [`visible_bar_range`](Self::visible_bar_range),
    /// never a second copy of its math. From `(vs, ve)` with `V = ve - vs`,
    /// `start = vs - min(vs, V)` and `end = min(len, start + 2V)` — at most
    /// twice the visible width, always covering the visible range. Empty stays
    /// empty and zero-area panes paint nothing, the same edge rules as the
    /// constant-overscan helper.
    #[must_use]
    pub fn proportional_bar_range(self, width_px: f32, total_bars: usize) -> (usize, usize) {
        if width_px <= 0.0 {
            return (0, 0);
        }
        let (visible_start, visible_end) = self.visible_bar_range(width_px, total_bars);
        if visible_start >= visible_end {
            return (visible_start, visible_end);
        }
        let width = visible_end - visible_start;
        let start = visible_start.saturating_sub(width.min(visible_start));
        let end = start.saturating_add(width.saturating_mul(PROPORTIONAL_CULL_FACTOR)).min(total_bars);
        (start, end)
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

    /// Map a (possibly fractional) bar slot to an x pixel: the continuous
    /// form of [`index_to_x`](Self::index_to_x), used by the cursor-anchored
    /// zoom math to keep a fractional slot stationary.
    #[must_use]
    pub fn slot_to_x(self, slot: f64) -> f32 {
        (slot * f64::from(self.scale.bar_spacing()) + f64::from(self.x_offset_px)) as f32
    }

    /// Map a bar index to an x pixel.
    #[must_use]
    pub fn index_to_x(self, index: usize) -> f32 {
        self.slot_to_x(index as f64)
    }

    /// Invert [`index_to_x`](Self::index_to_x); `None` on non-finite input.
    #[must_use]
    pub fn x_to_index(self, x: f32) -> Option<f64> {
        let x = f64::from(x) - f64::from(self.x_offset_px);
        self.scale.x_to_index(x as f32)
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

/// Window a slice without copying; saturates out-of-range bounds
/// instead of panicking, so scroll positions past the data edge show an
/// empty (or trailing) window rather than aborting the frame. Converted rows
/// and their parallel key series share this one slicer, so both windows
/// always agree.
#[must_use]
pub fn visible_rows<T>(rows: &[T], start: usize, count: usize) -> &[T] {
    let start = start.min(rows.len());
    let end = start.saturating_add(count).min(rows.len());
    &rows[start..end]
}

/// Fit the price range to a bar window: min over the window's lows, max over
/// its highs, plus symmetric padding of `pad_fraction` times the window span
/// on each side.
///
/// The headless AutoHeight policy (task value: legibility — wicks visible,
/// no dead space; it claims nothing about whether the window is tradeable).
/// Refuses empty rows, empty or out-of-bounds windows, flat or non-finite
/// extremes, and non-finite or negative padding, all with reasons. NaN legs
/// fold out (a hole carries no geometry); an all-NaN window refuses as
/// degenerate. The caller holds the viewport and rebuilds it through the
/// same constructor path as every other range change, so padding never
/// inverts: a finite non-negative fraction keeps `min < max`.
pub fn auto_height_range(
    rows: &[ChartRow],
    window: (usize, usize),
    pad_fraction: f64,
) -> Result<PriceRange, EguiError> {
    if !pad_fraction.is_finite() || pad_fraction < 0.0 {
        return Err(EguiError::InvalidParam {
            what: "pad_fraction".to_owned(),
            reason: "auto-height padding must be finite and non-negative".to_owned(),
        });
    }
    let (start, end) = window;
    if rows.is_empty() || start >= end {
        return Err(EguiError::InvalidParam {
            what: "window".to_owned(),
            reason: "auto-height needs a non-empty bar window".to_owned(),
        });
    }
    let Some(view) = rows.get(start..end) else {
        return Err(EguiError::InvalidParam {
            what: "window".to_owned(),
            reason: "auto-height window reaches past the loaded rows".to_owned(),
        });
    };
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for row in view {
        lo = lo.min(row.low);
        hi = hi.max(row.high);
    }
    if !lo.is_finite() || !hi.is_finite() || lo >= hi {
        return Err(EguiError::InvalidParam {
            what: "window".to_owned(),
            reason: "auto-height needs a window with finite non-flat extremes".to_owned(),
        });
    }
    let pad = (hi - lo) * pad_fraction;
    PriceRange::new(lo - pad, hi + pad).map_err(describe_scale)
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
