//! Per-indicator paint overrides: the Style tab's control-plane component.
//!
//! Series carry *style keys* (`"fast"`, `"bull"`), never color literals, and
//! [`ThemeConfig`] resolves those keys for the whole chart. That leaves no
//! room for one indicator to want its own colors or a heavier stroke — the
//! thing the Style tab in an indicator's settings pane exists to do. This
//! module is that room, and it is one shared component rather than a field
//! per indicator: [`IndicatorStyle`] holds a stroke width plus an optional
//! `#RRGGBB` override for each overridable theme key, and every keyed emitter
//! reads a color through [`IndicatorStyle::color`], which prefers the
//! override and otherwise falls back to the theme.
//!
//! Two rules keep the override honest. It is *additive*: clearing a field
//! (an empty value) restores the theme color rather than freezing the one
//! that was there, so the theme remains the chart's default truth. And it is
//! *refused, never repaired*: a value the field's own contract rejects (an
//! out-of-range width, a malformed color) is not written and not clamped, so
//! a control never reports holding something it does not.
//!
//! The schema is projected from one const, [`STYLE_FIELDS`] — the flat
//! `FieldAccess` key set, the `Describable` document, and the key lookup all
//! read it, so a new overridable key appears in the settings pane, in the
//! resolver, and in the schema together instead of drifting apart.

use std::collections::BTreeMap;

use fluent_wvr::prelude::*;
use fluent_wvr::validate::{check_for_execute, valid_output, Validate};
use fluent_wvr::{Describable, FieldAccess};

use crate::config::ThemeConfig;
use crate::error::EguiError;
use crate::paint::{parse_hex_color, resolve_color};

/// Narrowest stroke a style may ask for: thinner than a pixel stops reading
/// as a line on any display.
pub const MIN_LINE_WIDTH: f64 = 0.5;
/// Widest stroke a style may ask for; beyond this a series stops reading as
/// a series and starts reading as a block.
pub const MAX_LINE_WIDTH: f64 = 8.0;
/// The stroke width a style with no override paints at, matching every
/// emitter's historical hairline.
pub const DEFAULT_LINE_WIDTH: f64 = 1.0;

/// The Style tab's field name for the stroke width.
pub const LINE_WIDTH_FIELD: &str = "line_width";

/// The Style tab's flat field names: the stroke width, then one color per
/// overridable theme key in theme order.
///
/// This list is the single schema source. `FieldAccess` answers for exactly
/// these names, `describe()` declares exactly these properties, and
/// [`style_key`] strips the `color_` prefix to recover the theme key a field
/// overrides — so a field cannot exist without a key behind it, and a key
/// cannot resolve without a field to edit it.
pub const STYLE_FIELDS: [&str; 16] = [
    "line_width",
    "color_bull",
    "color_bear",
    "color_bull_down",
    "color_bear_up",
    "color_fast",
    "color_slow",
    "color_ribbon_bull",
    "color_ribbon_bear",
    "color_rsi",
    "color_cci",
    "color_cmo",
    "color_mfi",
    "color_stoch",
    "color_aroon",
    "color_adx",
];

/// The theme style key one Style field overrides (`"color_fast"` → `"fast"`),
/// or `None` for the width field and for names outside [`STYLE_FIELDS`].
///
/// Chrome keys (`background`, `grid`) are deliberately absent from
/// [`STYLE_FIELDS`]: an indicator paints data marks, so it may restyle the
/// data it draws but never the surface it draws on.
#[must_use]
pub fn style_key(field: &str) -> Option<&'static str> {
    STYLE_FIELDS
        .iter()
        .copied()
        .find(|name| *name == field)
        .and_then(|name| name.strip_prefix("color_"))
}

/// One indicator's paint overrides: a stroke width and a color per
/// overridable theme key.
///
/// Every field is optional in the sense that an unset override inherits the
/// theme, so the default value is not "a look" but "no opinion": a fresh
/// style paints exactly what the theme paints.
#[derive(Debug, Clone, PartialEq)]
pub struct IndicatorStyle {
    line_width: f64,
    colors: BTreeMap<String, String>,
}

impl IndicatorStyle {
    /// The stroke width in pixels, clamped into
    /// `[MIN_LINE_WIDTH, MAX_LINE_WIDTH]` so a style that somehow holds an
    /// out-of-band value still paints a legible line.
    #[must_use]
    pub fn line_width(&self) -> f32 {
        self.line_width.clamp(MIN_LINE_WIDTH, MAX_LINE_WIDTH) as f32
    }

    /// The stroke a style asks for, in one color.
    #[must_use]
    pub fn stroke(&self, color: egui::Color32) -> egui::Stroke {
        egui::Stroke::new(self.line_width(), color)
    }

    /// The `#RRGGBB` override set for `key`, or `None` when it inherits.
    #[must_use]
    pub fn override_color(&self, key: &str) -> Option<&str> {
        self.colors.get(key).map(String::as_str)
    }

    /// Resolve one style key to a paint color: the override when the slot is
    /// set, else the theme's slot.
    ///
    /// The single lookup every styled emitter uses, so a themed chart and a
    /// restyled indicator can never disagree about what a key means. A
    /// malformed override refuses with the key named, exactly like a
    /// malformed theme slot — paint never falls back to gray silently.
    pub fn color(&self, theme: &ThemeConfig, key: &str) -> Result<egui::Color32, EguiError> {
        match self.override_color(key) {
            Some(slot) => parse_hex_color(key, slot),
            None => resolve_color(theme, key),
        }
    }

    /// Fail-closed self-consistency check: the stroke width sits in its band
    /// and every set override spells a `#RRGGBB` color.
    pub fn validate(&self) -> Result<(), EguiError> {
        if !self.line_width.is_finite()
            || !(MIN_LINE_WIDTH..=MAX_LINE_WIDTH).contains(&self.line_width)
        {
            return Err(EguiError::InvalidParam {
                what: LINE_WIDTH_FIELD.to_owned(),
                reason: format!(
                    "line width must be finite and sit within [{MIN_LINE_WIDTH}, {MAX_LINE_WIDTH}]"
                ),
            });
        }
        for (key, slot) in &self.colors {
            if style_key(&format!("color_{key}")).is_none() {
                return Err(EguiError::InvalidParam {
                    what: format!("color_{key}"),
                    reason: "not an overridable theme style key".to_owned(),
                });
            }
            parse_hex_color(key, slot)?;
        }
        Ok(())
    }
}

impl Default for IndicatorStyle {
    /// The style that changes nothing: the theme's own colors at the
    /// historical hairline width.
    fn default() -> Self {
        Self {
            line_width: DEFAULT_LINE_WIDTH,
            colors: BTreeMap::new(),
        }
    }
}

impl Validate for IndicatorStyle {
    type Err = EguiError;
    fn validate(&self) -> Result<(), Self::Err> {
        IndicatorStyle::validate(self)
    }
}

impl FieldAccess for IndicatorStyle {
    /// Write one Style field. An empty color clears the override so the theme
    /// shows through again; a malformed color or an out-of-band width is
    /// refused with the field named, leaving the style exactly as it was.
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        if name == LINE_WIDTH_FIELD {
            let width: f64 = value.trim().parse().map_err(|_| {
                FieldError::Parse(format!(
                    "invalid line width for '{LINE_WIDTH_FIELD}': {value}"
                ))
            })?;
            if !width.is_finite() || !(MIN_LINE_WIDTH..=MAX_LINE_WIDTH).contains(&width) {
                return Err(FieldError::Constraint(format!(
                    "line width must be finite and sit within [{MIN_LINE_WIDTH}, {MAX_LINE_WIDTH}]: {value}"
                )));
            }
            self.line_width = width;
            return Ok(());
        }
        let key = style_key(name).ok_or_else(|| FieldError::NotFound(name.to_owned()))?;
        let slot = value.trim();
        if slot.is_empty() {
            self.colors.remove(key);
            return Ok(());
        }
        parse_hex_color(key, slot).map_err(|error| FieldError::Constraint(format!("{error}")))?;
        self.colors.insert(key.to_owned(), slot.to_owned());
        Ok(())
    }

    /// Read one Style field; an unset override reads as the empty string,
    /// which is the "inherit" the editor round-trips.
    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        if name == LINE_WIDTH_FIELD {
            return Ok(self.line_width.to_string());
        }
        let key = style_key(name).ok_or_else(|| FieldError::NotFound(name.to_owned()))?;
        Ok(self.override_color(key).unwrap_or_default().to_owned())
    }

    fn field_names(&self) -> &'static [&'static str] {
        &STYLE_FIELDS
    }
}

impl Describable for IndicatorStyle {
    /// The schema the settings pane renders: one `number` property for the
    /// stroke width and one `color`-formatted string per overridable key,
    /// built from [`STYLE_FIELDS`] so the document and the accessor cannot
    /// disagree about which fields exist.
    fn describe(&self) -> serde_json::Value {
        let mut properties = serde_json::Map::new();
        properties.insert(
            LINE_WIDTH_FIELD.to_owned(),
            serde_json::json!({
                "type": "number",
                "description": "Stroke width in pixels for this indicator's lines",
                "minimum": MIN_LINE_WIDTH,
                "maximum": MAX_LINE_WIDTH,
            }),
        );
        for field in STYLE_FIELDS
            .iter()
            .filter(|field| **field != LINE_WIDTH_FIELD)
        {
            let key = style_key(field).unwrap_or_default();
            properties.insert(
                (*field).to_owned(),
                serde_json::json!({
                    "type": "string",
                    "format": "color",
                    "description": format!(
                        "{key} color override (#RRGGBB; empty inherits the theme)"
                    ),
                }),
            );
        }
        serde_json::json!({ "type": "object", "properties": properties })
    }
}

impl WorkUnit for IndicatorStyle {
    fn name(&self) -> &str {
        "indicator_style"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        check_for_execute(self, "indicator style invalid")?;
        Ok(valid_output(format!(
            "indicator style ready ({:.2} px, {} overrides)",
            self.line_width,
            self.colors.len()
        )))
    }
}

impl_component!(IndicatorStyle);

#[cfg(test)]
#[path = "../tests/style.rs"]
mod tests;
