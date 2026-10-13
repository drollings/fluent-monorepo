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
//! What the machine owns: the vertical price window only. A drag pans it
//! with grab semantics (content follows the pointer); wheel and pinch zoom
//! around the pointer's price anchor; a box select sets the spanned range.
//! Horizontal scroll offset is caller-owned (`visible_rows` takes the start
//! bar), so drag carries no horizontal component. Released motion coasts
//! with exponential decay integrated in closed form, so splitting one time
//! interval into smaller steps keeps the trajectory.
//!
//! The window may stretch past the data extent into an elastic margin, but
//! the hard envelope is never exceeded: contact translates the window back
//! inside (span preserved) and absorbs the coast velocity. Zoom and box
//! events re-anchor and stop any coast; off-pane anchors name no price, so
//! those events hold the window.
//!
//! Motion constants below are feel heuristics (task value, not producer
//! confidence): they ship so the frame path has something to wire, and the
//! calibration harness measures them before any frame consumes them.
//!
//! [`Viewport`]: crate::viewport::Viewport
//! [`visible_rows`]: crate::viewport::visible_rows

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
}

/// Pure interaction state: elastic data bounds plus kinetic velocity.
///
/// Construct with the data extent the window may roam (plus its elastic
/// margin); advance a viewport with [`step`](Self::step). The window itself
/// stays caller-held, so replay only needs the event script.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InteractionState {
    /// Soft lower data bound (window may stretch below, into the margin).
    soft_min: f64,
    /// Soft upper data bound.
    soft_max: f64,
    /// Coast velocity in price per second; exactly zero at rest.
    velocity: f64,
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
        Ok(Self { soft_min: lo, soft_max: hi, velocity: 0.0 })
    }

    /// Current coast velocity in price per second; zero at rest.
    #[must_use]
    pub fn kinetic_velocity(self) -> f64 {
        self.velocity
    }

    /// Whether no coast is in flight.
    #[must_use]
    pub fn is_settled(self) -> bool {
        self.velocity == 0.0
    }

    /// Advance `viewport` by `dt` seconds under `events`, returning the next
    /// window. Discrete gestures apply as displacements (dt-independent);
    /// empty steps coast the seeded velocity with closed-form exponential
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
            return *viewport;
        };
        let height = f64::from(scale.height());
        let dt_valid = dt.is_finite() && dt > 0.0;
        let (mut min, mut max) = (scale.range().min, scale.range().max);

        if events.is_empty() {
            if dt_valid && self.velocity != 0.0 {
                // Closed form: displacement = v0 * (1 - k) / lambda with
                // lambda = -ln(base) * 60, so splitting one interval into
                // smaller steps keeps the trajectory.
                let decay = KINETIC_DAMPING_BASE.powf(dt * 60.0);
                let lambda = -KINETIC_DAMPING_BASE.ln() * 60.0;
                let displacement = self.velocity * (1.0 - decay) / lambda;
                min += displacement;
                max += displacement;
                self.velocity *= decay;
                if self.velocity.abs() <= REST_FRACTION_OF_SPAN * (max - min) {
                    self.velocity = 0.0;
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
                        if dt_valid {
                            let rate = shift / dt;
                            self.velocity =
                                (1.0 - VELOCITY_BLEND) * self.velocity + VELOCITY_BLEND * rate;
                        }
                    }
                    PointerInput::Wheel { delta_px, anchor_y } => {
                        if !delta_px.is_finite() {
                            continue;
                        }
                        let Some(anchor) = viewport.y_to_price(anchor_y) else { continue };
                        let factor =
                            WHEEL_ZOOM_BASE.powf(-f64::from(delta_px) / WHEEL_STEP_PX);
                        (min, max) = zoom_around(min, max, anchor, factor);
                        self.velocity = 0.0;
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
            return *viewport;
        }
        Viewport::new(
            scale.mode(),
            fitted.0,
            fitted.1,
            scale.height(),
            scale.bar_spacing(),
            scale.base_value(),
            scale.inverted(),
        )
        .unwrap_or_else(|_| {
            self.velocity = 0.0;
            *viewport
        })
    }
}

/// Rescale the window span by `factor`, keeping `anchor` fixed.
fn zoom_around(min: f64, max: f64, anchor: f64, factor: f64) -> (f64, f64) {
    (anchor - (anchor - min) * factor, anchor + (max - anchor) * factor)
}

#[cfg(test)]
#[path = "../tests/interaction.rs"]
mod tests;
