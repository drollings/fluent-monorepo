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
use fluent_wvr::validate::{check_for_execute, valid_output, Validate};
use fluent_wvr::{Describable, FieldAccess};

use crate::error::EguiError;
use crate::paint::parse_hex_color;

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

fn default_rsi() -> String {
    "#20C8F0".to_owned()
}

fn default_cci() -> String {
    "#B4F028".to_owned()
}

fn default_cmo() -> String {
    "#8CF050".to_owned()
}

fn default_mfi() -> String {
    "#F020F0".to_owned()
}

fn default_stoch() -> String {
    "#B4F0C8".to_owned()
}

fn default_aroon() -> String {
    "#FAF028".to_owned()
}

fn default_adx() -> String {
    "#20C8F0".to_owned()
}

fn default_grid() -> String {
    "#2A2A2A".to_owned()
}

/// One pane's layout entry: visibility plus its share of chart height.
#[derive(
    Debug, Clone, bon::Builder, serde::Serialize, serde::Deserialize, FieldAccess, Describable,
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
    Debug, Clone, bon::Builder, serde::Serialize, serde::Deserialize, FieldAccess, Describable,
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
    /// RSI oscillator color: the shipped oscillator hue.
    #[field(desc = "RSI oscillator color (#RRGGBB)")]
    #[builder(default = default_rsi())]
    #[serde(default = "default_rsi")]
    pub rsi: String,
    /// CCI oscillator color: the shipped oscillator hue.
    #[field(desc = "CCI oscillator color (#RRGGBB)")]
    #[builder(default = default_cci())]
    #[serde(default = "default_cci")]
    pub cci: String,
    /// CMO oscillator color: the shipped oscillator hue.
    #[field(desc = "CMO oscillator color (#RRGGBB)")]
    #[builder(default = default_cmo())]
    #[serde(default = "default_cmo")]
    pub cmo: String,
    /// MFI oscillator color: the shipped oscillator hue.
    #[field(desc = "MFI oscillator color (#RRGGBB)")]
    #[builder(default = default_mfi())]
    #[serde(default = "default_mfi")]
    pub mfi: String,
    /// Stochastic oscillator color: the shipped oscillator hue.
    #[field(desc = "Stochastic oscillator color (#RRGGBB)")]
    #[builder(default = default_stoch())]
    #[serde(default = "default_stoch")]
    pub stoch: String,
    /// Aroon oscillator color: the shipped oscillator hue.
    #[field(desc = "Aroon oscillator color (#RRGGBB)")]
    #[builder(default = default_aroon())]
    #[serde(default = "default_aroon")]
    pub aroon: String,
    /// ADX oscillator color. The shipped table names no ADX hue, so the
    /// key defaults to the current effective hue and changes nothing.
    #[field(desc = "ADX oscillator color (#RRGGBB)")]
    #[builder(default = default_adx())]
    #[serde(default = "default_adx")]
    pub adx: String,
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

impl ThemeConfig {
    /// The identity field: the theme key's own name, not a color.
    const IDENTITY_FIELD: &'static str = "name";

    /// Every series style key this theme resolves, in the derive's own field
    /// order, minus the identity field.
    ///
    /// Derived from [`FieldAccess::field_names`] — the same declarative field
    /// list the `Describable` schema is projected from — so a picker entry, the
    /// resolver, and the settings pane cannot drift apart: a new color field is
    /// a key the moment it is added to the struct.
    #[must_use]
    pub fn style_keys() -> &'static [&'static str] {
        static KEYS: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
        KEYS.get_or_init(|| {
            ThemeConfig::default()
                .field_names()
                .iter()
                .copied()
                .filter(|name| *name != Self::IDENTITY_FIELD)
                .collect()
        })
    }

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
            ("rsi", &self.rsi),
            ("cci", &self.cci),
            ("cmo", &self.cmo),
            ("mfi", &self.mfi),
            ("stoch", &self.stoch),
            ("aroon", &self.aroon),
            ("adx", &self.adx),
            ("grid", &self.grid),
        ] {
            // One hex reader: the same parse the painter uses, so a color
            // that validates is a color that paints.
            parse_hex_color(what, value)?;
        }
        Ok(())
    }

    /// Resolve a series style key to its color; `None` for unknown keys.
    ///
    /// A key from [`style_keys`](Self::style_keys) always resolves: the
    /// resolver and the enumerator read the same field list (the test pins it).
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
            "rsi" => Some(&self.rsi),
            "cci" => Some(&self.cci),
            "cmo" => Some(&self.cmo),
            "mfi" => Some(&self.mfi),
            "stoch" => Some(&self.stoch),
            "aroon" => Some(&self.aroon),
            "adx" => Some(&self.adx),
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
