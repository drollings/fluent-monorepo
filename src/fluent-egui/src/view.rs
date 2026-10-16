//! Headless pane view-models over converted chart rows.
//!
//! Pure display preparation with no paint dependency: every function here is
//! `f64`-in/`f64`-out over [`protopyne_chart_model::ChartRow`] slices or plain
//! option series. Exact-decimal money never appears; bars arrive already
//! converted (`rows_of`/`row_of`), and non-finite display points become holes
//! (`None`) instead of substituted values, following the conversion skip
//! policy. Warmup holes are preserved element-wise, so every output has the
//! same length as its input.
//!
//! Trading-domain series math (oscillator identity, frozen squash tables)
//! lives in the terminal crate over these shapes; colors never appear here:
//! series carry a theme style key resolved against the theme config at paint
//! time.

use protopyne_chart_model::ChartRow;

use crate::error::EguiError;

/// Which display pane a series belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaneKind {
    /// Main candle pane (prices, ribbon).
    Price,
    /// Zero-based volume/delta strip.
    Volume,
    /// Zero-centered oscillator pane.
    Oscillator,
}

/// A named display series with warmup holes preserved.
///
/// `values` uses `None` for warmup/unavailable bars; every constructor in
/// this module preserves input length element-wise, so a series lines up
/// with its source rows index-for-index. `style` is a theme key (for example
/// `"bull"`, `"bear"`, `"fast"`, `"slow"`), never a color literal.
#[derive(Debug, Clone, PartialEq)]
pub struct PaneSeries {
    /// Series name shown in legends and tables.
    pub name: String,
    /// Pane this series paints in.
    pub pane: PaneKind,
    /// Per-bar display values with holes preserved.
    pub values: Vec<Option<f64>>,
    /// Theme style key resolved at paint time.
    pub style: String,
}

impl PaneSeries {
    /// Build a series; refuses empty names or style keys with reasons.
    pub fn new(
        name: impl Into<String>,
        pane: PaneKind,
        values: Vec<Option<f64>>,
        style: impl Into<String>,
    ) -> Result<Self, EguiError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(EguiError::InvalidParam {
                what: "name".to_owned(),
                reason: "series name must not be empty".to_owned(),
            });
        }
        let style = style.into();
        if style.trim().is_empty() {
            return Err(EguiError::InvalidParam {
                what: "style".to_owned(),
                reason: "style key must not be empty".to_owned(),
            });
        }
        Ok(Self { name, pane, values, style })
    }

    /// Number of per-bar slots (holes included).
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether the series holds no slots.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// Ribbon band between two smoothing legs.
#[derive(Debug, Clone, PartialEq)]
pub struct RibbonView {
    /// Per-bar upper edge (`max` of the pair).
    pub upper: Vec<Option<f64>>,
    /// Per-bar lower edge (`min` of the pair).
    pub lower: Vec<Option<f64>>,
    /// Per-bar trend key: true while adaptive sits above signal.
    ///
    /// Meaningful only where `upper`/`lower` are both `Some`; holes carry
    /// false and are skipped at paint time.
    pub bullish: Vec<bool>,
    /// Fill strength carried through for the theme fill.
    pub alpha: f64,
}

/// Build the ribbon band from a fast/slow smoothing pair plus fill strength.
///
/// Refuses mismatched lengths and `alpha` outside `[0, 1]`; holes in either
/// leg produce holes in both edges. `upper` is always `>=` `lower` wherever
/// both are present.
pub fn ribbon_view(
    fast: &[Option<f64>],
    slow: &[Option<f64>],
    alpha: f64,
) -> Result<RibbonView, EguiError> {
    if fast.len() != slow.len() {
        return Err(EguiError::InvalidParam {
            what: "series".to_owned(),
            reason: "fast and slow lengths differ".to_owned(),
        });
    }
    if !alpha.is_finite() || alpha < 0.0 || alpha > 1.0 {
        return Err(EguiError::InvalidParam {
            what: "alpha".to_owned(),
            reason: "fill strength must be within [0, 1]".to_owned(),
        });
    }
    let mut upper = Vec::with_capacity(fast.len());
    let mut lower = Vec::with_capacity(fast.len());
    let mut bullish = Vec::with_capacity(fast.len());
    for (quick, sluggish) in fast.iter().zip(slow.iter()) {
        match (quick, sluggish) {
            (Some(a), Some(s)) if a.is_finite() && s.is_finite() => {
                upper.push(Some(a.max(*s)));
                lower.push(Some(a.min(*s)));
                bullish.push(a > s);
            }
            _ => {
                upper.push(None);
                lower.push(None);
                bullish.push(false);
            }
        }
    }
    Ok(RibbonView { upper, lower, bullish, alpha })
}

/// Theme style key for one ribbon bar; resolved against the theme at paint.
#[must_use]
pub fn ribbon_style_key(bullish: bool) -> &'static str {
    if bullish { "bull" } else { "bear" }
}

/// Theme style key for one ribbon *fill* run.
///
/// The shaded band resolves to the dedicated `ribbon_bull`/`ribbon_bear`
/// slots (teal/purple), never the candle `bull`/`bear` slots — a filled band
/// and a candle body are different chart shapes with their own theme colors.
/// [`ribbon_style_key`] keeps candle trend polarity.
#[must_use]
pub fn ribbon_fill_style_key(bullish: bool) -> &'static str {
    if bullish { "ribbon_bull" } else { "ribbon_bear" }
}

/// Zero-based delta volume bars.
#[derive(Debug, Clone, PartialEq)]
pub struct DeltaView {
    /// Buy-side tops (`0 → buy_top`).
    pub buy_top: Vec<Option<f64>>,
    /// Sell-side bottoms (`-sell_bottom → 0`, stored as non-negative depths).
    pub sell_bottom: Vec<Option<f64>>,
}

/// Build zero-based delta bars from converted rows plus split volumes.
///
/// The split (buy/sell share of each bar) arrives precomputed upstream; this
/// function only normalizes to display height around zero, mirroring the
/// reference candle scaling: bar `i` scales by the causal peak of total
/// `volume` seen so far (`buy[i] / peak * unit`), so replaying a prefix
/// reproduces the same heights. Rows supply the peak; `buy`/`sell` supply
/// the split. Holes stay holes; a zero peak yields zero-height bars.
///
/// Refuses length mismatch, non-finite or non-positive `unit`, and negative
/// split volumes (volumes are non-negative by construction).
pub fn delta_candle_view(
    rows: &[ChartRow],
    buy: &[Option<f64>],
    sell: &[Option<f64>],
    unit: f64,
) -> Result<DeltaView, EguiError> {
    if rows.len() != buy.len() || rows.len() != sell.len() {
        return Err(EguiError::InvalidParam {
            what: "series".to_owned(),
            reason: "rows, buy, and sell lengths must match".to_owned(),
        });
    }
    if !unit.is_finite() || unit <= 0.0 {
        return Err(EguiError::InvalidParam {
            what: "unit".to_owned(),
            reason: "height at average volume must be positive and finite".to_owned(),
        });
    }
    let mut buy_top = Vec::with_capacity(rows.len());
    let mut sell_bottom = Vec::with_capacity(rows.len());
    let mut peak = 0.0_f64;
    for (index, row) in rows.iter().enumerate() {
        let total = row.volume as f64;
        if total > peak {
            peak = total;
        }
        let scale = if peak > 0.0 { unit / peak } else { 0.0 };
        buy_top.push(match buy[index] {
            None => None,
            Some(b) if !b.is_finite() => None,
            Some(b) if b < 0.0 => {
                return Err(EguiError::InvalidParam {
                    what: "volume".to_owned(),
                    reason: format!("buy split at index {index} is negative"),
                });
            }
            Some(b) => Some(b * scale),
        });
        sell_bottom.push(match sell[index] {
            None => None,
            Some(s) if !s.is_finite() => None,
            Some(s) if s < 0.0 => {
                return Err(EguiError::InvalidParam {
                    what: "volume".to_owned(),
                    reason: format!("sell split at index {index} is negative"),
                });
            }
            Some(s) => Some(s * scale),
        });
    }
    Ok(DeltaView { buy_top, sell_bottom })
}

/// Which threshold crossing a pivot mark decorates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PivotSide {
    /// Fast oscillator fell below `+threshold` (bear dot).
    High,
    /// Fast oscillator rose above `-threshold` (bull dot).
    Low,
}

/// One threshold crossing on the fast oscillator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PivotMark {
    /// Bar index of the confirmed crossing bar.
    pub index: usize,
    /// Which side crossed.
    pub side: PivotSide,
}

/// Mark threshold crossings of the fast oscillator series.
///
/// Mirrors the confirmed-bar crossing branches: a `High` mark fires when the
/// previous bar sat at/above `+threshold` and the current bar dropped below
/// it; a `Low` mark fires when the previous bar sat at/below `-threshold`
/// and the current bar rose above it. Pairs touching a hole or a non-finite
/// reading never mark. Refuses non-finite or non-positive thresholds.
pub fn pivot_marks(fast: &[Option<f64>], threshold: f64) -> Result<Vec<PivotMark>, EguiError> {
    if !threshold.is_finite() || threshold <= 0.0 {
        return Err(EguiError::InvalidParam {
            what: "threshold".to_owned(),
            reason: "pivot threshold must be positive and finite".to_owned(),
        });
    }
    let mut marks = Vec::new();
    for index in 1..fast.len() {
        let (Some(previous), Some(current)) = (fast[index - 1], fast[index]) else {
            continue;
        };
        if !previous.is_finite() || !current.is_finite() {
            continue;
        }
        if previous >= threshold && current < threshold {
            marks.push(PivotMark { index, side: PivotSide::High });
        } else if previous <= -threshold && current > -threshold {
            marks.push(PivotMark { index, side: PivotSide::Low });
        }
    }
    Ok(marks)
}

/// Unit sigmoid both band mappers share: `x / (1 + |x|)`, strictly
/// monotonic from `(-∞, +∞)` onto `(-1, +1)`, odd, with `0 → 0`. Pure
/// display math — no series identity, no calibration; the domain crates
/// own which constants flow through it.
#[must_use]
pub fn unit_squash(x: f64) -> f64 {
    x / (1.0 + x.abs())
}

/// Map one reading through an affine input shift plus a scaled sigmoid.
///
/// Computes `unit_squash((value + offset) * slope) * scale` in exactly that
/// op order: shift, scale, squash, then widen to the display band. The
/// per-series calibration (`offset`, `slope`, `scale`) stays with the
/// caller — this is the shape every calibrated band shares, not any one
/// band's constants.
#[must_use]
pub fn affine_squash(value: f64, offset: f64, slope: f64, scale: f64) -> f64 {
    unit_squash((value + offset) * slope) * scale
}

/// Compress one reading onto a symmetric band through the shared sigmoid.
///
/// Computes `unit_squash(value / divisor) * scale`: `0 → 0`, `±divisor →`
/// `±scale / 2`, finite inputs stay strictly inside `±scale`. Non-finite
/// inputs yield `NaN` (series callers turn that into a hole via
/// [`map_series_optional`]).
#[must_use]
pub fn band_squash(value: f64, divisor: f64, scale: f64) -> f64 {
    unit_squash(value / divisor) * scale
}

/// Lift a scalar map over an option series, preserving holes.
///
/// Length-preserving: output has one slot per input slot, `None` stays
/// `None`, and a non-finite mapped value becomes a hole rather than a
/// substituted value. The refusal (not repair) direction is load-bearing:
/// every series built here lines up index-for-index with its source rows,
/// so inventing a value would misalign the pane.
pub fn map_series_optional(series: &[Option<f64>], f: impl Fn(f64) -> f64) -> Vec<Option<f64>> {
    series.iter().map(|slot| slot.map(&f).filter(|mapped| mapped.is_finite())).collect()
}

#[cfg(test)]
#[path = "../tests/view.rs"]
mod tests;
