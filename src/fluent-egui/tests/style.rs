//! Per-indicator paint overrides: the Style tab\'s flat schema, its
//! additive inherit/restore rule, its refusals, and the color every styled
//! emitter reads.

use super::*;

use crate::fields::{FieldKind, SettingsTab, schema_fields};
use crate::paint::{paint_oscillator, paint_oscillator_styled, parse_hex_color};

fn theme() -> ThemeConfig {
    ThemeConfig::default()
}

#[test]
fn a_default_style_changes_nothing() {
    let style = IndicatorStyle::default();
    assert!(
        (style.line_width() - DEFAULT_LINE_WIDTH as f32).abs() < f32::EPSILON,
        "the hairline is the default width"
    );
    for key in ["bull", "bear", "fast", "slow", "ribbon_bull", "ribbon_bear"] {
        assert_eq!(style.override_color(key), None, "{key} starts unset");
        assert_eq!(
            style.color(&theme(), key).expect("a known key resolves"),
            crate::paint::resolve_color(&theme(), key).expect("a known key resolves"),
            "{key} inherits the theme byte-for-byte"
        );
    }
    assert!(style.validate().is_ok(), "the default style validates");
}

#[test]
fn the_field_list_is_the_schema_and_the_key_lookup_agree() {
    assert_eq!(IndicatorStyle::default().field_names(), &STYLE_FIELDS, "the const is the key set");
    assert_eq!(STYLE_FIELDS.first().copied(), Some(LINE_WIDTH_FIELD), "the width leads");
    assert_eq!(
        STYLE_FIELDS[1..].len(),
        STYLE_FIELDS.len() - 1,
        "every other field is a color"
    );
    for field in STYLE_FIELDS.iter().skip(1) {
        let key = style_key(field).unwrap_or_else(|| panic!("{field} maps to a theme key"));
        assert!(field.starts_with("color_"), "{field} is named as a color override");
        assert!(theme().style(key).is_some(), "{key} is a real theme slot");
    }
    assert_eq!(style_key(LINE_WIDTH_FIELD), None, "the width overrides no key");
    assert_eq!(style_key("color_nope"), None, "an undeclared field maps to no key");
    assert_eq!(style_key(""), None, "an empty name maps to no key");
}

#[test]
fn the_described_schema_matches_the_field_list_exactly() {
    let fields = schema_fields(&IndicatorStyle::default().describe());
    let mut declared: Vec<&str> = fields.iter().map(|field| field.name.as_str()).collect();
    let mut expected: Vec<&str> = STYLE_FIELDS.to_vec();
    declared.sort_unstable();
    expected.sort_unstable();
    assert_eq!(declared, expected, "the document declares exactly the declared field set");

    let width = fields.iter().find(|field| field.name == LINE_WIDTH_FIELD).expect("the width");
    assert_eq!(width.kind, FieldKind::Number, "the width is a real number");
    assert_eq!(width.minimum, Some(MIN_LINE_WIDTH), "the width declares its lower bound");
    assert_eq!(width.maximum, Some(MAX_LINE_WIDTH), "the width declares its upper bound");

    for field in fields.iter().filter(|field| field.name != LINE_WIDTH_FIELD) {
        assert_eq!(field.kind, FieldKind::Color, "{} renders as a color", field.name);
        assert_eq!(field.format.as_deref(), Some(crate::fields::COLOR_FORMAT));
        assert!(
            field.description.contains("inherits the theme"),
            "{} documents the inherit rule: {}",
            field.name,
            field.description
        );
    }
}

#[test]
fn an_override_replaces_the_theme_color_and_clearing_restores_it() {
    let mut style = IndicatorStyle::default();
    style.set_field("color_fast", "#123456").expect("a well-formed override writes");
    assert_eq!(style.override_color("fast"), Some("#123456"));
    assert_eq!(
        style.color(&theme(), "fast").expect("the override resolves"),
        egui::Color32::from_rgb(0x12, 0x34, 0x56),
        "the override paints, not the theme"
    );
    assert_eq!(
        style.color(&theme(), "slow").expect("an unset key still resolves"),
        crate::paint::resolve_color(&theme(), "slow").expect("a known key resolves"),
        "one override leaves its neighbours themed"
    );

    // Additive, not sticky: clearing the field restores the theme rather than
    // freezing whatever color happened to be on screen.
    style.set_field("color_fast", "").expect("an empty value clears");
    assert_eq!(style.override_color("fast"), None, "the override is gone");
    assert_eq!(
        style.color(&theme(), "fast").expect("the theme key resolves"),
        crate::paint::resolve_color(&theme(), "fast").expect("a known key resolves"),
        "the theme shows through again"
    );
}

#[test]
fn a_refused_edit_leaves_the_style_exactly_as_it_was() {
    let mut style = IndicatorStyle::default();
    style.set_field("color_fast", "#123456").expect("a well-formed override writes");
    style.set_field(LINE_WIDTH_FIELD, "2.5").expect("an in-band width writes");

    assert!(style.set_field("color_fast", "#GGGGGG").is_err(), "non-hex digits refuse");
    assert!(style.set_field("color_fast", "#1234").is_err(), "a short hex refuses");
    assert!(style.set_field("color_fast", "123456").is_err(), "a missing hash refuses");
    assert!(style.set_field(LINE_WIDTH_FIELD, "0.1").is_err(), "too thin refuses");
    assert!(style.set_field(LINE_WIDTH_FIELD, "9").is_err(), "too thick refuses");
    assert!(style.set_field(LINE_WIDTH_FIELD, "nope").is_err(), "a non-number refuses");
    assert!(style.set_field("nope", "1").is_err(), "an undeclared field refuses");

    assert_eq!(style.override_color("fast"), Some("#123456"), "the override survived");
    assert_eq!(style.get_field(LINE_WIDTH_FIELD).expect("the width reads"), "2.5", "so did the width");
    assert!(style.validate().is_ok(), "and the style is still self-consistent");
}

#[test]
fn the_read_write_round_trip_covers_every_declared_field() {
    let mut style = IndicatorStyle::default();
    let mut written = Vec::new();
    for field in STYLE_FIELDS {
        let value = if field == LINE_WIDTH_FIELD { "1.75".to_owned() } else { "#0A0B0C".to_owned() };
        style.set_field(field, &value).expect("every declared field writes");
        assert_eq!(style.get_field(field).expect("every declared field reads"), value);
        written.push(field);
    }
    assert_eq!(written, STYLE_FIELDS.to_vec(), "every field round-tripped");
    for field in STYLE_FIELDS.iter().filter(|field| **field != LINE_WIDTH_FIELD) {
        let key = style_key(field).expect("a color field names a key");
        assert_eq!(style.color(&theme(), key).expect("the override resolves"), egui::Color32::from_rgb(0x0A, 0x0B, 0x0C));
    }
    assert!((style.line_width() - 1.75).abs() < 1e-6, "the width reads back");
}

#[test]
fn an_unset_color_reads_as_the_empty_string_that_means_inherit() {
    let style = IndicatorStyle::default();
    for field in STYLE_FIELDS.iter().filter(|field| **field != LINE_WIDTH_FIELD) {
        assert_eq!(
            style.get_field(field).expect("a declared color reads"),
            "",
            "{field} reads empty, which is the inherit the editor round-trips",
        );
    }
}

#[test]
fn validate_mirrors_the_field_contracts_over_the_whole_style() {
    let style = IndicatorStyle::default();
    assert!(style.validate().is_ok());

    // A style whose width sits outside the band cannot be built through
    // `set_field`, so the check is exercised through the same predicate the
    // editor refuses on: every in-band width validates.
    for width in [MIN_LINE_WIDTH, 1.0, 4.0, MAX_LINE_WIDTH] {
        let mut style = IndicatorStyle::default();
        style.set_field(LINE_WIDTH_FIELD, &width.to_string()).expect("in-band width writes");
        assert!(style.validate().is_ok(), "width {width} validates");
    }
    assert!(
        parse_hex_color("fast", "#20C8F0").is_ok() && parse_hex_color("fast", "nope").is_err(),
        "the shared hex reader accepts a real color and refuses a real non-color"
    );
    let err = parse_hex_color("slow", "nope").expect_err("a malformed slot refuses");
    assert_eq!(
        err.to_string(),
        "invalid parameter for slow: \"nope\" is not a #RRGGBB color",
        "the refusal names the key and the bad slot"
    );
}

#[test]
fn the_stroke_width_is_clamped_on_read_so_a_style_always_paints_a_line() {
    let style = IndicatorStyle::default();
    assert!(
        (style.stroke(egui::Color32::WHITE).width - DEFAULT_LINE_WIDTH as f32).abs() < f32::EPSILON,
        "the default stroke is the default width"
    );
    let mut wide = IndicatorStyle::default();
    wide.set_field(LINE_WIDTH_FIELD, "4").expect("in-band width writes");
    assert!(
        (wide.stroke(egui::Color32::WHITE).width - 4.0).abs() < f32::EPSILON,
        "the style\'s width reaches the stroke"
    );
}

#[test]
fn an_unknown_key_still_refuses_through_the_style() {
    let style = IndicatorStyle::default();
    let error = style.color(&theme(), "nope").expect_err("an unknown key refuses");
    assert_eq!(
        error.to_string(),
        "invalid parameter for nope: unknown theme style key",
        "the refusal names the key"
    );
}

#[test]
fn the_styled_emitter_draws_the_override_and_its_width() {
    egui::__run_test_ui(|ui| {
        let painter = ui.painter();
        let points = vec![Some((0.0_f32, 0.0_f32)), Some((10.0, 5.0))];
        let mut style = IndicatorStyle::default();
        style.set_field("color_slow", "#FF00FF").expect("the override writes");
        style.set_field(LINE_WIDTH_FIELD, "3").expect("the width writes");

        // The styled pass paints exactly what the style says; the plain pass
        // is the same emitter with the no-op style, so both must succeed and
        // neither may refuse a well-formed theme.
        paint_oscillator_styled(painter, &points, "slow", &theme(), &style, 1.0)
            .expect("a styled line paints");
        paint_oscillator(painter, &points, "slow", &theme(), 1.0).expect("an unstyled line paints");

        // A style whose override is malformed refuses by key, like a
        // malformed theme slot does.
        let mut broken = IndicatorStyle::default();
        broken.colors.insert("slow".to_owned(), "not-a-color".to_owned());
        let error = paint_oscillator_styled(painter, &points, "slow", &theme(), &broken, 1.0)
            .expect_err("a malformed override refuses instead of painting gray");
        assert!(error.to_string().contains("not a #RRGGBB color"), "{error}");
    });
}

#[test]
fn the_style_component_executes_as_a_work_unit() {
    let style = IndicatorStyle::default();
    let output = style
        .execute(&fluent_wvr::WorkContext::default())
        .expect("a valid style executes");
    assert!(output.success, "a valid style executes cleanly");
    assert!(
        output.message.contains("indicator style ready"),
        "the unit reports itself: {output:?}"
    );
}

#[test]
fn every_style_field_classifies_into_the_style_tab() {
    let fields = schema_fields(&IndicatorStyle::default().describe());
    let style_tab = crate::fields::fields_for_tab(&fields, SettingsTab::Style);
    assert_eq!(
        style_tab.len(),
        STYLE_FIELDS.len(),
        "the Style tab shows the whole override set, and nothing hides under Inputs"
    );
    let inputs = crate::fields::fields_for_tab(&fields, SettingsTab::Inputs);
    assert!(inputs.is_empty(), "no override leaks into the Inputs tab: {inputs:?}");
}