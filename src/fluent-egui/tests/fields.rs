//! Schema-driven control-plane editors: the lift, the write path, and the
//! refusals.
//!
//! The rule under test is that a control knows its fields only through its
//! schema: every property becomes one editor, every write goes through the
//! field's own `FieldAccess` contract, and a refused write leaves the value
//! alone instead of clamping it.

use super::*;
use fluent_wvr::Describable;

/// A small control-plane value exercising every editor kind plus the
/// constraints an operator can violate from the keyboard.
#[derive(Debug, Clone, PartialEq, FieldAccess, Describable)]
struct Probe {
    #[field(desc = "Whole number", min = 0)]
    count: i64,
    #[field(desc = "Real number")]
    weight: f64,
    #[field(desc = "On or off")]
    armed: bool,
    #[field(desc = "Free text", max_len = 4)]
    label: String,
}

fn probe() -> Probe {
    Probe {
        count: 2,
        weight: 0.5,
        armed: false,
        label: "live".to_owned(),
    }
}

/// The unreachable-by-accident case: a whole number wider than the editor's
/// signed range, so the stored value is real but not editable here.
#[derive(Debug, Clone, PartialEq, FieldAccess, Describable)]
struct WideProbe {
    #[field(desc = "Whole number")]
    span: u64,
}

#[test]
fn schema_fields_lift_every_property_in_schema_order() {
    let fields = schema_fields(&probe().describe());
    let names: Vec<&str> = fields.iter().map(|field| field.name.as_str()).collect();
    // Property order is the schema's own map order, which is name order —
    // deterministic, and the order the editors render in every frame.
    assert_eq!(
        names,
        ["armed", "count", "label", "weight"],
        "one editor per property"
    );
    let field = |name: &str| {
        fields
            .iter()
            .find(|field| field.name == name)
            .unwrap_or_else(|| panic!("{name} lifts"))
            .clone()
    };
    assert_eq!(field("count").kind, FieldKind::Integer);
    assert_eq!(field("count").description, "Whole number");
    assert_eq!(
        field("count").minimum,
        Some(0.0),
        "the declared floor is carried, not invented"
    );
    assert_eq!(field("count").maximum, None);
    assert_eq!(field("weight").kind, FieldKind::Number);
    assert_eq!(field("armed").kind, FieldKind::Boolean);
    assert_eq!(field("label").kind, FieldKind::Text);
    assert_eq!(
        field("label").max_len,
        Some(4),
        "the declared text cap is carried"
    );
}

#[test]
fn a_declared_bound_is_read_in_either_spelling() {
    // Bounds reach a schema as JSON numbers or as their decimal text. A
    // bound this lift cannot read is a constraint the control would drop
    // without saying so, so both spellings are read.
    for minimum in [serde_json::json!(0), serde_json::json!("0")] {
        let lifted = schema_fields(&serde_json::json!({"properties": {
            "count": {"type": "integer", "minimum": minimum}
        }}));
        assert_eq!(lifted[0].minimum, Some(0.0));
    }
    for cap in [serde_json::json!(4), serde_json::json!("4")] {
        let lifted = schema_fields(&serde_json::json!({"properties": {
            "label": {"type": "string", "maxLength": cap}
        }}));
        assert_eq!(lifted[0].max_len, Some(4));
    }
}

#[test]
fn an_enum_list_lifts_a_select_and_a_malformed_one_falls_back_to_text() {
    let lifted = schema_fields(&serde_json::json!({"properties": {
        "kind": {"type": "string", "enum": ["sma", "ema"]}
    }}));
    assert_eq!(lifted[0].kind, FieldKind::Select);
    assert_eq!(lifted[0].options, vec!["sma".to_owned(), "ema".to_owned()]);
    for bad in [
        serde_json::json!({"properties": {"kind": {"type": "string"}}}),
        serde_json::json!({"properties": {"kind": {"type": "string", "enum": []}}}),
        serde_json::json!({"properties": {"kind": {"type": "string", "enum": [1, 2]}}}),
    ] {
        let lifted = schema_fields(&bad);
        assert_eq!(
            lifted[0].kind,
            FieldKind::Text,
            "a malformed list degrades: {bad}"
        );
        assert!(lifted[0].options.is_empty());
    }
}

#[test]
fn an_untyped_or_empty_schema_yields_no_fields() {
    assert!(schema_fields(&serde_json::json!({})).is_empty());
    assert!(schema_fields(&serde_json::json!({"properties": {}})).is_empty());
    // An unknown type renders as text rather than disappearing: the field
    // is still editable, and its own write contract still applies.
    let lifted = schema_fields(&serde_json::json!({"properties": {
        "mystery": {"type": "array", "description": "unsupported"}
    }}));
    assert_eq!(lifted.len(), 1);
    assert_eq!(lifted[0].kind, FieldKind::Text);
}

#[test]
fn painting_an_untouched_control_writes_nothing_and_refuses_nothing() {
    let mut control = probe();
    let fields = schema_fields(&control.describe());
    egui::__run_test_ui(|ui| {
        let edits = paint_schema_fields(ui, &fields, &mut control);
        assert_eq!(
            edits,
            FieldEdits::default(),
            "an untouched frame changes nothing"
        );
    });
    assert_eq!(control, probe());
}

#[test]
fn a_refused_write_leaves_the_target_exactly_as_it_was() {
    // The editors never clamp: they write through `set_field`, so the
    // field's own constraint is what refuses, and the refusal names the
    // field it belongs to.
    let mut control = probe();
    let count = control.get_field("count").expect("count reads");
    assert_eq!(count, "2");
    for (name, value, expected) in [
        ("count", "-1", "below minimum"),
        ("label", "far too long", "exceeds maximum"),
    ] {
        let error = control
            .set_field(name, value)
            .expect_err("the field's contract refuses");
        let refusal = format!("{name}: {error}");
        assert!(
            refusal.contains(expected),
            "the refusal explains itself: {refusal}"
        );
    }
    assert_eq!(
        control,
        probe(),
        "a refused write leaves the value exactly as it was"
    );
}

#[test]
fn a_number_the_editor_cannot_hold_is_reported_rather_than_repaired() {
    // `span` holds a real value wider than the editor's signed range. The
    // editor reports and paints nothing rather than narrowing it to zero
    // or wrapping it — a control that quietly showed a different number
    // than it holds is the failure this refuses.
    let mut control = WideProbe { span: u64::MAX };
    let fields = schema_fields(&control.describe());
    egui::__run_test_ui(|ui| {
        let edits = paint_schema_fields(ui, &fields, &mut control);
        assert!(
            edits.written.is_empty(),
            "nothing is written from an unreadable field"
        );
        assert_eq!(
            edits.refusals.len(),
            1,
            "exactly the unreadable field refuses: {:?}",
            edits.refusals
        );
        assert!(
            edits.refusals[0].starts_with("span: ")
                && edits.refusals[0].contains("not an editable integer"),
            "the unreadable field names itself and why: {:?}",
            edits.refusals
        );
    });
    assert_eq!(
        control.span,
        u64::MAX,
        "the field still holds exactly what it held"
    );
}

#[test]
fn a_writable_whole_number_still_reaches_its_field() {
    // The refusal above is not a blanket "no integers work": an in-range
    // field is editable, and the write lands through the same path.
    let mut control = WideProbe { span: 4 };
    let fields = schema_fields(&control.describe());
    control
        .set_field("span", "9")
        .expect("an in-range whole number is accepted");
    egui::__run_test_ui(|ui| {
        let edits = paint_schema_fields(ui, &fields, &mut control);
        assert!(
            edits.written.is_empty(),
            "an untouched frame writes nothing"
        );
        assert!(
            edits.refusals.is_empty(),
            "an editable field refuses nothing"
        );
    });
    assert_eq!(control.span, 9);
}

#[test]
fn the_editor_passes_replays_own_schema_shape() {
    // The real control this module exists for: the replay cursor is a
    // `Component` with a schema, and the editor lifts it without knowing
    // the field names.
    let state = crate::ReplayState::new()
        .cursor(3)
        .total_bars(8)
        .mark("live".to_owned())
        .build();
    let fields = schema_fields(&state.describe());
    let names: Vec<&str> = fields.iter().map(|field| field.name.as_str()).collect();
    assert_eq!(names, ["cursor", "mark", "total_bars"]);
    let field = |name: &str| {
        fields
            .iter()
            .find(|field| field.name == name)
            .expect("replay cursor field lifts")
    };
    assert_eq!(field("cursor").kind, FieldKind::Integer);
    assert_eq!(field("cursor").minimum, Some(0.0));
    assert_eq!(field("mark").kind, FieldKind::Text);
    assert_eq!(field("mark").max_len, Some(64));
}
#[test]
fn a_declared_format_rides_the_lifted_field() {
    // A format refines a kind: a decimal is still a string on the wire, but
    // an editor that reads the format can hold it to its exact contract.
    // Unformatted fields carry none, so "no format" and "unknown format"
    // stay distinguishable from a declared one.
    let lifted = schema_fields(&serde_json::json!({"properties": {
        "cap": {"type": "string", "format": "decimal"},
        "when": {"type": "string", "format": "date-time"},
        "plain": {"type": "string"},
    }}));
    let field = |name: &str| {
        lifted
            .iter()
            .find(|field| field.name == name)
            .unwrap_or_else(|| panic!("{name} lifts"))
            .clone()
    };
    assert_eq!(field("cap").kind, FieldKind::Text);
    assert_eq!(field("cap").format.as_deref(), Some("decimal"));
    assert_eq!(field("when").format.as_deref(), Some("date-time"));
    assert_eq!(field("plain").format, None);
}

/// A control spanning all three settings tabs, so the classifier has one of
/// everything to route.
#[derive(Debug, Clone, PartialEq, FieldAccess, Describable)]
struct TabProbe {
    #[field(desc = "Lookback length")]
    length: i64,
    #[field(desc = "Smoothing span")]
    smoothing: f64,
    #[field(desc = "Line color (#RRGGBB)")]
    line_color: String,
    #[field(desc = "Fill transparency")]
    fill_alpha: f64,
    #[field(desc = "Show the line")]
    show_line: bool,
    #[field(desc = "Overlay visibility")]
    visible: bool,
}

fn tab_probe() -> TabProbe {
    TabProbe {
        length: 14,
        smoothing: 0.5,
        line_color: "#20C8F0".to_owned(),
        fill_alpha: 0.8,
        show_line: true,
        visible: true,
    }
}

fn tab_field(name: &str, kind: FieldKind) -> SchemaField {
    SchemaField {
        name: name.to_owned(),
        kind,
        format: None,
        description: String::new(),
        minimum: None,
        maximum: None,
        max_len: None,
        options: Vec::new(),
    }
}

#[test]
fn schema_fields_classify_into_inputs_style_and_visibility_tabs() {
    // Boolean `show_*`/`visible` fields route to Visibility; color, alpha,
    // transparency, brightness, threshold, style, and theme names route to
    // Style; calculation fields (length, period, smoothing, source,
    // timeframe, oscillator) route to Inputs.
    for (name, kind, expected) in [
        ("show_first_ma", FieldKind::Boolean, SettingsTab::Visibility),
        ("show_line", FieldKind::Boolean, SettingsTab::Visibility),
        ("visible", FieldKind::Boolean, SettingsTab::Visibility),
        ("line_color", FieldKind::Text, SettingsTab::Style),
        ("fill_alpha", FieldKind::Number, SettingsTab::Style),
        ("transparency", FieldKind::Number, SettingsTab::Style),
        ("brightness", FieldKind::Number, SettingsTab::Style),
        ("threshold", FieldKind::Number, SettingsTab::Style),
        ("line_style", FieldKind::Text, SettingsTab::Style),
        ("theme", FieldKind::Text, SettingsTab::Style),
        ("length", FieldKind::Integer, SettingsTab::Inputs),
        ("period", FieldKind::Integer, SettingsTab::Inputs),
        ("smoothing", FieldKind::Number, SettingsTab::Inputs),
        ("source", FieldKind::Text, SettingsTab::Inputs),
        ("timeframe", FieldKind::Text, SettingsTab::Inputs),
        ("oscillator_gain", FieldKind::Number, SettingsTab::Inputs),
        ("line_width", FieldKind::Number, SettingsTab::Style),
        ("line_span", FieldKind::Number, SettingsTab::Inputs),
    ] {
        let field = tab_field(name, kind);
        assert_eq!(
            classify_schema_field(&field),
            expected,
            "{name} routes to {expected:?}"
        );
    }
    // The tabs partition the schema losslessly: every field lands in exactly
    // one tab, and the union is the whole schema.
    let fields = schema_fields(&tab_probe().describe());
    let mut union: Vec<String> = Vec::new();
    for tab in SettingsTab::all() {
        let tabbed = fields_for_tab(&fields, tab);
        assert!(!tabbed.is_empty(), "{tab:?} renders at least one editor");
        for field in &tabbed {
            assert_eq!(
                classify_schema_field(field),
                tab,
                "a field rendered under {tab:?} classifies back to it"
            );
        }
        union.extend(tabbed.into_iter().map(|field| field.name));
    }
    let mut lifted: Vec<String> = fields.into_iter().map(|field| field.name).collect();
    union.sort();
    lifted.sort();
    assert_eq!(
        union, lifted,
        "the three tabs partition the schema without loss or overlap"
    );
    assert_eq!(
        SettingsTab::all().map(SettingsTab::label),
        ["Inputs", "Style", "Visibility"],
        "tab labels stay fixed"
    );
}

#[test]
fn paint_tabbed_schema_fields_switches_tabs_and_writes_through_field_access() {
    let mut control = tab_probe();
    let fields = schema_fields(&control.describe());
    // Every tab renders headlessly with no refusals and no phantom writes,
    // and rendering never moves the tab out from under the caller.
    for tab in SettingsTab::all() {
        let mut active = tab;
        egui::__run_test_ui(|ui| {
            let edits = paint_tabbed_schema_fields(ui, &mut active, &fields, &[], &mut control);
            assert_eq!(
                edits,
                FieldEdits::default(),
                "an untouched frame changes nothing"
            );
            assert_eq!(active, tab, "rendering never moves the active tab");
        });
    }
    // Switching tabs changes which fields render: the Visibility tab holds
    // exactly the two toggles, the Style tab the two paint knobs.
    let names = |tab: SettingsTab| {
        let mut names: Vec<String> = fields_for_tab(&fields, tab)
            .into_iter()
            .map(|field| field.name)
            .collect();
        names.sort();
        names
    };
    assert_eq!(names(SettingsTab::Visibility), vec!["show_line", "visible"]);
    assert_eq!(names(SettingsTab::Style), vec!["fill_alpha", "line_color"]);
    // Writes flow through the same `FieldAccess` contract the single-tab
    // editor uses, on every tab.
    control
        .set_field("length", "21")
        .expect("inputs write through");
    control
        .set_field("line_color", "#B42020")
        .expect("style writes through");
    control
        .set_field("show_line", "false")
        .expect("visibility writes through");
    assert_eq!(control.length, 21);
    assert_eq!(control.line_color, "#B42020");
    assert!(!control.show_line);
}
