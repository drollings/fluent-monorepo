//! Generic control plane: pane layout, theme, schema, overrides.
//!
//! The struct is the single source of truth: [`PaneConfig`] and
//! [`ThemeConfig`] derive the accessor, validator, and schema from one
//! definition (no hand-written builders, no second settings schema),
//! validate fail-closed through [`Validate`], and execute through the
//! [`check_for_execute`] bridge. Runtime user overrides ride
//! [`ComponentAdapter`] field overrides — they change the render config,
//! never upstream parameters. Domain configs (chart selection, replay
//! cursor wiring) compose these shapes in the terminal crate.

use fluent_wvr::prelude::*;
use fluent_wvr::{Describable, FieldAccess};
use protopyne_types::{Validate, check_for_execute, valid_output};

use crate::error::EguiError;

fn default_theme() -> String {
    "default".to_owned()
}

fn default_bull() -> String {
    "#20B46C".to_owned()
}

fn default_bear() -> String {
    "#B42020".to_owned()
}

fn default_fast() -> String {
    "#20C8F0".to_owned()
}

fn default_slow() -> String {
    "#B4F028".to_owned()
}

fn default_background() -> String {
    "#000000".to_owned()
}

fn default_ribbon_bull() -> String {
    "#00BCD4".to_owned()
}

fn default_ribbon_bear() -> String {
    "#7B1FA2".to_owned()
}

fn default_bull_down() -> String {
    "#FF9800".to_owned()
}

fn default_bear_up() -> String {
    "#2196F3".to_owned()
}

fn default_grid() -> String {
    "#2A2A2A".to_owned()
}

/// One pane's layout entry: visibility plus its share of chart height.
#[derive(
    Debug, Clone, bon::Builder, serde::Serialize, serde::Deserialize, FieldAccess, Describable
)]
#[builder(start_fn = new)]
pub struct PaneConfig {
    /// Pane key (for example "price", "volume", "oscillator").
    #[field(desc = "Pane key", max_len = 32)]
    pub name: String,
    /// Whether this pane paints.
    #[field(desc = "Whether this pane paints")]
    #[builder(default = true)]
    pub visible: bool,
    /// Share of chart height in `(0, 1]`.
    #[field(desc = "Share of chart height")]
    #[builder(default = 1.0)]
    pub height_share: f64,
}

impl PaneConfig {
    /// Fail-closed self-consistency check.
    pub fn validate(&self) -> Result<(), EguiError> {
        if self.name.trim().is_empty() {
            return Err(EguiError::InvalidParam {
                what: "name".to_owned(),
                reason: "pane name must not be empty".to_owned(),
            });
        }
        if !self.height_share.is_finite() || self.height_share <= 0.0 || self.height_share > 1.0 {
            return Err(EguiError::InvalidParam {
                what: "height_share".to_owned(),
                reason: "height share must sit within (0, 1]".to_owned(),
            });
        }
        Ok(())
    }
}

impl Validate for PaneConfig {
    type Err = EguiError;
    fn validate(&self) -> Result<(), Self::Err> {
        PaneConfig::validate(self)
    }
}

impl WorkUnit for PaneConfig {
    fn name(&self) -> &str {
        "pane_config"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        check_for_execute(self, "pane config invalid")?;
        Ok(valid_output(format!("pane ready ({})", self.name)))
    }
}

impl_component!(PaneConfig);

/// Named theme: style keys resolved to `#RRGGBB` colors at paint time.
///
/// Slots are named for their role (`bull`, `bear`, `fast`, `slow`), never
/// for a kernel — the paint edge resolves a series' style key here instead
/// of carrying color literals in compute.
#[derive(
    Debug, Clone, bon::Builder, serde::Serialize, serde::Deserialize, FieldAccess, Describable
)]
#[builder(start_fn = new)]
pub struct ThemeConfig {
    /// Theme key selected by the chart config.
    #[field(desc = "Theme key", max_len = 32)]
    #[builder(default = default_theme())]
    #[serde(default = "default_theme")]
    pub name: String,
    /// Bull style color.
    #[field(desc = "Bull style color (#RRGGBB)")]
    #[builder(default = default_bull())]
    #[serde(default = "default_bull")]
    pub bull: String,
    /// Bear style color.
    #[field(desc = "Bear style color (#RRGGBB)")]
    #[builder(default = default_bear())]
    #[serde(default = "default_bear")]
    pub bear: String,
    /// Fast-series style color.
    #[field(desc = "Fast-series style color (#RRGGBB)")]
    #[builder(default = default_fast())]
    #[serde(default = "default_fast")]
    pub fast: String,
    /// Slow-series style color.
    #[field(desc = "Slow-series style color (#RRGGBB)")]
    #[builder(default = default_slow())]
    #[serde(default = "default_slow")]
    pub slow: String,
    /// Chart background color.
    #[field(desc = "Chart background color (#RRGGBB)")]
    #[builder(default = default_background())]
    #[serde(default = "default_background")]
    pub background: String,
    /// Ribbon fill color on bull runs (shaded band, not candles).
    #[field(desc = "Ribbon bull fill color (#RRGGBB)")]
    #[builder(default = default_ribbon_bull())]
    #[serde(default = "default_ribbon_bull")]
    pub ribbon_bull: String,
    /// Ribbon fill color on bear runs (shaded band, not candles).
    #[field(desc = "Ribbon bear fill color (#RRGGBB)")]
    #[builder(default = default_ribbon_bear())]
    #[serde(default = "default_ribbon_bear")]
    pub ribbon_bear: String,
    /// Candle color for bull-trend bars that closed down (retrace).
    #[field(desc = "Bull retrace candle color (#RRGGBB)")]
    #[builder(default = default_bull_down())]
    #[serde(default = "default_bull_down")]
    pub bull_down: String,
    /// Candle color for bear-trend bars that closed up (relief).
    #[field(desc = "Bear relief candle color (#RRGGBB)")]
    #[builder(default = default_bear_up())]
    #[serde(default = "default_bear_up")]
    pub bear_up: String,
    /// Gridline color.
    #[field(desc = "Gridline color (#RRGGBB)")]
    #[builder(default = default_grid())]
    #[serde(default = "default_grid")]
    pub grid: String,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self::new().build()
    }
}

/// Whether `value` spells a `#RRGGBB` color.
fn is_hex_color(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 7 && bytes[0] == b'#' && bytes[1..].iter().all(u8::is_ascii_hexdigit)
}

impl ThemeConfig {
    /// Fail-closed self-consistency check.
    pub fn validate(&self) -> Result<(), EguiError> {
        if self.name.trim().is_empty() {
            return Err(EguiError::InvalidParam {
                what: "name".to_owned(),
                reason: "theme name must not be empty".to_owned(),
            });
        }
        for (what, value) in [
            ("bull", &self.bull),
            ("bear", &self.bear),
            ("fast", &self.fast),
            ("slow", &self.slow),
            ("background", &self.background),
            ("ribbon_bull", &self.ribbon_bull),
            ("ribbon_bear", &self.ribbon_bear),
            ("bull_down", &self.bull_down),
            ("bear_up", &self.bear_up),
            ("grid", &self.grid),
        ] {
            if !is_hex_color(value) {
                return Err(EguiError::InvalidParam {
                    what: what.to_owned(),
                    reason: format!("{value:?} is not a #RRGGBB color"),
                });
            }
        }
        Ok(())
    }

    /// Resolve a series style key to its color; `None` for unknown keys.
    #[must_use]
    pub fn style(&self, key: &str) -> Option<&str> {
        match key {
            "bull" => Some(&self.bull),
            "bear" => Some(&self.bear),
            "fast" => Some(&self.fast),
            "slow" => Some(&self.slow),
            "background" => Some(&self.background),
            "ribbon_bull" => Some(&self.ribbon_bull),
            "ribbon_bear" => Some(&self.ribbon_bear),
            "bull_down" => Some(&self.bull_down),
            "bear_up" => Some(&self.bear_up),
            "grid" => Some(&self.grid),
            _ => None,
        }
    }
}

impl Validate for ThemeConfig {
    type Err = EguiError;
    fn validate(&self) -> Result<(), Self::Err> {
        ThemeConfig::validate(self)
    }
}

impl WorkUnit for ThemeConfig {
    fn name(&self) -> &str {
        "theme_config"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        check_for_execute(self, "theme config invalid")?;
        Ok(valid_output(format!("theme ready ({})", self.name)))
    }
}

impl_component!(ThemeConfig);

#[cfg(test)]
#[path = "../tests/config.rs"]
mod tests;
