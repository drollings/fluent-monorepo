//! Price-scale modes, ranges, and index/price mapping.
//!
//! One truth for both directions: `price_to_y`/`y_to_price` and
//! `index_to_x`/`x_to_index` invert each other over the configured geometry.
//! Logarithmic mode maps `ln(price)`; percentage and indexed modes rebase
//! onto the first visible value. Every fallible entry refuses degenerate
//! geometry instead of dividing by zero.

use std::str::FromStr;

use crate::error::ScaleError;

/// How raw prices are reshaped before mapping to pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum PriceScaleMode {
    /// Absolute prices, linear.
    #[default]
    Normal,
    /// Natural-log prices, linear in log space.
    Logarithmic,
    /// Percent change from the first visible value (that value reads 0%).
    Percentage,
    /// Rebased so the first visible value reads 100.
    IndexedTo100,
}

impl std::fmt::Display for PriceScaleMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Normal => f.write_str("Normal"),
            Self::Logarithmic => f.write_str("Logarithmic"),
            Self::Percentage => f.write_str("Percentage"),
            Self::IndexedTo100 => f.write_str("Indexed to 100"),
        }
    }
}

impl FromStr for PriceScaleMode {
    type Err = ScaleError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "normal" => Ok(Self::Normal),
            "logarithmic" | "log" => Ok(Self::Logarithmic),
            "percentage" | "percent" | "%" => Ok(Self::Percentage),
            "indexedto100" | "indexed to 100" | "indexed" => Ok(Self::IndexedTo100),
            _ => Err(ScaleError::InvalidParam {
                what: "mode".to_owned(),
                reason: format!("unknown price scale mode {s:?}"),
            }),
        }
    }
}

/// A min/max price window in display space.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PriceRange {
    /// Lowest visible price.
    pub min: f64,
    /// Highest visible price.
    pub max: f64,
}

impl PriceRange {
    /// Window over `[min, max]`; refuses non-finite legs and inverted ranges.
    pub fn new(min: f64, max: f64) -> Result<Self, ScaleError> {
        if !min.is_finite() || !max.is_finite() {
            return Err(ScaleError::InvalidParam {
                what: "range".to_owned(),
                reason: "bounds must be finite".to_owned(),
            });
        }
        if min >= max {
            return Err(ScaleError::InvalidParam {
                what: "range".to_owned(),
                reason: "min must be below max".to_owned(),
            });
        }
        Ok(Self { min, max })
    }

    /// Span (`max - min`), clamped above zero so mapping never divides by zero.
    #[must_use]
    pub fn length(self) -> f64 {
        (self.max - self.min).max(1e-12)
    }

    /// Whether `price` sits inside `[min, max]`.
    #[must_use]
    pub fn contains(self, price: f64) -> bool {
        price >= self.min && price <= self.max
    }
}

/// Index/price mapping over one pane of fixed pixel geometry.
///
/// `bar_spacing` is pixels per bar, `height` the pane height in pixels, and
/// `base_value` the first-visible reference for percentage/indexed modes.
/// Construct once per visible window; recompute only on scroll/zoom, never
/// per frame per bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PriceScale {
    mode: PriceScaleMode,
    range: PriceRange,
    height: f32,
    bar_spacing: f32,
    base_value: f64,
    inverted: bool,
    /// `range.min`/`range.max` reshaped into scale space, computed once at
    /// construction. `None` when the range has no scaled image (the
    /// degenerate-span guard): construction still succeeds and every map
    /// refuses, exactly as before the pair was cached.
    scaled_bounds: Option<(f64, f64)>,
}

/// Reshape one price into scale space (log/percentage/indexed/normal). The one
/// implementation construction (the endpoints) and mapping (per price) share.
fn to_scaled(mode: PriceScaleMode, base_value: f64, price: f64) -> Option<f64> {
    if !price.is_finite() {
        return None;
    }
    match mode {
        PriceScaleMode::Normal => Some(price),
        PriceScaleMode::Logarithmic => (price > 0.0).then(|| price.ln()),
        PriceScaleMode::Percentage => Some((price - base_value) / base_value * 100.0),
        PriceScaleMode::IndexedTo100 => Some(price / base_value * 100.0),
    }
}

/// Invert [`to_scaled`].
fn unscale(mode: PriceScaleMode, base_value: f64, scaled: f64) -> Option<f64> {
    if !scaled.is_finite() {
        return None;
    }
    match mode {
        PriceScaleMode::Normal => Some(scaled),
        PriceScaleMode::Logarithmic => Some(scaled.exp()),
        PriceScaleMode::Percentage => Some(base_value * (scaled / 100.0 + 1.0)),
        PriceScaleMode::IndexedTo100 => Some(base_value * scaled / 100.0),
    }
}

/// Scale-space window bounds, or `None` when the range has no scaled image (a
/// degenerate span or a non-representable endpoint).
fn scaled_bounds(mode: PriceScaleMode, base_value: f64, range: PriceRange) -> Option<(f64, f64)> {
    let low = to_scaled(mode, base_value, range.min)?;
    let high = to_scaled(mode, base_value, range.max)?;
    if (low - high).abs() < 1e-12 || !low.is_finite() || !high.is_finite() {
        return None;
    }
    Some((low, high))
}

impl PriceScale {
    /// Build a mapping; refuses zero/negative geometry, non-finite base
    /// values, and non-positive log bounds.
    pub fn new(
        mode: PriceScaleMode,
        range: PriceRange,
        height: f32,
        bar_spacing: f32,
        base_value: f64,
        inverted: bool,
    ) -> Result<Self, ScaleError> {
        if !height.is_finite() || height <= 0.0 {
            return Err(ScaleError::InvalidParam {
                what: "height".to_owned(),
                reason: "must be a positive finite pixel count".to_owned(),
            });
        }
        if !bar_spacing.is_finite() || bar_spacing <= 0.0 {
            return Err(ScaleError::InvalidParam {
                what: "bar_spacing".to_owned(),
                reason: "must be a positive finite pixel count".to_owned(),
            });
        }
        if !base_value.is_finite() {
            return Err(ScaleError::InvalidParam {
                what: "base_value".to_owned(),
                reason: "must be finite".to_owned(),
            });
        }
        if mode == PriceScaleMode::Logarithmic && (range.min <= 0.0 || range.max <= 0.0) {
            return Err(ScaleError::InvalidParam {
                what: "range".to_owned(),
                reason: "logarithmic mode needs positive bounds".to_owned(),
            });
        }
        if (mode == PriceScaleMode::Percentage || mode == PriceScaleMode::IndexedTo100)
            && base_value == 0.0
        {
            return Err(ScaleError::InvalidParam {
                what: "base_value".to_owned(),
                reason: "percentage modes cannot rebase onto zero".to_owned(),
            });
        }
        // The scaled endpoints are fixed by the validated inputs, so they are
        // reshaped once here instead of per price: the paint loop maps a price
        // every bar every frame.
        let bounds = scaled_bounds(mode, base_value, range);
        Ok(Self {
            mode,
            range,
            height,
            bar_spacing,
            base_value,
            inverted,
            scaled_bounds: bounds,
        })
    }

    /// Active mode.
    #[must_use]
    pub fn mode(self) -> PriceScaleMode {
        self.mode
    }

    /// Visible price window.
    #[must_use]
    pub fn range(self) -> PriceRange {
        self.range
    }

    /// Pane height in pixels: the `0..=height` pixel band prices map onto.
    #[must_use]
    pub fn height(self) -> f32 {
        self.height
    }

    /// Pixels per bar on the index axis.
    #[must_use]
    pub fn bar_spacing(self) -> f32 {
        self.bar_spacing
    }

    /// First-visible reference the percentage/indexed modes rebase onto.
    #[must_use]
    pub fn base_value(self) -> f64 {
        self.base_value
    }

    /// Whether the y axis grows upward instead of downward.
    #[must_use]
    pub fn inverted(self) -> bool {
        self.inverted
    }

    /// Map a price to a y pixel (`0` at the top). `None` on non-finite or
    /// unmappable input.
    #[must_use]
    pub fn price_to_y(self, price: f64) -> Option<f32> {
        let (low, high) = self.scaled_bounds?;
        let scaled = to_scaled(self.mode, self.base_value, price)?;
        let height = f64::from(self.height);
        let ratio = (high - scaled) / (high - low);
        let y = ratio * height;
        let y = if self.inverted { height - y } else { y };
        if y.is_finite() {
            Some(y as f32)
        } else {
            None
        }
    }

    /// Invert [`price_to_y`](Self::price_to_y). `None` off-pane.
    ///
    /// The pane is exactly `0..=height`; a pixel outside it is off-pane and
    /// has no price. [`price_to_y`](Self::price_to_y) still extrapolates for
    /// the paint edge (lines and candles running off the pane), so the
    /// round-trip only holds for in-pane prices.
    #[must_use]
    pub fn y_to_price(self, y: f32) -> Option<f64> {
        if !y.is_finite() {
            return None;
        }
        if y < 0.0 || y > self.height {
            return None;
        }
        let (low, high) = self.scaled_bounds?;
        let height = f64::from(self.height);
        let y = f64::from(y);
        let y = if self.inverted { height - y } else { y };
        let scaled = high - y / height * (high - low);
        unscale(self.mode, self.base_value, scaled)
    }

    /// Map a bar index to an x pixel. Total over the visible window.
    #[must_use]
    pub fn index_to_x(self, index: usize) -> f32 {
        index as f32 * self.bar_spacing
    }

    /// Invert [`index_to_x`](Self::index_to_x); fractions name intra-bar
    /// positions. `None` on non-finite input.
    #[must_use]
    pub fn x_to_index(self, x: f32) -> Option<f64> {
        if x.is_finite() {
            Some(f64::from(x) / f64::from(self.bar_spacing))
        } else {
            None
        }
    }
}

#[cfg(test)]
#[path = "../tests/mapping.rs"]
mod tests;
