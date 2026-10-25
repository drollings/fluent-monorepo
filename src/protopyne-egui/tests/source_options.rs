use super::*;
use fluent_egui::{FieldKind, SettingsTab};
use fluent_wvr::Describable;

fn rows() -> Vec<ChartRow> {
    (0..60)
        .map(|i| {
            let close = 100.0 + f64::from(i);
            ChartRow {
                time: i64::from(i),
                open: close - 0.5,
                high: close + 0.5,
                low: close - 1.0,
                close,
                volume: 10,
            }
        })
        .collect()
}

fn source_field(workspace: &ChartWorkspace, slot: &str) -> fluent_egui::SchemaField {
    let (config_fields, _) = workspace.settings_fields(slot).expect("settings lift");
    config_fields
        .into_iter()
        .find(|field| field.name == "source")
        .expect("source lifts")
}

#[test]
fn source_dropdown_lists_price_sources_and_live_producer_nodes() {
    let mut workspace = ChartWorkspace::with_rows(rows()).expect("workspace boots");
    workspace.slot_indicator("ema").expect("producer slots");
    workspace.slot_indicator("sma").expect("consumer slots");
    workspace.recompute().expect("evaluations build");
    for slot in ["sma", "ema"] {
        let field = source_field(&workspace, slot);
        assert_eq!(field.kind, FieldKind::Select, "{slot} source is a dropdown");
        assert!(
            field.options.contains(&"close".to_owned()),
            "{slot} offers price sources: {:?}",
            field.options
        );
        assert!(
            field.options.contains(&"series:kernel:ema".to_owned()),
            "{slot} offers live producer nodes: {:?}",
            field.options
        );
    }
}

#[test]
fn fast_mas_source_matches_the_kernel_dropdown_standard() {
    let mut workspace = ChartWorkspace::with_rows(rows()).expect("workspace boots");
    workspace.slot_indicator("ema").expect("producer slots");
    workspace
        .slot_indicator("fast_mas")
        .expect("fast max slots");
    workspace.recompute().expect("evaluations build");
    let field = source_field(&workspace, "fast_mas");
    assert_eq!(
        field.kind,
        FieldKind::Select,
        "fast_mas source is a dropdown"
    );
    assert!(
        field.options.contains(&"close".to_owned()),
        "fast_mas offers price sources: {:?}",
        field.options
    );
    assert!(
        field.options.contains(&"series:kernel:ema".to_owned()),
        "fast_mas offers live producer nodes: {:?}",
        field.options
    );
}

#[test]
fn integration_lite_offers_a_price_source_choice_without_producer_edges() {
    // The bar-reading composite takes no `series:` producer edge, but its
    // arms do read one selectable price column: the picker lists exactly
    // the price-source catalog, so a producer spelling never reaches it.
    let lite = crate::integration_lite::IntegrationLiteConfig::default().describe();
    let properties = lite
        .get("properties")
        .expect("object properties")
        .as_object()
        .expect("object");
    let source = properties
        .get("price_source")
        .expect("the price-source choice");
    assert_eq!(source.get("type").and_then(|v| v.as_str()), Some("string"));
    let options: Vec<&str> = source
        .get("enum")
        .and_then(|v| v.as_array())
        .expect("catalog options")
        .iter()
        .map(|v| v.as_str().expect("string option"))
        .collect();
    assert_eq!(
        options,
        crate::topology::price_source_ids(),
        "the picker lists the catalog without re-spelling it"
    );
    assert!(
        options.iter().all(|option| !option.starts_with("series:")),
        "no producer spelling reaches the bar-reading composite: {options:?}"
    );
}

#[test]
fn style_names_paint_keys_and_never_resolves_input() {
    let mut workspace = ChartWorkspace::with_rows(rows()).expect("workspace boots");
    workspace.slot_indicator("rsi").expect("rsi slots");
    workspace.recompute().expect("evaluations build");
    let before = workspace
        .evaluations
        .get("rsi")
        .expect("rsi evaluates")
        .clone();
    workspace
        .set_slot_field("rsi", "style", "slow")
        .expect("style edits");
    let after = workspace
        .evaluations
        .get("rsi")
        .expect("rsi evaluates")
        .clone();
    assert_eq!(
        before.series.first().expect("one").node,
        after.series.first().expect("one").node,
        "the plan node is untouched by a style edit"
    );
    assert_eq!(
        before.series.first().expect("one").values,
        after.series.first().expect("one").values,
        "raw values are untouched by a style edit"
    );
    assert_ne!(
        before.series.first().expect("one").key,
        after.series.first().expect("one").key,
        "the cache key follows the fingerprinted style setting"
    );
    assert_ne!(
        before.series.first().expect("one").style,
        after.series.first().expect("one").style,
        "only the paint key moves: {before:?} vs {after:?}"
    );
}

#[test]
fn style_tab_assignments_are_unchanged_by_the_source_merge() {
    let mut workspace = ChartWorkspace::with_rows(rows()).expect("workspace boots");
    workspace.slot_indicator("rsi").expect("rsi slots");
    workspace.recompute().expect("evaluations build");
    let lifted = workspace.settings_fields("rsi").expect("settings lift");
    let style_names: Vec<String> = fluent_egui::fields_for_tab(&lifted.0, SettingsTab::Style)
        .iter()
        .map(|field| field.name.clone())
        .collect();
    for dial in [
        "normalize",
        "osc_gain",
        "osc_trim",
        "osc_threshold",
        "style",
    ] {
        assert!(
            style_names.iter().any(|name| name == dial),
            "{dial} stays on the Style tab: {style_names:?}"
        );
    }
    assert!(
        !style_names.iter().any(|name| name == "source"),
        "source stays on Inputs: {style_names:?}"
    );
}
