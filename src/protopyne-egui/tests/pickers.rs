//! Catalog-backed pickers: smoothing and oscillator options derive from the
//! M2/M3 catalogs, schemas expose them as selectors, and unknown names
//! refuse with a migration message.

use super::*;
use fluent_egui::{IndicatorStyle, Viewport};
use protopyne_chart_model::ChartRow;
use protopyne_chart_scale::PriceScaleMode;
use protopyne_kernels::smoothing::{length_smoothing_ids, smoothing_options, SmoothingFamily};

use crate::chart::ChartConfig;
use crate::fast_mas::FastMasConfig;
use crate::integration_lite::IntegrationLiteConfig;
use crate::oscillators::{oscillator_options, OscillatorKind};
use crate::topology::SeriesCatalog;

/// Sixty rising bars with a distinct volume leg.
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
    let painter = egui::Painter::new(
        ctx.clone(),
        egui::LayerId::background(),
        egui::Rect::EVERYTHING,
    );
    (ctx, painter)
}

fn end_frame(ctx: &egui::Context) -> egui::FullOutput {
    let mut output = ctx.end_pass();
    output.textures_delta.clear();
    output
}

#[test]
fn smoothing_picker_lists_exactly_the_m2_families() {
    let options = smoothing_options();
    let family_ids: Vec<&str> = SmoothingFamily::all()
        .iter()
        .copied()
        .map(SmoothingFamily::id)
        .collect();
    let option_ids: Vec<&str> = options.iter().map(|option| option.id.as_str()).collect();
    assert_eq!(
        option_ids, family_ids,
        "the picker is the catalog, in catalog order"
    );
    for (option, family) in options.iter().zip(SmoothingFamily::all()) {
        assert_eq!(option.label, family.label(), "labels ride along");
        assert_eq!(
            option.takes_length,
            family.takes_length(),
            "length eligibility rides along"
        );
    }
    let length_ids = length_smoothing_ids();
    assert!(
        !length_ids.iter().any(|id| *id == "eit"),
        "length-only arms exclude alpha-driven families: {length_ids:?}"
    );
    assert_eq!(
        length_ids.len(),
        SmoothingFamily::all()
            .iter()
            .filter(|family| family.takes_length())
            .count(),
        "the schema list is exactly what validates"
    );
}

#[test]
fn oscillator_picker_lists_exactly_the_m3_kinds_with_legs() {
    let options = oscillator_options();
    let kind_ids: Vec<&str> = OscillatorKind::all()
        .iter()
        .copied()
        .map(OscillatorKind::id)
        .collect();
    let option_ids: Vec<&str> = options.iter().map(|option| option.id.as_str()).collect();
    assert_eq!(
        option_ids, kind_ids,
        "the picker is the catalog, in catalog order"
    );
    for (option, kind) in options.iter().zip(OscillatorKind::all()) {
        let legs: Vec<i64> = kind.which_legs().iter().map(|leg| leg.which()).collect();
        assert_eq!(option.legs, legs, "{} carries its which legs", option.id);
        assert_eq!(
            option.evaluable,
            kind.is_evaluable(),
            "{} carries evaluability",
            option.id
        );
    }
    let cdv = options
        .iter()
        .find(|option| option.id == "cdv")
        .expect("cdv lists");
    assert!(!cdv.evaluable, "cdv lists but never evaluates");
    assert!(cdv.refusal.is_some(), "cdv carries its reason");
}

#[test]
fn unknown_names_refuse_with_a_migration_message() {
    let fast = FastMasConfig::new()
        .first_ma_type("hull99".to_owned())
        .build();
    let err = fast.validate().expect_err("an unlisted smoothing refuses");
    assert!(
        err.to_string().contains("catalog"),
        "the refusal names the catalog: {err}"
    );
    let kernel = KernelIndicatorConfig::new()
        .kernel("hull99".to_owned())
        .build();
    let err = kernel.validate().expect_err("an unlisted kernel refuses");
    assert!(
        err.to_string().contains("catalog"),
        "the refusal names the catalog: {err}"
    );
    let chart = ChartConfig {
        fast_oscillator: "nope".to_owned(),
        ..ChartConfig::default()
    };
    let err = chart
        .validate()
        .expect_err("an unlisted oscillator refuses");
    assert!(
        err.to_string().contains("catalog"),
        "the refusal names the catalog: {err}"
    );
}

#[test]
fn settings_schemas_expose_catalog_selectors() {
    use fluent_wvr::Describable;
    let fast = FastMasConfig::new().build().describe();
    let properties = fast.get("properties").expect("object properties");
    for field in [
        "first_ma_type",
        "second_ma_type",
        "signal_ma_type",
        "mean_ma_type",
        "trend_ma_type",
        "base_ma_type",
    ] {
        let options = properties
            .get(field)
            .unwrap_or_else(|| panic!("{field} is described"))
            .get("enum")
            .unwrap_or_else(|| panic!("{field} is a selector"));
        let listed: Vec<&str> = options
            .as_array()
            .expect("string options")
            .iter()
            .map(|option| option.as_str().expect("string option"))
            .collect();
        assert_eq!(
            listed,
            length_smoothing_ids(),
            "{field} lists what validates"
        );
    }
    let kernel = KernelIndicatorConfig::new().build().describe();
    let properties = kernel
        .get("properties")
        .expect("object properties")
        .as_object()
        .expect("object");
    assert!(
        properties.get("kernel").is_none(),
        "the kernel is chosen in the catalog, never offered as a settings choice"
    );
    assert!(
        properties.get("source").is_some(),
        "the input selector stays"
    );
    let lite = IntegrationLiteConfig::default().describe();
    let lite_properties = lite
        .get("properties")
        .expect("object properties")
        .as_object()
        .expect("object");
    let lite_selectors: Vec<&str> = lite_properties
        .iter()
        .filter_map(|(name, hint)| hint.get("enum").is_some().then_some(name.as_str()))
        .collect();
    assert_eq!(
        lite_selectors,
        vec![
            "fast_kind",
            "fast_smooth_type",
            "slow_kind",
            "slow_smooth_type"
        ],
        "only the leg-kind and leg-smoothing dropdowns select"
    );
    for (name, config) in [
        ("fast_mas", FastMasConfig::new().build().describe()),
        ("kernel", KernelIndicatorConfig::new().build().describe()),
        ("lite", IntegrationLiteConfig::default().describe()),
    ] {
        let names: Vec<&str> = config
            .get("properties")
            .expect("object properties")
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        assert!(!names.is_empty(), "{name} describes fields");
    }
}

#[test]
fn catalog_round_trip_without_hardcoded_ids() {
    let rows = ramp_rows();
    let theme = ThemeConfig::default();
    let overlay = Viewport::new(PriceScaleMode::Normal, 90.0, 170.0, 200.0, 8.0, 90.0, false)
        .expect("price pane");
    let (ctx, painter) = test_painter();
    let smoothing = smoothing_options()
        .into_iter()
        .find(|option| option.takes_length && option.kernel.is_some())
        .expect("a kernel-backed smoothing lists");
    let plugin = KernelIndicator::new(&smoothing.id).expect("the picked smoothing instantiates");
    assert_ne!(plugin.title(), smoothing.id, "titles never echo raw ids");
    assert_eq!(
        plugin.attachment(),
        fluent_egui::SlotAttachment::MainOverlay,
        "single smoothings paint as overlays"
    );
    let config = plugin.default_config();
    let evaluation = plugin
        .evaluate(&rows, &*config, &SeriesCatalog::empty())
        .expect("the picked smoothing evaluates");
    assert!(
        !evaluation.series.is_empty(),
        "the smoothing publishes a series"
    );
    plugin
        .paint_main_overlay(
            &painter,
            &evaluation,
            &overlay,
            egui::Pos2::ZERO,
            0,
            640.0,
            &theme,
            &*config,
            &IndicatorStyle::default(),
        )
        .expect("the overlay paints");
    let kind = oscillator_options()
        .into_iter()
        .find(|option| option.evaluable)
        .and_then(|option| {
            OscillatorKind::all()
                .into_iter()
                .find(|kind| kind.id() == option.id)
        })
        .expect("an evaluable oscillator lists");
    let raw: Vec<Option<f64>> = rows.iter().map(|row| Some(row.close)).collect();
    let normalized = crate::oscillators::normalize_series(&raw, kind).expect("normalizes");
    assert!(
        normalized.iter().any(Option::is_some),
        "the oscillator resolves values"
    );
    let points = fluent_egui::oscillator_points(&normalized, &overlay);
    assert!(
        points.iter().any(Option::is_some),
        "the oscillator maps to screen points"
    );
    fluent_egui::paint_oscillator_styled(
        &painter,
        &points,
        "fast",
        &theme,
        &IndicatorStyle::default(),
        1.0,
    )
    .expect("the oscillator paints");
    assert!(
        !end_frame(&ctx).shapes.is_empty(),
        "the picks emit geometry"
    );
}
