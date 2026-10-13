//! Quiet regime labels for display and gate consumers.
//!
//! The frozen detector ([`parity_label`]) stays the exact-`==` parity probe:
//! it answers what the market read says right now, flicker included. Display
//! panes and gates read through [`display_label`] instead, which routes the
//! same surfaces through the hysteresis wrapper so chop inside the band
//! holds instead of flickering. The wrapper output feeds the existing theme
//! style keys via [`regime_style_key`] — no new label type exists on this
//! path, and [`MarketRegime`] is the kernel's own enum, reused, never
//! shadowed.
//!
//! Label quietness is display task-value (readability for humans and gates),
//! never regime confidence: a held label can still be wrong about the
//! market. Nothing here caches or persists a label on its own; the plan
//! cache owns persistence, and it stores what this module returned.
//!
//! [`parity_label`]: parity_label
//! [`display_label`]: display_label

use protopyne_kernels::market_context::{
    MarketRegime, REGIME_HYSTERESIS, detect_regime, detect_regime_stable,
};

use fluent_egui::EguiError;

/// Re-exported stability band so panes can name the hold margin without a
/// second constant living here.
pub use protopyne_kernels::market_context::REGIME_HYSTERESIS as STABILITY_BAND;

/// Frozen detector read: exact-`==` parity probe over the three normalized
/// surfaces. Flicker included, hysteresis excluded. Probe-only: display and
/// gate paths must call [`display_label`] instead.
#[must_use]
pub fn parity_label(
    ma_health: Option<f64>,
    volatility_ratio: Option<f64>,
    vol_ratio: Option<f64>,
) -> MarketRegime {
    detect_regime(ma_health, volatility_ratio, vol_ratio)
}

/// Quiet display/gate read: the frozen detector decides the fresh label,
/// the hysteresis wrapper decides whether the switch earns the flicker.
/// Cross-surface switches need the fresh surface to beat the held surface
/// by more than [`STABILITY_BAND`]; same-surface sign flips switch at once;
/// vanished evidence never holds a stale label.
#[must_use]
pub fn display_label(
    prev: MarketRegime,
    ma_health: Option<f64>,
    volatility_ratio: Option<f64>,
    vol_ratio: Option<f64>,
) -> MarketRegime {
    detect_regime_stable(prev, ma_health, volatility_ratio, vol_ratio)
}

/// Existing theme style key for one regime label. Every arm resolves
/// against `ThemeConfig :: style` today (`bull`/`bear`/`fast`/`slow`/`grid`),
/// so regime color follows the single theme truth instead of a second
/// palette or label type.
#[must_use]
pub fn regime_style_key(label: MarketRegime) -> &'static str {
    match label {
        MarketRegime::TrendingBull => "bull",
        MarketRegime::TrendingBear => "bear",
        MarketRegime::Volatile => "fast",
        MarketRegime::Ranging => "slow",
        MarketRegime::LowVolume => "grid",
    }
}

/// Held display label across bars: constructed with the label on screen,
/// advanced bar-by-bar through [`display_label`]. Pure state, no caching —
/// persistence stays in the plan cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuietLabel {
    /// Label currently on screen.
    current: MarketRegime,
}

impl QuietLabel {
    /// Hold `initial` on screen before any bar arrives.
    #[must_use]
    pub fn new(initial: MarketRegime) -> Self {
        Self { current: initial }
    }

    /// Label currently on screen.
    #[must_use]
    pub fn current(&self) -> MarketRegime {
        self.current
    }

    /// Advance one bar through the hysteresis wrapper, returning the label
    /// now on screen.
    pub fn update(
        &mut self,
        ma_health: Option<f64>,
        volatility_ratio: Option<f64>,
        vol_ratio: Option<f64>,
    ) -> MarketRegime {
        self.current = display_label(self.current, ma_health, volatility_ratio, vol_ratio);
        self.current
    }

    /// Replace the held label (toolbar reset, span switch). The next
    /// [`QuietLabel::update`] still routes through the wrapper from here.
    pub fn reset(&mut self, label: MarketRegime) {
        self.current = label;
    }
}

/// Fold one quiet label track over parallel surface slices.
///
/// Length-preserving: one label per bar, each routed through
/// [`display_label`] from the previous screen label. Absent surfaces flow
/// through the frozen detector (vanished evidence releases to quiet, never
/// holds stale). Refuses slice-length mismatch with a reason.
pub fn label_series(
    initial: MarketRegime,
    ma_health: &[Option<f64>],
    volatility_ratio: &[Option<f64>],
    vol_ratio: &[Option<f64>],
) -> Result<Vec<MarketRegime>, EguiError> {
    if ma_health.len() != volatility_ratio.len() || ma_health.len() != vol_ratio.len() {
        return Err(EguiError::InvalidParam {
            what: "series".to_owned(),
            reason: "regime surface slices must share one length".to_owned(),
        });
    }
    let mut held = QuietLabel::new(initial);
    let mut out = Vec::with_capacity(ma_health.len());
    for index in 0..ma_health.len() {
        out.push(held.update(ma_health[index], volatility_ratio[index], vol_ratio[index]));
    }
    Ok(out)
}

/// Confirm the module routes through the shared band constant rather than a
/// local copy (keeps the one-truth rule compiler-checked at the use site).
#[must_use]
pub fn stability_band() -> f64 {
    REGIME_HYSTERESIS
}

#[cfg(test)]
#[path = "../tests/regime_labels.rs"]
mod tests;
