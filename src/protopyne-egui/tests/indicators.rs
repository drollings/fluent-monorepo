//! Indicator plugin system: the searchable indicator catalog, the
//! `ChartIndicator` plugin lifecycle, and per-slot settings isolation.
//!
//! Every plugin evaluates through the shared kernel registry (or the frozen
//! IntegrationLite/FastMas modules), so no calculation is re-implemented here.
//! The tests pin the catalog's category partition and filtering, the plugin
//! evaluation into the pane cache, live readouts, and the isolation the hot
//! slot stack promises across plugins.

use super::*;

use fluent_egui::{HotSlotStack, IndicatorStyle, PaneCache, PanePlan, SlotAttachment};
use protopyne_chart_scale::PriceScaleMode;

use crate::fast_mas::FastMasConfig;
use crate::integration_lite::IntegrationLiteConfig;
use crate::topology::SeriesCatalog;

/// Sixty rising bars with a distinct volume leg, so every MA and volume
/// kernel has data to weight.
fn ramp_rows() -> Vec<ChartRow> {
    (0..60)
        .map(|i| {
            let close = 100.0 + i as f64;
            ChartRow {
                time: i,
                open: close - 0.25,
                high: close + 0.5,
                low: close - 0.5,
                close,
                volume: 1_000 + 10 * i as u64,
            }
        })
        .collect()
}

fn test_painter() -> (egui::Context, egui::Painter) {
    let ctx = egui::Context::default();
    ctx.begin_pass(egui::RawInput::default());
    let painter =
        egui::Painter::new(ctx.clone(), egui::LayerId::background(), egui::Rect::EVERYTHING);
    (ctx, painter)
}

fn end_frame(ctx: &egui::Context) -> egui::FullOutput {
    let mut output = ctx.end_pass();
    output.textures_delta.clear();
    output
}

#[test]
fn indicator_catalog_populates_my_scripts_technical_financial_and_favorites() {
    let mut catalog = build_indicator_catalog().expect("the catalog builds");

    // My Scripts holds the two shipped script indicators.
    for (id, title) in [("integration_lite", "Integration Lite"), ("fast_mas", "Fast MAs")] {
        let entry = catalog.get(id).expect("script indicator registers");
        assert_eq!(entry.title(), title);
        assert_eq!(entry.category(), &IndicatorCategory::MyScripts);
    }

    // Technical holds every builtin kernel, exactly.
    let registry = builtin_registry().expect("registry builds");
    assert_eq!(registry.len(), 56, "the registry ships 56 kernels");
    catalog.set_category(Some(IndicatorCategory::Technical));
    let technical: Vec<String> =
        catalog.filtered().into_iter().map(|entry| entry.id().to_owned()).collect();
    assert_eq!(technical.len(), registry.len(), "every kernel is under Technical");
    for id in registry.ids() {
        assert!(technical.contains(&id), "kernel {id} is under Technical");
    }

    // Financial holds the domain monitors, which are not kernels.
    catalog.set_category(Some(IndicatorCategory::Financial));
    let financial: Vec<String> =
        catalog.filtered().into_iter().map(|entry| entry.id().to_owned()).collect();
    for id in ["regime", "health"] {
        assert!(financial.contains(&id.to_owned()), "monitor {id} is under Financial");
        assert!(!registry.ids().contains(&id.to_owned()), "monitor {id} is not a kernel");
    }

    // Query filtering narrows within a category; empty query is the recall floor.
    catalog.set_category(Some(IndicatorCategory::Technical));
    catalog.set_query("sma");
    let sma: Vec<String> =
        catalog.filtered().into_iter().map(|entry| entry.id().to_owned()).collect();
    assert_eq!(sma, vec!["sma".to_owned()], "only the sma kernel matches the query");
    catalog.set_query("");
    assert_eq!(catalog.filtered().len(), registry.len(), "the empty query matches every kernel");

    // Favorites filter composes with the category.
    catalog.set_category(None);
    assert!(catalog.toggle_favorite("fast_mas").expect("favorite toggles"));
    catalog.set_favorites_only(true);
    let favorites: Vec<String> =
        catalog.filtered().into_iter().map(|entry| entry.id().to_owned()).collect();
    assert_eq!(favorites, vec!["fast_mas".to_owned()], "only the starred indicator shows");
}

#[test]
fn chart_indicator_plugins_evaluate_into_cache_and_isolate_settings() {
    let rows = ramp_rows();
    let mut plan = PanePlan::new();
    let mut stack = HotSlotStack::new();
    let mut cache = PaneCache::new();
    let mut published = 0_usize;

    for (fingerprint, id) in
        [(1_u64, "integration_lite"), (2, "fast_mas"), (3, "sma"), (4, "rsi")]
    {
        let plugin: Box<dyn ChartIndicator> =
            instantiate_indicator(id).expect("the plugin instantiates");
        let config = plugin.default_config();
        let nodes = plugin.plan_nodes(&*config).expect("nodes list");
        let node_refs: Vec<&str> = nodes.iter().map(String::as_str).collect();
        stack
            .insert(
                plugin.id(),
                plugin.title(),
                plugin.attachment(),
                config,
                &node_refs,
                &[],
                fingerprint,
                &mut plan,
            )
            .expect("the plugin slots in");

        // Evaluate against the slot's own config (the settings authority).
        let evaluation: IndicatorEvaluation = {
            let config = stack.config(plugin.id()).expect("the slot owns its config");
            plugin.evaluate(&rows, config, &SeriesCatalog::empty()).expect("the plugin evaluates")
        };
        assert!(!evaluation.series.is_empty(), "{id} produces series");
        for series in &evaluation.series {
            let series: &IndicatorSeries = series;
            assert_eq!(series.values.len(), rows.len(), "{} aligns with the rows", series.node);
            cache.publish(series.key, series.values.clone());
            assert!(cache.get(series.key).is_some(), "{} publishes into the cache", series.node);
            published += 1;
        }

        let readouts = plugin.readouts(&evaluation);
        assert!(!readouts.is_empty(), "{id} yields readouts");
        assert!(
            readouts.iter().all(|readout| readout.value.is_some()),
            "{id} readouts are live on a full ramp"
        );
        stack.set_readouts(plugin.id(), readouts).expect("readouts set");
    }

    assert_eq!(stack.len(), 4, "every plugin is slotted");
    assert_eq!(published, 11, "4 + 5 + 1 + 1 series publish");
    assert_eq!(cache.len(), published, "each series has its own cache key");

    // The eye toggles one slot instantly and leaves the others alone.
    stack.toggle_visibility("fast_mas").expect("the eye hides");
    assert!(!stack.visible("fast_mas").expect("reads"));
    assert!(stack.visible("integration_lite").expect("reads"), "the other slot stays shown");
    stack.toggle_visibility("fast_mas").expect("the eye shows");
    assert!(stack.visible("fast_mas").expect("reads"));

    // Editing one slot's schema never reaches another slot.
    let lite_default = IntegrationLiteConfig::default();
    stack.set_slot_field("fast_mas", "first_ma_length", "9").expect("the field edits");
    assert_eq!(stack.get_slot_field("fast_mas", "first_ma_length").expect("reads"), "9");
    assert_eq!(
        stack.get_slot_field("integration_lite", "eit_alpha").expect("reads"),
        lite_default.eit_alpha.to_string(),
        "the other slot keeps its own value"
    );
    assert!(
        stack.set_slot_field("fast_mas", "not_a_field", "1").is_err(),
        "an unknown field refuses through the slot's own contract"
    );

    // The plugins are directly constructible and named.
    assert_eq!(IntegrationLiteIndicator::new().id(), "integration_lite");
    assert_eq!(FastMasIndicator::new().id(), "fast_mas");
    let kernel = KernelIndicator::new("sma").expect("a kernel plugin builds");
    assert_eq!(kernel.id(), "sma");
    assert_eq!(kernel.title(), "Simple Moving Average", "titles come from the kernel spec");
    assert_eq!(kernel.attachment(), SlotAttachment::MainOverlay, "single smoothings overlay");
    let kernel_config = KernelIndicatorConfig::new().kernel("sma".to_owned()).length(10).build();
    assert!(kernel_config.validate().is_ok(), "a valid kernel config passes");

    // Unknown ids refuse; the monitor ids instantiate.
    assert!(instantiate_indicator("not-an-indicator").is_err(), "unknown ids refuse");
    assert!(instantiate_indicator("").is_err(), "blank ids refuse");
    assert_eq!(instantiate_indicator("regime").expect("monitor builds").id(), "regime");
}

#[test]
fn chart_indicator_plugins_paint_their_attachments_headlessly() {
    let rows = ramp_rows();
    let theme = ThemeConfig::default();
    let sub_pane = Viewport::new(PriceScaleMode::Normal, -110.0, 110.0, 200.0, 8.0, 90.0, false)
        .expect("bounded pane");
    let overlay = Viewport::new(PriceScaleMode::Normal, 90.0, 170.0, 200.0, 8.0, 90.0, false)
        .expect("price pane");

    let (ctx, painter) = test_painter();
    let lite = IntegrationLiteIndicator::new();
    let lite_config = lite.default_config();
    let lite_view = lite.evaluate(&rows, &*lite_config, &SeriesCatalog::empty()).expect("lite evaluates");
    lite.paint_sub_pane(
        &painter,
        &lite_view,
        &sub_pane,
        egui::Pos2::ZERO,
        0,
        640.0,
        &theme,
        &*lite_config,
        &IndicatorStyle::default(),
    )
    .expect("the sub-pane paints");

    let fast = FastMasIndicator::new();
    let fast_config = fast.default_config();
    let fast_view = fast.evaluate(&rows, &*fast_config, &SeriesCatalog::empty()).expect("fast mas evaluates");
    fast.paint_main_overlay(
        &painter,
        &fast_view,
        &overlay,
        egui::Pos2::ZERO,
        0,
        640.0,
        &theme,
        &*fast_config,
        &IndicatorStyle::default(),
    )
    .expect("the overlay paints");

    assert!(!end_frame(&ctx).shapes.is_empty(), "the plugins emit geometry");
}

/// The catalog's `All`/`Favorites` tabs are domain selectors, not entry
/// categories; the two script indicators carry their own distinct schemas.
#[test]
fn script_indicators_keep_distinct_component_schemas() {
    let fast = FastMasConfig::new().build();
    let lite = IntegrationLiteConfig::new().build();
    for name in fast.field_names() {
        assert!(!lite.field_names().contains(name), "{name} is fast-MAs-only");
    }
    assert_eq!(IndicatorCategory::all().len(), 5, "five domain tabs");
    assert_eq!(IndicatorCategory::All.label(), "All");
    assert_eq!(IndicatorCategory::Favorites.label(), "Favorites");
}
