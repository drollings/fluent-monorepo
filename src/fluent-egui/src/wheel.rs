//! Region-aware wheel routing: where a wheel gesture acts.
//!
//! A chart is not one surface: the candle plot scrolls on the time (x) axis,
//! each pane's price labels drive that pane's own y zoom, and a floating list
//! (the hot-slot legend) scrolls under the pointer instead of the chart. This
//! module is the single decision that maps a hover point to *what a wheel acts
//! on*, kept headless (plain `f32` rectangles, no `egui` types) so the routing
//! is unit-testable without a UI context and the terminal never re-decides it.
//!
//! Routing is disjoint by construction: zones are tested in a fixed priority
//! (list, then each pane's axis strip, then its plot) and the first zone whose
//! rectangle contains the point wins, so a point on a boundary belongs to
//! exactly one target. Each target carries a *pane-local* anchor — the pixel
//! the gesture keeps fixed — so the caller forwards it straight into the
//! existing [`crate::interaction`] wheel inputs without re-deriving geometry.
//!
//! The module names no indicator, no pane layout, and no domain: it routes
//! rectangles. Which rectangle is a price axis or a list is the caller's
//! vocabulary, decided once when it builds the [`WheelZones`].

/// What a wheel gesture over a chart region acts on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WheelTarget {
    /// Zoom the shared time (x) axis, keeping the bar under the anchor pixel
    /// fixed. `anchor_x` is pane-local.
    Time { anchor_x: f32 },
    /// Zoom a pane's price (y) axis, keeping the price under the anchor pixel
    /// fixed. `pane` identifies the zone's pane (see [`WheelZones::route`]),
    /// and `anchor_y` is that pane's local y.
    Price { pane: usize, anchor_y: f32 },
    /// Scroll a floating list under the pointer; the chart does not move.
    List,
}

impl WheelTarget {
    /// Whether this target zooms a time or price axis (i.e. the caller should
    /// forward a zoom) as opposed to scrolling a list.
    #[must_use]
    pub fn is_zoom(&self) -> bool {
        !matches!(self, Self::List)
    }
}

/// One pane's wheel zones: its data plot and its right-hand price axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaneZones {
    /// The pane's data area (candles / sub-pane series). A wheel here zooms
    /// the shared time axis.
    pub plot: Rect,
    /// The pane's right-hand price-label strip. A wheel here zooms this
    /// pane's own price axis.
    pub axis: Rect,
}

/// The wheel zones of one chart frame: the price pane, the oscillator pane,
/// and the floating list, resolved into disjoint hit regions.
///
/// Built once per frame from the pane layout; handed to
/// [`route`](Self::route) with the pointer to decide the target. Priority is
/// list, then each pane in order, so an overlay on top of a pane wins.
#[derive(Debug, Clone, Default)]
pub struct WheelZones {
    /// Panes in priority order (index 0 is the price pane, index 1 the
    /// oscillator pane, …). The index doubles as the `pane` field of a
    /// [`WheelTarget::Price`].
    panes: Vec<PaneZones>,
    /// Optional floating list rect (the hot-slot legend); a wheel inside it
    /// scrolls instead of zooming.
    list: Option<Rect>,
}

impl WheelZones {
    /// Build zones from panes (in priority order) and an optional list rect.
    #[must_use]
    pub fn new(panes: Vec<PaneZones>, list: Option<Rect>) -> Self {
        Self { panes, list }
    }

    /// Add a pane's zones in priority order (append). Returns `self` so the
    /// builder reads as a chain.
    #[must_use]
    pub fn with_pane(mut self, pane: PaneZones) -> Self {
        self.panes.push(pane);
        self
    }

    /// Set the floating list rect. Returns `self`.
    #[must_use]
    pub fn with_list(mut self, list: Rect) -> Self {
        self.list = Some(list);
        self
    }

    /// Decide what a wheel at `point` acts on, testing zones in a fixed
    /// priority: the list first (so an overlay wins), then each pane's axis
    /// strip, then its plot. Returns `None` when the point is outside every
    /// zone (the caller leaves the chart alone). Each returned target carries
    /// a pane-local anchor derived from the matched zone's plot origin.
    #[must_use]
    pub fn route(&self, point: Pos2) -> Option<WheelTarget> {
        if self.list.is_some_and(|rect| rect.contains(point)) {
            return Some(WheelTarget::List);
        }
        for (pane, zones) in self.panes.iter().enumerate() {
            if zones.axis.contains(point) {
                return Some(WheelTarget::Price {
                    pane,
                    anchor_y: point.y - zones.plot.top(),
                });
            }
            if zones.plot.contains(point) {
                return Some(WheelTarget::Time { anchor_x: point.x - zones.plot.left() });
            }
        }
        None
    }
}

/// A minimal 2D rectangle in screen pixels. Headless by design so routing is
/// testable without an `egui` context; callers with an `egui::Rect` convert at
/// the boundary via [`Rect::new`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub min_x: f32,
    /// Top edge.
    pub min_y: f32,
    /// Right edge.
    pub max_x: f32,
    /// Bottom edge.
    pub max_y: f32,
}

impl Rect {
    /// Build a rectangle from its two corners, normalizing so `min <= max` on
    /// both axes (a swapped pair still yields a usable region).
    #[must_use]
    pub fn new(a: Pos2, b: Pos2) -> Self {
        Self {
            min_x: a.x.min(b.x),
            min_y: a.y.min(b.y),
            max_x: a.x.max(b.x),
            max_y: a.y.max(b.y),
        }
    }

    /// Left edge.
    #[must_use]
    pub fn left(&self) -> f32 {
        self.min_x
    }

    /// Top edge.
    #[must_use]
    pub fn top(&self) -> f32 {
        self.min_y
    }

    /// Whether `point` lies within the rectangle (inclusive of edges).
    #[must_use]
    pub fn contains(&self, point: Pos2) -> bool {
        point.x >= self.min_x && point.x <= self.max_x && point.y >= self.min_y
            && point.y <= self.max_y
    }
}

/// A minimal 2D point in screen pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pos2 {
    /// Horizontal pixel.
    pub x: f32,
    /// Vertical pixel.
    pub y: f32,
}

impl Pos2 {
    /// Build a point.
    #[must_use]
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// The one place `egui` geometry crosses into the headless router: the
/// terminal builds its pane layout with `egui` rects and converts them here,
/// so `route` itself never names the paint library.
impl From<egui::Rect> for Rect {
    fn from(rect: egui::Rect) -> Self {
        Self::new(
            Pos2::new(rect.min.x, rect.min.y),
            Pos2::new(rect.max.x, rect.max.y),
        )
    }
}

impl From<egui::Pos2> for Pos2 {
    fn from(point: egui::Pos2) -> Self {
        Self::new(point.x, point.y)
    }
}

#[cfg(test)]
#[path = "../tests/wheel.rs"]
mod tests;