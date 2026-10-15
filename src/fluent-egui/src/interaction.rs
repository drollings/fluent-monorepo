//! Pure pointer-interaction state machine over the price window.
//!
//! Headless and deterministic: an [`InteractionState`] owns the elastic data
//! bounds plus one kinetic velocity (price per second) and advances a
//! caller-held [`Viewport`] through a single entry point,
//! [`step`](InteractionState::step). Elapsed time is injected (`dt`), never
//! read from a clock, so headless replay reproduces frames bit-for-bit and
//! golden tests never flake. The windowing shell translates pointer events
//! into [`PointerInput`]; this module never names `egui`.
//!
//! What the machine owns: the vertical price window and the horizontal time
//! axis. A drag pans the price window with grab semantics (content follows
//! the pointer); a 2D pan moves both axes; wheel and pinch zoom the price
//! window around the pointer's price anchor; a box select sets the spanned
//! range; and a time-axis wheel scales `bar_spacing` around the bar slot
//! under the cursor, keeping that bar stationary. Released motion on either
//! axis coasts with exponential decay integrated in closed form, so
//! splitting one time interval into smaller steps keeps the trajectory.
//!
//! The window may stretch past the data extent into an elastic margin, but
//! the hard envelope is never exceeded: contact translates the window back
//! inside (span preserved) and absorbs the coast velocity. The horizontal
//! envelope is configured with
//! [`with_horizontal_bounds`](InteractionState::with_horizontal_bounds) and
//! clamps the time-axis offset the same way. Zoom and box events re-anchor
//! and stop any coast; off-pane anchors name no price, so those events hold
//! the window.
//!
//! Motion constants below are feel heuristics (task value, not producer
//! confidence): they ship so the frame path has something to wire, and the
//! calibration harness measures them before any frame consumes them.
//!
//! [`Viewport`]: crate::viewport::Viewport
//! [`visible_rows`]: crate::viewport::visible_rows

use protopyne_chart_scale::PriceRange;

use crate::error::EguiError;
use crate::viewport::Viewport;

/// Kinetic damping (task value: feel): fraction of velocity retained per
/// 1/60 s frame, applied as `KINETIC_DAMPING_BASE.powf(dt * 60)` so decay is
/// dt-parameterized.
pub const KINETIC_DAMPING_BASE: f64 = 0.92;

/// Wheel zoom per step (task value: feel): the span scales by this base per
/// `WHEEL_STEP_PX` pixels scrolled (scroll up zooms in).
pub const WHEEL_ZOOM_BASE: f64 = 1.15;
/// Pixels of wheel scroll per unit zoom step (task value: feel).
pub const WHEEL_STEP_PX: f64 = 50.0;

/// Rest threshold (task value: feel): the coast snaps to exact zero once the
/// velocity drops to this fraction of the window span, so rest is
/// scale-free across prices.
pub const REST_FRACTION_OF_SPAN: f64 = 1e-4;

/// Elastic margin per side (task value: feel), as a fraction of the data
/// extent: the window may stretch this far past the data before the hard
/// envelope stops it.
pub const ELASTIC_FRACTION: f64 = 0.15;

/// Drag velocity smoothing (task value: feel): each drag blends this
/// fraction of its displacement rate into the running velocity.
pub const VELOCITY_BLEND: f64 = 0.5;

/// Minimum horizontal bar spacing in pixels (task value: candle legibility):
/// the time-axis zoom rail, below which candles would collapse into a smear.
pub const MIN_BAR_SPACING: f32 = 1.0;

/// Maximum horizontal bar spacing in pixels (task value: candle legibility):
/// the time-axis zoom rail, above which too few candles fit to read.
pub const MAX_BAR_SPACING: f32 = 64.0;

/// Cursor-anchor drift tolerance in pixels (task value: anchor stability):
/// across a cursor-anchored time-axis zoom the bar under the cursor must stay
/// within this distance of its anchor pixel.
pub const ANCHOR_DRIFT_EPS_PX: f32 = 1e-3;

/// One translated pointer gesture. The windowing shell builds these from raw
/// input; every field is headless (pixels, scale factors, no `egui` types).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PointerInput {
    /// Vertical drag in pixels (positive pulls content down): pans the price
    /// window with grab semantics and seeds kinetic velocity.
    Drag {
        /// Pointer travel in pixels; positive moves content down.
        dy_px: f32,
    },
    /// Wheel scroll: zooms the price window around the price at `anchor_y`.
    /// Positive `delta_px` (scroll up) zooms in.
    Wheel {
        /// Scroll travel in pixels.
        delta_px: f32,
        /// Pane-local y pixel the zoom keeps fixed.
        anchor_y: f32,
    },
    /// Pinch: zooms around the anchor price; `scale > 1` zooms in.
    Pinch {
        /// Multiplicative zoom factor; must be positive and finite.
        scale: f32,
        /// Pane-local y pixel the zoom keeps fixed.
        anchor_y: f32,
    },
    /// Box select: sets the window to the price range spanned by the two
    /// pane-local y ends (order-independent).
    BoxZoom {
        /// One box end in pane-local y pixels.
        top_y: f32,
        /// Other box end in pane-local y pixels.
        bottom_y: f32,
    },
    /// Two-axis drag in pixels (positive pulls content down and right): pans
    /// the price window and the time axis together, seeding both coast
    /// velocities. Grab semantics: the content under the pointer follows it.
    Pan2D {
        /// Horizontal pointer travel in pixels; positive moves content right.
        dx_px: f32,
        /// Vertical pointer travel in pixels; positive moves content down.
        dy_px: f32,
    },
    /// Time-axis wheel: scales the horizontal `bar_spacing` around the bar
    /// slot under `anchor_x`, keeping that bar stationary. Positive
    /// `delta_px` (scroll up) zooms in.
    WheelTime {
        /// Scroll travel in pixels.
        delta_px: f32,
        /// Pane-local x pixel the zoom keeps fixed.
        anchor_x: f32,
    },
}

/// Pure interaction state: elastic data bounds plus kinetic velocity.
///
/// Construct with the data extent the window may roam (plus its elastic
/// margin); advance a viewport with [`step`](Self::step). The window itself
/// stays caller-held, so replay only needs the event script. The machine
/// tracks two independent coasts: the vertical price window and the
/// horizontal time axis (`x_offset_px`). Horizontal elastic bounds are
/// optional; configure them with
/// [`with_horizontal_bounds`](Self::with_horizontal_bounds) so panning and
/// time-axis coasting stay inside the data envelope.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InteractionState {
    /// Soft lower data bound (window may stretch below, into the margin).
    soft_min: f64,
    /// Soft upper data bound.
    soft_max: f64,
    /// Vertical coast velocity in price per second; exactly zero at rest.
    velocity: f64,
    /// Horizontal coast velocity in pixels per second; zero at rest.
    velocity_x: f64,
    /// Optional horizontal envelope `(total_bars, pane_width_px)`; `None`
    /// leaves the time axis unclamped.
    horizontal: Option<(f64, f64)>,
}

impl InteractionState {
    /// Bind the machine to a data extent; refuses non-finite or inverted
    /// bounds. Starts at rest.
    pub fn new(lo: f64, hi: f64) -> Result<Self, EguiError> {
        if !lo.is_finite() || !hi.is_finite() {
            return Err(EguiError::InvalidParam {
                what: "bounds".to_owned(),
                reason: "interaction bounds must be finite".to_owned(),
            });
        }
        if lo >= hi {
            return Err(EguiError::InvalidParam {
                what: "bounds".to_owned(),
                reason: "lower interaction bound must sit below the upper".to_owned(),
            });
        }
        Ok(Self {
            soft_min: lo,
            soft_max: hi,
            velocity: 0.0,
            velocity_x: 0.0,
            horizontal: None,
        })
    }

    /// Bind the horizontal elastic envelope to `total_bars` over a pane
    /// `pane_width_px` wide; refuses a zero bar count or non-positive,
    /// non-finite pane width. Without this the time axis pans and coasts
    /// unclamped.
    pub fn with_horizontal_bounds(
        mut self,
        total_bars: usize,
        pane_width_px: f32,
    ) -> Result<Self, EguiError> {
        if total_bars == 0 {
            return Err(EguiError::InvalidParam {
                what: "total_bars".to_owned(),
                reason: "horizontal bounds need at least one bar".to_owned(),
            });
        }
        if !pane_width_px.is_finite() || pane_width_px <= 0.0 {
            return Err(EguiError::InvalidParam {
                what: "pane_width_px".to_owned(),
                reason: "pane width must be positive and finite".to_owned(),
            });
        }
        self.horizontal = Some((total_bars as f64, f64::from(pane_width_px)));
        Ok(self)
    }

    /// Current coast velocity in price per second; zero at rest.
    #[must_use]
    pub fn kinetic_velocity(self) -> f64 {
        self.velocity
    }

    /// Current horizontal coast velocity in pixels per second; zero at rest.
    #[must_use]
    pub fn kinetic_velocity_x(self) -> f64 {
        self.velocity_x
    }

    /// Whether no coast is in flight on either axis.
    #[must_use]
    pub fn is_settled(self) -> bool {
        self.velocity == 0.0 && self.velocity_x == 0.0
    }

    /// Reference span for the horizontal rest snap: the pane width when the
    /// envelope is configured, else a unit span (horizontal coast is only
    /// meaningful with bounds, but the snap must never divide by zero).
    fn horizontal_reference(&self) -> f64 {
        self.horizontal.map_or(1.0, |(_, width)| width)
    }

    /// Advance `viewport` by `dt` seconds under `events`, returning the next
    /// window. Discrete gestures apply as displacements (dt-independent);
    /// empty steps coast the seeded velocities with closed-form exponential
    /// decay. Events that name no price (off-pane anchors, degenerate boxes)
    /// hold the window; a rebuild the scale refuses also holds the last good
    /// window and stops the coast.
    #[must_use]
    pub fn step(&mut self, dt: f64, viewport: &Viewport, events: &[PointerInput]) -> Viewport {
        let scale = viewport.scale();
        // Edge-sampled price-per-pixel: exact for linear scales, first-order
        // for log/percentage (small per-frame gestures). Unreachable for a
        // constructed viewport; guarded anyway.
        let (Some(top), Some(bottom)) =
            (viewport.y_to_price(0.0), viewport.y_to_price(scale.height()))
        else {
            self.velocity = 0.0;
            self.velocity_x = 0.0;
            return *viewport;
        };
        let height = f64::from(scale.height());
        let dt_valid = dt.is_finite() && dt > 0.0;
        let (mut min, mut max) = (scale.range().min, scale.range().max);
        // Horizontal state carried on the caller's viewport, advanced here.
        let mut spacing = f64::from(scale.bar_spacing());
        let mut x_offset = f64::from(viewport.x_offset_px());

        if events.is_empty() {
            if dt_valid {
                // Closed form: displacement = v0 * (1 - k) / lambda with
                // lambda = -ln(base) * 60, so splitting one interval into
                // smaller steps keeps the trajectory.
                let decay = KINETIC_DAMPING_BASE.powf(dt * 60.0);
                let lambda = -KINETIC_DAMPING_BASE.ln() * 60.0;
                if self.velocity != 0.0 {
                    let displacement = self.velocity * (1.0 - decay) / lambda;
                    min += displacement;
                    max += displacement;
                    self.velocity *= decay;
                    if self.velocity.abs() <= REST_FRACTION_OF_SPAN * (max - min) {
                        self.velocity = 0.0;
                    }
                }
                if self.velocity_x != 0.0 {
                    let displacement = self.velocity_x * (1.0 - decay) / lambda;
                    x_offset += displacement;
                    self.velocity_x *= decay;
                    if self.velocity_x.abs()
                        <= REST_FRACTION_OF_SPAN * self.horizontal_reference()
                    {
                        self.velocity_x = 0.0;
                    }
                }
            }
        } else {
            for event in events {
                match *event {
                    PointerInput::Drag { dy_px } => {
                        if !dy_px.is_finite() {
                            continue;
                        }
                        let shift = f64::from(dy_px) * (top - bottom) / height;
                        min += shift;
                        max += shift;
                        // The displacement always applies; the rate seeds a
                        // coast only when it is finite. A subnormal-but-
                        // positive `dt` would overflow `shift / dt` into an
                        // infinite coast velocity, so finiteness of the
                        // quotient — not just positivity of `dt` — gates it.
                        if dt_valid {
                            let rate = shift / dt;
                            if rate.is_finite() {
                                self.velocity =
                                    (1.0 - VELOCITY_BLEND) * self.velocity + VELOCITY_BLEND * rate;
                            }
                        }
                    }
                    PointerInput::Pan2D { dx_px, dy_px } => {
                        if dy_px.is_finite() {
                            let shift = f64::from(dy_px) * (top - bottom) / height;
                            min += shift;
                            max += shift;
                            if dt_valid {
                                let rate = shift / dt;
                                if rate.is_finite() {
                                    self.velocity = (1.0 - VELOCITY_BLEND) * self.velocity
                                        + VELOCITY_BLEND * rate;
                                }
                            }
                        }
                        if dx_px.is_finite() {
                            x_offset += f64::from(dx_px);
                            if dt_valid {
                                let rate = f64::from(dx_px) / dt;
                                if rate.is_finite() {
                                    self.velocity_x = (1.0 - VELOCITY_BLEND) * self.velocity_x
                                        + VELOCITY_BLEND * rate;
                                }
                            }
                        }
                    }
                    PointerInput::Wheel { delta_px, anchor_y } => {
                        if !delta_px.is_finite() {
                            continue;
                        }
                        let Some(anchor) = viewport.y_to_price(anchor_y) else { continue };
                        // Same factor and the same rescale a caller gets from
                        // `wheel_zoom_factor` / `zoom_range_around`, so a
                        // pane that zooms outside the machine still feels and
                        // behaves identically to one that steps it.
                        let factor = wheel_zoom_factor(delta_px);
                        let Some(next) =
                            zoom_range_around(scale.range(), anchor, factor)
                        else {
                            continue;
                        };
                        (min, max) = (next.min, next.max);
                        self.velocity = 0.0;
                    }
                    PointerInput::WheelTime { delta_px, anchor_x } => {
                        if !delta_px.is_finite() || !anchor_x.is_finite() {
                            continue;
                        }
                        // Scroll up (positive) widens the bars — zoom in on
                        // the time axis, mirroring the price-axis wheel.
                        let factor =
                            WHEEL_ZOOM_BASE.powf(f64::from(delta_px) / WHEEL_STEP_PX);
                        let new_spacing = (spacing * factor)
                            .clamp(f64::from(MIN_BAR_SPACING), f64::from(MAX_BAR_SPACING));
                        // Cursor anchor: solve new_offset = anchor_x - slot *
                        // new_spacing with slot the bar slot under the
                        // cursor, so that slot maps back to exactly
                        // anchor_x after the spacing changes.
                        let slot = (f64::from(anchor_x) - x_offset) / spacing;
                        x_offset = f64::from(anchor_x) - slot * new_spacing;
                        spacing = new_spacing;
                        self.velocity_x = 0.0;
                    }
                    PointerInput::Pinch { scale: pinch, anchor_y } => {
                        if !pinch.is_finite() || pinch <= 0.0 {
                            continue;
                        }
                        let Some(anchor) = viewport.y_to_price(anchor_y) else { continue };
                        (min, max) = zoom_around(min, max, anchor, 1.0 / f64::from(pinch));
                        self.velocity = 0.0;
                    }
                    PointerInput::BoxZoom { top_y, bottom_y } => {
                        let (Some(first), Some(second)) =
                            (viewport.y_to_price(top_y), viewport.y_to_price(bottom_y))
                        else {
                            continue;
                        };
                        (min, max) = (first.min(second), first.max(second));
                        self.velocity = 0.0;
                    }
                }
            }
        }

        // Elastic envelope: the data extent plus the margin per side.
        let data_span = self.soft_max - self.soft_min;
        let margin = ELASTIC_FRACTION * data_span;
        let (lo_lim, hi_lim) = (self.soft_min - margin, self.soft_max + margin);
        let mut fitted = (min, max);
        if min.is_finite() && max.is_finite() && min < max {
            let span = max - min;
            if span <= hi_lim - lo_lim {
                // Translate-to-fit: the span survives, the wall only moves it.
                let mut shift = 0.0;
                if min < lo_lim {
                    shift += lo_lim - min;
                }
                if max + shift > hi_lim {
                    shift -= (max + shift) - hi_lim;
                }
                fitted = (min + shift, max + shift);
            } else {
                fitted = (min.max(lo_lim), max.min(hi_lim));
            }
        }
        if fitted != (min, max) {
            // Wall contact absorbs the coast.
            self.velocity = 0.0;
        }
        if fitted.0 >= fitted.1 {
            self.velocity = 0.0;
            self.velocity_x = 0.0;
            return *viewport;
        }

        // Horizontal elastic envelope: the visible slot window `[s0, s1]`
        // stays within `[-margin_slots, total_bars + margin_slots]`, so the
        // pane may overhang the data by the same fraction as the price axis.
        if let Some((total_bars, pane_width)) = self.horizontal {
            let margin_slots = ELASTIC_FRACTION * total_bars;
            let lo_lim = pane_width - (total_bars + margin_slots) * spacing;
            let hi_lim = margin_slots * spacing;
            let (lo, hi) = (lo_lim.min(hi_lim), lo_lim.max(hi_lim));
            if x_offset < lo || x_offset > hi {
                x_offset = x_offset.clamp(lo, hi);
                self.velocity_x = 0.0;
            }
        }

        Viewport::new(
            scale.mode(),
            fitted.0,
            fitted.1,
            scale.height(),
            spacing as f32,
            scale.base_value(),
            scale.inverted(),
        )
        .and_then(|next| next.with_x_offset(x_offset as f32))
        .unwrap_or_else(|_| {
            self.velocity = 0.0;
            self.velocity_x = 0.0;
            *viewport
        })
    }
}

/// Rescale the window span by `factor`, keeping `anchor` fixed.
fn zoom_around(min: f64, max: f64, anchor: f64, factor: f64) -> (f64, f64) {
    (anchor - (anchor - min) * factor, anchor + (max - anchor) * factor)
}

/// The multiplicative factor a wheel gesture of `delta_px` applies to a price
/// window: scroll up (positive) zooms in, matching the time-axis wheel. The
/// one definition of "how far a notch zooms" — a pane that zooms outside
/// [`InteractionState`] (an oscillator pane, say) uses this so both paths feel
/// identical.
#[must_use]
pub fn wheel_zoom_factor(delta_px: f32) -> f64 {
    WHEEL_ZOOM_BASE.powf(-f64::from(delta_px) / WHEEL_STEP_PX)
}

/// Rescale `range` by `factor` around `anchor`, keeping the price at `anchor`
/// fixed. Refuses (`None`) on a non-finite factor or an anchor outside the
/// range, so a caller never receives an inverted window.
#[must_use]
pub fn zoom_range_around(range: PriceRange, anchor: f64, factor: f64) -> Option<PriceRange> {
    if !factor.is_finite() || factor <= 0.0 || !anchor.is_finite() {
        return None;
    }
    let (min, max) = (range.min, range.max);
    let (next_min, next_max) = zoom_around(min, max, anchor, factor);
    PriceRange::new(next_min, next_max).ok()
}

#[cfg(test)]
#[path = "../tests/interaction.rs"]
mod tests;
