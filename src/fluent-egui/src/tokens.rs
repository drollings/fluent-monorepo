//! Material token theme system: a validated role table resolving to one
//! immutable scheme.
//!
//! [`MaterialTokens`] is the shell-owned token component (mode, contrast,
//! and the closed role vocabulary below). [`MaterialTokens::resolve`] parses
//! every role once into a [`ResolvedScheme`]; [`chart_roles`] projects the
//! chart's slots out of that scheme through the same hex reader the theme
//! uses, so a chart in a token shell cannot disagree with the theme about a
//! shared slot. [`apply_visuals`] lowers a scheme to `egui` visuals. Shells
//! inject their own dark-mode detector into [`MaterialTokens::resolve_auto`];
//! no detector lives here.

use std::collections::BTreeMap;

use common_core::hash::fingerprint_framed;
use fluent_wvr::prelude::*;
use fluent_wvr::validate::{check_for_execute, valid_output, Validate};
use fluent_wvr::{Describable, FieldAccess};

use crate::config::ThemeConfig;
use crate::error::EguiError;
use crate::paint::parse_hex_color;

fn default_mode() -> String {
    "auto".to_owned()
}

/// Every settable field: the two dials plus the closed role vocabulary.
/// Roles the table does not name refuse instead of resolving.
static FIELD_NAMES: &[&str] = &[
    "mode",
    "contrast",
    "primary",
    "on_primary",
    "primary_container",
    "on_primary_container",
    "secondary",
    "on_secondary",
    "secondary_container",
    "on_secondary_container",
    "tertiary",
    "on_tertiary",
    "tertiary_container",
    "on_tertiary_container",
    "error",
    "on_error",
    "error_container",
    "on_error_container",
    "background",
    "on_background",
    "surface",
    "on_surface",
    "surface_variant",
    "on_surface_variant",
    "surface_dim",
    "surface_bright",
    "surface_container_lowest",
    "surface_container_low",
    "surface_container",
    "surface_container_high",
    "surface_container_highest",
    "inverse_surface",
    "inverse_on_surface",
    "inverse_primary",
    "outline",
    "outline_variant",
    "shadow",
    "scrim",
    "surface_tint",
    "bull",
    "bear",
    "fast",
    "slow",
    "ribbon_bull",
    "ribbon_bear",
    "bull_down",
    "bear_up",
    "rsi",
    "cci",
    "cmo",
    "mfi",
    "stoch",
    "aroon",
    "adx",
    "grid",
];

/// The closed role vocabulary: every name [`MaterialTokens`] accepts.
#[must_use]
pub fn known_roles() -> &'static [&'static str] {
    &FIELD_NAMES[2..]
}

/// Named token theme: a mode, a contrast dial, and role colors.
///
/// Roles enter through [`insert_role`](Self::insert_role), `set_field`, or
/// deserialization; every entry point validates before anything resolves.
#[derive(Debug, Clone, bon::Builder, serde::Serialize, serde::Deserialize)]
#[builder(start_fn = new)]
pub struct MaterialTokens {
    /// Color mode: `light`, `dark`, or `auto` (via the injected detector).
    #[builder(default = default_mode())]
    #[serde(default = "default_mode")]
    mode: String,
    /// Contrast dial in `[-1, 1]`, carried into the resolved scheme.
    #[builder(default)]
    #[serde(default)]
    contrast: f64,
    /// Role colors as `#RRGGBB`.
    #[builder(default)]
    #[serde(default)]
    roles: BTreeMap<String, String>,
}

impl MaterialTokens {
    /// Register one role color; unknown names and malformed hex refuse with
    /// the role named.
    pub fn insert_role(&mut self, role: &str, hex: &str) -> Result<(), EguiError> {
        if !known_roles().contains(&role) {
            return Err(EguiError::InvalidParam {
                what: role.to_owned(),
                reason: "unknown token role".to_owned(),
            });
        }
        parse_hex_color(role, hex)?;
        self.roles.insert(role.to_owned(), hex.to_owned());
        Ok(())
    }

    /// Borrow one role's hex (`None` for unset roles).
    #[must_use]
    pub fn role(&self, name: &str) -> Option<&str> {
        self.roles.get(name).map(String::as_str)
    }

    /// Fail-closed self-consistency check.
    pub fn validate(&self) -> Result<(), EguiError> {
        match self.mode.trim().to_ascii_lowercase().as_str() {
            "light" | "dark" | "auto" => {}
            _ => {
                return Err(EguiError::InvalidParam {
                    what: "mode".to_owned(),
                    reason: "mode must be light, dark, or auto".to_owned(),
                });
            }
        }
        if !self.contrast.is_finite() || self.contrast < -1.0 || self.contrast > 1.0 {
            return Err(EguiError::InvalidParam {
                what: "contrast".to_owned(),
                reason: "contrast must sit within [-1, 1]".to_owned(),
            });
        }
        for (role, hex) in &self.roles {
            if !known_roles().contains(&role.as_str()) {
                return Err(EguiError::InvalidParam {
                    what: role.clone(),
                    reason: "unknown token role".to_owned(),
                });
            }
            parse_hex_color(role, hex)?;
        }
        Ok(())
    }

    /// Resolve every role into an immutable scheme for `dark` mode or not.
    /// Refuses instead of resolving a half-valid table.
    pub fn resolve(&self, dark: bool) -> Result<ResolvedScheme, EguiError> {
        self.validate()?;
        let mut colors = BTreeMap::new();
        for (role, hex) in &self.roles {
            colors.insert(role.clone(), parse_hex_color(role, hex)?);
        }
        let dark_byte = [u8::from(dark)];
        let contrast_bytes = self.contrast.to_le_bytes();
        let mut framed: Vec<(&str, &[u8])> =
            vec![("contrast", &contrast_bytes), ("dark", &dark_byte)];
        for (role, hex) in &self.roles {
            framed.push((role.as_str(), hex.as_bytes()));
        }
        Ok(ResolvedScheme {
            dark,
            contrast: self.contrast,
            colors,
            fingerprint: fingerprint_framed(&framed),
        })
    }

    /// Resolve `auto` mode through the shell's dark-mode detector; explicit
    /// modes win over the detector. Refuses exactly like [`resolve`](Self::resolve).
    pub fn resolve_auto(
        &self,
        detect_dark: impl Fn() -> bool,
    ) -> Result<ResolvedScheme, EguiError> {
        let dark = match self.mode.trim().to_ascii_lowercase().as_str() {
            "dark" => true,
            "light" => false,
            _ => detect_dark(),
        };
        self.resolve(dark)
    }
}

impl FieldAccess for MaterialTokens {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        match name {
            "mode" => match value.trim().to_ascii_lowercase().as_str() {
                "light" | "dark" | "auto" => {
                    self.mode = value.trim().to_ascii_lowercase();
                    Ok(())
                }
                _ => Err(FieldError::Constraint(
                    "mode: must be light, dark, or auto".to_owned(),
                )),
            },
            "contrast" => {
                let parsed: f64 = value.parse().map_err(|_| {
                    FieldError::Parse(format!("invalid f64 for 'contrast': {value}"))
                })?;
                if !parsed.is_finite() || parsed < -1.0 || parsed > 1.0 {
                    return Err(FieldError::Constraint(
                        "contrast: must sit within [-1, 1]".to_owned(),
                    ));
                }
                self.contrast = parsed;
                Ok(())
            }
            role if known_roles().contains(&role) => {
                parse_hex_color(role, value)
                    .map_err(|error| FieldError::Parse(error.to_string()))?;
                self.roles.insert(role.to_owned(), value.to_owned());
                Ok(())
            }
            _ => Err(FieldError::NotFound(name.to_owned())),
        }
    }

    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        match name {
            "mode" => Ok(self.mode.clone()),
            "contrast" => Ok(self.contrast.to_string()),
            role if known_roles().contains(&role) => self
                .roles
                .get(role)
                .cloned()
                .ok_or_else(|| FieldError::NotFound(role.to_owned())),
            _ => Err(FieldError::NotFound(name.to_owned())),
        }
    }

    fn field_names(&self) -> &'static [&'static str] {
        FIELD_NAMES
    }
}

impl Describable for MaterialTokens {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "mode": {"type": "string", "description": "Color mode: light, dark, or auto"},
                "contrast": {"type": "number", "minimum": -1, "maximum": 1,
                             "description": "Contrast dial in [-1, 1]"},
                "roles": {"type": "object", "description": "Role colors as #RRGGBB",
                           "additionalProperties": {"type": "string"}}
            },
            "required": ["mode", "contrast"]
        })
    }
}

impl Validate for MaterialTokens {
    type Err = EguiError;
    fn validate(&self) -> Result<(), Self::Err> {
        MaterialTokens::validate(self)
    }
}

impl WorkUnit for MaterialTokens {
    fn name(&self) -> &str {
        "material_tokens"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        check_for_execute(self, "material tokens invalid")?;
        Ok(valid_output(format!(
            "tokens ready ({} roles)",
            self.roles.len()
        )))
    }
}

impl_component!(MaterialTokens);

/// One resolved scheme: every role parsed, immutable, fingerprinted for
/// cache keys. Bit-identical token input resolves bit-identically.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedScheme {
    /// Whether this scheme resolved for dark mode.
    pub dark: bool,
    /// The contrast dial the tokens carried.
    pub contrast: f64,
    /// Parsed role colors.
    pub colors: BTreeMap<String, egui::Color32>,
    /// Content fingerprint over mode, contrast, and every role.
    pub fingerprint: u64,
}

/// Project the chart's slots out of a resolved scheme: every theme style
/// key the scheme names a role for. Shell-only roles stay out, and the
/// caller layers the projection over its theme base — so a shared slot
/// paints one value on both sides.
#[must_use]
pub fn chart_roles(scheme: &ResolvedScheme) -> BTreeMap<String, egui::Color32> {
    ThemeConfig::style_keys()
        .iter()
        .filter_map(|key| {
            scheme
                .colors
                .get(*key)
                .map(|color| ((*key).to_owned(), *color))
        })
        .collect()
}

/// Lower a resolved scheme to `egui` visuals: the platform dark/light base
/// with the surface, ink, and primary slots applied. Roles the scheme does
/// not name keep the base values.
#[must_use]
pub fn apply_visuals(scheme: &ResolvedScheme) -> egui::Visuals {
    let mut visuals = if scheme.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    if let Some(surface) = scheme.colors.get("surface") {
        visuals.window_fill = *surface;
        visuals.panel_fill = *surface;
    }
    if let Some(on_surface) = scheme.colors.get("on_surface") {
        visuals.override_text_color = Some(*on_surface);
    }
    if let Some(primary) = scheme.colors.get("primary") {
        visuals.selection.bg_fill = *primary;
        visuals.hyperlink_color = *primary;
        if let Some(on_primary) = scheme.colors.get("on_primary") {
            visuals.selection.stroke = egui::Stroke::new(1.0, *on_primary);
        }
    }
    visuals
}

#[cfg(test)]
#[path = "../tests/tokens.rs"]
mod tests;
