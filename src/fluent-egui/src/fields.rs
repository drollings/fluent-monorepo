//! Schema-driven control-plane field editors.
//!
//! Every control-plane struct derives `FieldAccess` + `Describable`, so its
//! `describe()` document already names each field, its type, its hint, and
//! its declared numeric range. This module reads that document once into
//! [`SchemaField`]s and paints one editor per field, so a control renders
//! from its schema and a new field appears in the UI without a UI edit.
//!
//! Two rules keep the editors honest. A value the widget produces that the
//! field's own contract refuses (non-finite float, out-of-range integer,
//! over-length string) is **not** written and **not** clamped — the refusal
//! is returned with the field's name, because a silently clamped control is
//! a control that lies about what it holds. And an editor never binds a
//! schema `minimum`/`maximum` as a drag bound: the bound would make the
//! operator's out-of-range input unreachable instead of visibly refused,
//! which is the difference between a constraint and a guard rail.

use fluent_wvr::FieldAccess;

/// What a control-plane field's schema says it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// Whole number, edited as a drag value.
    Integer,
    /// Real number, edited as a drag value.
    Number,
    /// On/off, edited as a checkbox.
    Boolean,
    /// Free text, edited as a single-line text edit.
    Text,
}

impl FieldKind {
    /// The schema `type` this kind reads. Anything else falls back to text:
    /// an unrecognised type renders rather than disappearing, and its write
    /// still goes through the field's own `set_field` contract.
    #[must_use]
    pub fn of_schema(schema_type: Option<&str>) -> Self {
        match schema_type {
            Some("integer") => Self::Integer,
            Some("number") => Self::Number,
            Some("boolean") => Self::Boolean,
            _ => Self::Text,
        }
    }
}

/// One field lifted from a `Describable` schema.
#[derive(Debug, Clone, PartialEq)]
pub struct SchemaField {
    /// Property name; also the `FieldAccess` key.
    pub name: String,
    /// Type the schema declares.
    pub kind: FieldKind,
    /// Format the schema declares (`date-time`, `decimal`, …), when it
    /// carries one. A format refines a kind without replacing it: a decimal
    /// is still a string on the wire, but an editor that knows the format
    /// can hold the text to its exact contract instead of a freeform box.
    pub format: Option<String>,
    /// Schema hint, rendered beside the editor.
    pub description: String,
    /// Declared lower bound, when the schema carries one.
    pub minimum: Option<f64>,
    /// Declared upper bound, when the schema carries one.
    pub maximum: Option<f64>,
    /// Declared text length cap, when the schema carries one.
    pub max_len: Option<usize>,
}

/// Every field in `schema`, in schema order. Properties are read from the
/// document itself, so this is the only place that walks them. Order is the
/// document's order (`serde_json` preserves map insertion order with its
/// default `preserve_order` feature, which this workspace relies on for
/// schema rendering).
#[must_use]
pub fn schema_fields(schema: &serde_json::Value) -> Vec<SchemaField> {
    schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .map(|properties| {
            properties
                .iter()
                .map(|(name, hint)| SchemaField {
                    name: name.clone(),
                    kind: FieldKind::of_schema(hint.get("type").and_then(serde_json::Value::as_str)),
                    format: hint
                        .get("format")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                    description: hint
                        .get("description")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    minimum: number_hint(hint.get("minimum")),
                    maximum: number_hint(hint.get("maximum")),
                    max_len: usize_hint(hint.get("maxLength")),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A declared numeric bound, whether the schema spells it as a JSON number
/// or as its decimal text. Both spellings occur in practice — a bound that
/// arrives in a form this cannot read is a bound the control would quietly
/// drop, and a dropped constraint is worse than a parsed one.
fn number_hint(hint: Option<&serde_json::Value>) -> Option<f64> {
    match hint? {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

/// A declared length cap, in the same two spellings as [`number_hint`].
fn usize_hint(hint: Option<&serde_json::Value>) -> Option<usize> {
    match hint? {
        serde_json::Value::Number(number) => number.as_u64().map(|limit| limit as usize),
        serde_json::Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

/// What one pass of the field editors did to the target.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FieldEdits {
    /// Field names written this frame, in schema order.
    pub written: Vec<String>,
    /// Refusal reasons, each naming its field. A refusal leaves the target
    /// exactly as it was.
    pub refusals: Vec<String>,
}

/// One tab of an indicator settings pane: calculation inputs, paint styling,
/// or the visibility toggles.
///
/// The split is a routing over field names, not a second schema: every
/// lifted field lands in exactly one tab (see [`classify_schema_field`]),
/// so the three tabs partition the schema without loss or overlap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    /// Calculation parameters: lengths, periods, smoothing, sources.
    Inputs,
    /// Paint knobs: colors, alpha, line style, thresholds.
    Style,
    /// On/off toggles: `show_*` and `visible` booleans.
    Visibility,
}

impl SettingsTab {
    /// Every tab, in render order.
    #[must_use]
    pub fn all() -> [Self; 3] {
        [Self::Inputs, Self::Style, Self::Visibility]
    }

    /// The fixed tab label the settings pane renders.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Inputs => "Inputs",
            Self::Style => "Style",
            Self::Visibility => "Visibility",
        }
    }
}

/// Route one lifted field to its settings tab.
///
/// Precedence is fixed so the partition is total: a boolean visibility name
/// (`show_*`, `visible`) wins over everything, then a paint-style name
/// (color, alpha, transparency, brightness, threshold, style, theme), and
/// everything else — calculation fields and the unclassified remainder —
/// renders under Inputs. Name matching is case-insensitive, because schemas
/// arrive in both `snake_case` and display casing.
#[must_use]
pub fn classify_schema_field(field: &SchemaField) -> SettingsTab {
    let name = field.name.to_ascii_lowercase();
    let is_toggle = name == "visible"
        || name.starts_with("visible_")
        || name.starts_with("show_")
        || name == "show";
    if field.kind == FieldKind::Boolean && is_toggle {
        return SettingsTab::Visibility;
    }
    if ["color", "alpha", "transparency", "brightness", "threshold", "style", "theme"]
        .iter()
        .any(|token| name.contains(token))
    {
        return SettingsTab::Style;
    }
    SettingsTab::Inputs
}

/// The schema fields that render under `tab`, in schema order.
#[must_use]
pub fn fields_for_tab(fields: &[SchemaField], tab: SettingsTab) -> Vec<SchemaField> {
    fields.iter().filter(|field| classify_schema_field(field) == tab).cloned().collect()
}

/// Paint the tab bar plus the active tab's editors, writing accepted edits
/// straight through `target`'s own `FieldAccess` contract.
///
/// The caller owns `active_tab`; rendering never moves it. Only the active
/// tab's fields render, so an untouched frame writes nothing — exactly the
/// [`paint_schema_fields`] contract, scoped to one tab.
pub fn paint_tabbed_schema_fields(
    ui: &mut egui::Ui,
    active_tab: &mut SettingsTab,
    fields: &[SchemaField],
    target: &mut dyn FieldAccess,
) -> FieldEdits {
    ui.horizontal(|bar| {
        for tab in SettingsTab::all() {
            bar.selectable_value(active_tab, tab, tab.label());
        }
    });
    paint_schema_fields(ui, &fields_for_tab(fields, *active_tab), target)
}

/// Paint one editor per field into `ui`, writing accepted edits straight
/// through `target`'s own `FieldAccess` contract.
///
/// The editors are dumb on purpose: they read the current value, edit it,
/// and write it back through the field's own validation. Nothing here knows
/// what any particular field means, so the same pass serves a pane config, a
/// replay cursor, and a grant tier.
pub fn paint_schema_fields(
    ui: &mut egui::Ui,
    fields: &[SchemaField],
    target: &mut dyn FieldAccess,
) -> FieldEdits {
    let mut edits = FieldEdits::default();
    for field in fields {
        ui.horizontal(|row| {
            row.label(&field.name).on_hover_text(bounds_hint(field));
            paint_one(row, field, target, &mut edits);
        });
    }
    edits
}

/// Hover text for one field: the schema description plus the declared
/// bounds the field's own contract enforces. Bounds are display only — the
/// refusal still comes from `set_field`, never from clamping or from
/// binding the widget to the bound (which would make out-of-range input
/// unreachable instead of visibly refused).
fn bounds_hint(field: &SchemaField) -> String {
    let mut hint = field.description.clone();
    let mut bounds = Vec::new();
    if let Some(minimum) = field.minimum {
        bounds.push(format!("min {minimum}"));
    }
    if let Some(maximum) = field.maximum {
        bounds.push(format!("max {maximum}"));
    }
    if let Some(max_len) = field.max_len {
        bounds.push(format!("max length {max_len}"));
    }
    if let Some(format) = field.format.as_deref() {
        bounds.push(format!("format {format}"));
    }
    if !bounds.is_empty() {
        if hint.is_empty() {
            hint.push('(');
        } else {
            hint.push_str(" (");
        }
        hint.push_str(&bounds.join(", "));
        hint.push(')');
    }
    hint
}

/// One field's editor: read, edit, write back only what changed.
fn paint_one(
    row: &mut egui::Ui,
    field: &SchemaField,
    target: &mut dyn FieldAccess,
    edits: &mut FieldEdits,
) {
    let current = match target.get_field(&field.name) {
        Ok(current) => current,
        Err(error) => {
            edits.refusals.push(refusal(field, error.to_string()));
            return;
        }
    };
    let edited = match field.kind {
        FieldKind::Integer => edit_integer(row, field, &current, edits),
        FieldKind::Number => edit_number(row, field, &current, edits),
        FieldKind::Boolean => {
            let trimmed = current.trim().to_ascii_lowercase();
            let canonical = match trimmed.as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            };
            let Some(value) = canonical else {
                edits.refusals.push(refusal(
                    field,
                    format!("{current:?} is not an editable boolean"),
                ));
                return;
            };
            let mut value = value;
            let changed = row.checkbox(&mut value, "").changed();
            changed.then(|| value.to_string())
        }
        FieldKind::Text => {
            let mut value = current.clone();
            let response = row.add(
                egui::TextEdit::singleline(&mut value).hint_text(field.description.clone()),
            );
            (response.changed() && value != current).then_some(value)
        }
    };
    let Some(edited) = edited else {
        return;
    };
    match target.set_field(&field.name, &edited) {
        Ok(()) => edits.written.push(field.name.clone()),
        Err(error) => edits.refusals.push(refusal(field, error.to_string())),
    }
}

/// A whole-number editor. A value this editor cannot hold is reported, not
/// rounded or wrapped into the widget.
fn edit_integer(
    row: &mut egui::Ui,
    field: &SchemaField,
    current: &str,
    edits: &mut FieldEdits,
) -> Option<String> {
    let Ok(value) = current.parse::<i64>() else {
        edits.refusals.push(refusal(field, format!("{current} is not an editable integer")));
        return None;
    };
    let mut value = value;
    let changed = row.add(egui::DragValue::new(&mut value).speed(1)).changed();
    changed.then(|| value.to_string())
}

/// A real-number editor. `NaN` and the infinities are never written: a
/// control holding one would render it as a value it does not have.
fn edit_number(
    row: &mut egui::Ui,
    field: &SchemaField,
    current: &str,
    edits: &mut FieldEdits,
) -> Option<String> {
    let Ok(value) = current.parse::<f64>() else {
        edits.refusals.push(refusal(field, format!("{current} is not an editable number")));
        return None;
    };
    let mut value = value;
    let changed = row.add(egui::DragValue::new(&mut value).speed(0.01)).changed();
    if !changed {
        return None;
    }
    if !value.is_finite() {
        edits.refusals.push(refusal(field, "a real number must be finite".to_owned()));
        return None;
    }
    Some(value.to_string())
}

/// One refusal, naming the field it belongs to.
fn refusal(field: &SchemaField, reason: impl std::fmt::Display) -> String {
    format!("{}: {reason}", field.name)
}

#[cfg(test)]
#[path = "../tests/fields.rs"]
mod tests;