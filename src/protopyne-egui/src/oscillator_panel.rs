//! Oscillator kind defaults: the dock panel's per-kind inputs.
//!
//! One [`KindDefaults`] per bounded oscillator kind (period, smoothing,
//! gain — plain input values in the shipped script's spirit, never source
//! bindings), held in a single workspace-owned [`OscillatorPanelState`].
//! Slots with unset dials follow these kind defaults; explicit slot values
//! win; the frozen catalog remains the last resort. Color lives in the
//! theme (the panel edits the kind theme key), so paint needs no new
//! lookup.

use fluent_wvr::prelude::*;
use fluent_wvr::validate::{check_for_execute, valid_output, Validate};
use fluent_wvr::{Describable, FieldAccess};
use protopyne_kernels::smoothing::{SmoothingFamily, MA_PERIOD_MAX, MA_PERIOD_MIN};

use fluent_egui::EguiError;

/// Panel kinds, in dock order: normalized oscillators first, then the
/// bounded unmapped legs. Kinds without kernels (`cdv`, DMI legs) and
/// `voss` (pending its scale check) have no entry.
pub const PANEL_KINDS: [&str; 7] = ["rsi", "cci", "cmo", "mfi", "stoch", "aroon", "adx"];

/// The panel kind a kernel follows, if any: the same mapping that seeds
/// kind line keys, reused so resolution and styling never disagree on
/// which kind a kernel belongs to.
#[must_use]
pub fn panel_kind_for_kernel(kernel: &str) -> Option<&'static str> {
    match kernel {
        "rsi" => Some("rsi"),
        "cci" => Some("cci"),
        "cmo" => Some("cmo"),
        "mfi" => Some("mfi"),
        "stoch" | "stochf" => Some("stoch"),
        "aroon" => Some("aroon"),
        "adx" => Some("adx"),
        _ => None,
    }
}

/// Kinds with no shapeable catalog row: their panel rows hide gain and the
/// triple (period, smoothing, and color still apply to the raw leg and its
/// paint). Resolution never fills these dials, so the placeholders below
/// stay inert. Only `adx` remains: aroon maps and shapes since the rescale
/// proof.
pub const PANEL_UNSHAPED_KINDS: [&str; 1] = ["adx"];

/// Whether the dock hides one panel dial for a kind: unshaped kinds show
/// period, smoothing, and color only, never gain or triple dials that
/// cannot move their picture.
#[must_use]
pub fn hides_dial(kind: &str, field: &str) -> bool {
    PANEL_UNSHAPED_KINDS.contains(&kind) && matches!(field, "gain" | "offset" | "k" | "mult")
}

/// Whether a kernel's slots follow the panel triple: only kinds with a
/// catalog row to shape. Unmapped kernels keep their blanks, so the
/// unshaped rows' placeholders never fabricate a shape.
#[must_use]
pub fn panel_kind_is_shaped(kernel: &str) -> bool {
    matches!(
        panel_kind_for_kernel(kernel),
        Some(kind) if !PANEL_UNSHAPED_KINDS.contains(&kind)
    )
}

/// Leg kinds the IntegrationLite arms may select: normalized oscillators
/// with kernels, so the raw leg and the catalog row both exist.
pub const LITE_LEG_KINDS: [&str; 5] = ["rsi", "cci", "cmo", "mfi", "stoch"];

/// Dropdown options for the Lite leg-kind selectors, in panel order.
#[must_use]
pub fn lite_leg_kind_ids() -> Vec<String> {
    LITE_LEG_KINDS
        .iter()
        .map(|kind| (*kind).to_owned())
        .collect()
}

fn default_kind_period() -> i64 {
    14
}

fn default_kind_smooth_type() -> String {
    "na".to_owned()
}

fn default_kind_smooth_period() -> i64 {
    3
}

fn default_kind_gain() -> f64 {
    1.0
}

/// One oscillator kind's defaults: the panel's input row for that kind.
///
/// Bounds mirror the MA period contract (the same 2..200 window every
/// length dial shares); smoothing follows the leg-smoothing rules (only
/// length-taking families arm); gain stays positive and finite; the triple
/// follows the slot triple rules (finite offset, positive steepness and
/// band) and stays blankable so legacy documents fall back to the catalog.
/// All bounds apply unconditionally, like the script's own input
/// constraints.
#[derive(
    Debug,
    Clone,
    PartialEq,
    bon::Builder,
    serde::Serialize,
    serde::Deserialize,
    FieldAccess,
    Describable,
)]
#[builder(start_fn = new)]
pub struct KindDefaults {
    /// Default length in bars for new slots of this kind.
    #[field(desc = "Oscillator length in bars", min = 2, max = 200)]
    #[builder(default = default_kind_period())]
    pub period: i64,
    /// Display-leg smoothing family for this kind (`NA` disables).
    #[field(
        desc = "Display-leg smoothing",
        variants = "protopyne_kernels::smoothing::length_smoothing_ids"
    )]
    #[builder(default = default_kind_smooth_type())]
    pub smooth_type: String,
    /// Display-leg smoothing window in bars.
    #[field(desc = "Smoothing window in bars", min = 2, max = 200)]
    #[builder(default = default_kind_smooth_period())]
    pub smooth_period: i64,
    /// Display gain over the catalog squash for this kind.
    #[field(desc = "Display gain over the catalog squash", min = 0.0)]
    #[builder(default = default_kind_gain())]
    pub gain: f64,
    /// Display offset over the catalog row for this kind; blank follows
    /// the catalog (and legacy documents without triple columns read as
    /// blank, so upgrades never move the picture).
    #[field(desc = "Display offset over the catalog row (blank follows the catalog)")]
    #[serde(default)]
    pub offset: Option<f64>,
    /// Display steepness over the catalog row for this kind; blank follows
    /// the catalog.
    #[field(
        desc = "Display steepness over the catalog row, positive (blank follows the catalog)",
        min = 0.0
    )]
    #[serde(default)]
    pub k: Option<f64>,
    /// Display band half-width over the catalog row for this kind; blank
    /// follows the catalog.
    #[field(
        desc = "Display band half-width over the catalog row, positive (blank follows the catalog)",
        min = 0.0
    )]
    #[serde(default)]
    pub mult: Option<f64>,
}

impl KindDefaults {
    /// Fail-closed self-consistency check.
    pub fn validate(&self) -> Result<(), EguiError> {
        if !(MA_PERIOD_MIN..=MA_PERIOD_MAX).contains(&self.period) {
            return Err(EguiError::InvalidParam {
                what: "period".to_owned(),
                reason: format!(
                    "oscillator period must sit within [{MA_PERIOD_MIN}, {MA_PERIOD_MAX}], got {}",
                    self.period
                ),
            });
        }
        match SmoothingFamily::parse(&self.smooth_type) {
            Some(SmoothingFamily::Na) => {}
            Some(family) if family.takes_length() => {}
            _ => {
                return Err(EguiError::InvalidParam {
                    what: "smooth_type".to_owned(),
                    reason: format!(
                        "smoothing {:?} is not a length-taking smoothing family",
                        self.smooth_type
                    ),
                });
            }
        }
        if !(MA_PERIOD_MIN..=MA_PERIOD_MAX).contains(&self.smooth_period) {
            return Err(EguiError::InvalidParam {
                what: "smooth_period".to_owned(),
                reason: format!(
                    "smoothing window must sit within [{MA_PERIOD_MIN}, {MA_PERIOD_MAX}], got {}",
                    self.smooth_period
                ),
            });
        }
        if !self.gain.is_finite() || self.gain <= 0.0 {
            return Err(EguiError::InvalidParam {
                what: "gain".to_owned(),
                reason: format!(
                    "display gain must be positive and finite, got {}",
                    self.gain
                ),
            });
        }
        if self.offset.is_some_and(|value| !value.is_finite()) {
            return Err(EguiError::InvalidParam {
                what: "offset".to_owned(),
                reason: format!("display offset must be finite, got {:?}", self.offset),
            });
        }
        if self
            .k
            .is_some_and(|value| !value.is_finite() || value <= 0.0)
        {
            return Err(EguiError::InvalidParam {
                what: "k".to_owned(),
                reason: format!(
                    "display steepness must be a positive finite number, got {:?}",
                    self.k
                ),
            });
        }
        if self
            .mult
            .is_some_and(|value| !value.is_finite() || value <= 0.0)
        {
            return Err(EguiError::InvalidParam {
                what: "mult".to_owned(),
                reason: format!(
                    "display band half-width must be a positive finite number, got {:?}",
                    self.mult
                ),
            });
        }
        Ok(())
    }

    /// One kind's row with its catalog triple: normalized kinds carry
    /// their frozen `(offset, k, multiplier)`; unmapped kinds carry valid
    /// but hidden placeholders the resolver never reads.
    #[must_use]
    pub fn for_kind(kind: &str) -> Self {
        // The catalog row is the one home for the affine triple. Kinds the
        // catalog does not default here keep the neutral squash the panel has
        // always shown, so an unlisted kind cannot inherit a calibrated shape.
        let (offset, k, mult) = match kind {
            "rsi" | "mfi" | "cci" | "cmo" | "stoch" | "aroon" => {
                protopyne_kernels::lite::leg_constants(kind).expect("catalog-backed kinds resolve")
            }
            _ => (0.0, 1.0, 100.0),
        };
        Self::new()
            .period(14)
            .smooth_type(default_kind_smooth_type())
            .smooth_period(default_kind_smooth_period())
            .gain(default_kind_gain())
            .offset(offset)
            .k(k)
            .mult(mult)
            .build()
    }
}

impl Default for KindDefaults {
    fn default() -> Self {
        Self::for_kind("rsi")
    }
}

impl Validate for KindDefaults {
    type Err = EguiError;
    fn validate(&self) -> Result<(), Self::Err> {
        KindDefaults::validate(self)
    }
}

impl WorkUnit for KindDefaults {
    fn name(&self) -> &str {
        "kind_defaults"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        check_for_execute(self, "kind defaults invalid")?;
        Ok(valid_output(format!(
            "kind defaults ready (length {})",
            self.period
        )))
    }
}

impl_component!(KindDefaults);

/// The whole Oscillators panel: one input row per [`PANEL_KINDS`] kind.
#[derive(Debug, Clone, PartialEq, bon::Builder, serde::Serialize, serde::Deserialize)]
#[builder(start_fn = new)]
pub struct OscillatorPanelState {
    /// RSI kind defaults.
    #[builder(default = KindDefaults::for_kind("rsi"))]
    pub rsi: KindDefaults,
    /// CCI kind defaults.
    #[builder(default = KindDefaults::for_kind("cci"))]
    pub cci: KindDefaults,
    /// CMO kind defaults.
    #[builder(default = KindDefaults::for_kind("cmo"))]
    pub cmo: KindDefaults,
    /// MFI kind defaults.
    #[builder(default = KindDefaults::for_kind("mfi"))]
    pub mfi: KindDefaults,
    /// Stochastic kind defaults.
    #[builder(default = KindDefaults::for_kind("stoch"))]
    pub stoch: KindDefaults,
    /// Aroon kind defaults.
    #[builder(default = KindDefaults::for_kind("aroon"))]
    pub aroon: KindDefaults,
    /// ADX kind defaults.
    #[builder(default = KindDefaults::for_kind("adx"))]
    pub adx: KindDefaults,
}

impl OscillatorPanelState {
    /// Read one kind's row; refuses an unknown kind instead of substituting.
    pub fn entry(&self, kind: &str) -> Result<&KindDefaults, EguiError> {
        match kind {
            "rsi" => Ok(&self.rsi),
            "cci" => Ok(&self.cci),
            "cmo" => Ok(&self.cmo),
            "mfi" => Ok(&self.mfi),
            "stoch" => Ok(&self.stoch),
            "aroon" => Ok(&self.aroon),
            "adx" => Ok(&self.adx),
            _ => Err(EguiError::InvalidParam {
                what: "kind".to_owned(),
                reason: format!("{kind:?} is not an oscillator panel kind"),
            }),
        }
    }

    /// Write one kind's row; refuses an unknown kind instead of substituting.
    pub fn entry_mut(&mut self, kind: &str) -> Result<&mut KindDefaults, EguiError> {
        match kind {
            "rsi" => Ok(&mut self.rsi),
            "cci" => Ok(&mut self.cci),
            "cmo" => Ok(&mut self.cmo),
            "mfi" => Ok(&mut self.mfi),
            "stoch" => Ok(&mut self.stoch),
            "aroon" => Ok(&mut self.aroon),
            "adx" => Ok(&mut self.adx),
            _ => Err(EguiError::InvalidParam {
                what: "kind".to_owned(),
                reason: format!("{kind:?} is not an oscillator panel kind"),
            }),
        }
    }

    /// Fail-closed self-consistency check: every row validates, with the
    /// kind named first so a dock refusal points at its row.
    pub fn validate(&self) -> Result<(), EguiError> {
        for kind in PANEL_KINDS {
            self.entry(kind)?.validate().map_err(|error| match error {
                EguiError::InvalidParam { what, reason } => EguiError::InvalidParam {
                    what: format!("{kind}.{what}"),
                    reason,
                },
                error @ EguiError::Dependency { .. } => error,
            })?;
        }
        Ok(())
    }
}

impl Default for OscillatorPanelState {
    fn default() -> Self {
        Self::new().build()
    }
}

impl Validate for OscillatorPanelState {
    type Err = EguiError;
    fn validate(&self) -> Result<(), Self::Err> {
        OscillatorPanelState::validate(self)
    }
}

/// Dock label per panel kind.
#[must_use]
pub fn panel_kind_label(kind: &str) -> &'static str {
    match kind {
        "rsi" => "RSI",
        "cci" => "CCI",
        "cmo" => "CMO",
        "mfi" => "MFI",
        "stoch" => "Stochastic",
        "aroon" => "Aroon",
        "adx" => "ADX",
        _ => "Oscillator",
    }
}

/// One kind theme key as a single-field settings target, so the dock color
/// row reuses the shared Style-tab color editor instead of hand-rolling
/// one. Writes validate through the same hex reader the painter uses, so
/// a color that edits is a color that paints.
pub struct ThemeKindColor<'a> {
    /// Theme the kind key lives in.
    pub theme: &'a mut fluent_egui::ThemeConfig,
    /// Kind key to read and write.
    pub kind: &'static str,
}

impl FieldAccess for ThemeKindColor<'_> {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        if name != "color" {
            return Err(FieldError::NotFound(name.to_owned()));
        }
        let slot = value.trim();
        fluent_egui::parse_hex_color(self.kind, slot)
            .map_err(|error| FieldError::Constraint(format!("{error}")))?;
        let slot = slot.to_owned();
        match self.kind {
            "rsi" => self.theme.rsi = slot,
            "cci" => self.theme.cci = slot,
            "cmo" => self.theme.cmo = slot,
            "mfi" => self.theme.mfi = slot,
            "stoch" => self.theme.stoch = slot,
            "aroon" => self.theme.aroon = slot,
            "adx" => self.theme.adx = slot,
            _ => return Err(FieldError::NotFound(self.kind.to_owned())),
        }
        Ok(())
    }

    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        if name != "color" {
            return Err(FieldError::NotFound(name.to_owned()));
        }
        self.theme
            .style(self.kind)
            .map(str::to_owned)
            .ok_or_else(|| FieldError::NotFound(self.kind.to_owned()))
    }

    fn field_names(&self) -> &'static [&'static str] {
        &["color"]
    }
}

impl Describable for ThemeKindColor<'_> {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "color": {
                    "type": "string",
                    "format": "color",
                    "description": format!(
                        "{} oscillator color (#RRGGBB)",
                        panel_kind_label(self.kind)
                    ),
                },
            },
        })
    }
}

#[cfg(test)]
#[path = "../tests/taxonomy_single_truth.rs"]
mod taxonomy_single_truth_tests;
