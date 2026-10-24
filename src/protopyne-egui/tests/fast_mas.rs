//! Fast MAs module: one isolated `Component` schema, kernel-truth evaluation
//! over fenced rows, visibility toggles, and paint isolation from Integration
//! Lite.
//!
//! Every moving average is evaluated through the shared kernel registry, so no
//! MA formula is re-implemented here. The tests pin the schema partition, the
//! kernel agreement, and the settings/cache isolation the module promises.

use super::*;

use fluent_egui::{IndicatorStyle, PaneCache, Viewport};
use protopyne_chart_model::ChartRow;
use protopyne_chart_scale::PriceScaleMode;
use protopyne_kernels::indicator::ParamSet;
use protopyne_kernels::smoothing::params_for_kernel;

use crate::integration_lite::IntegrationLiteConfig;
use crate::topology::{cache_key_for, kernel_series, source_frame, SeriesCatalog};

/// Sixty rising bars with a distinct volume leg, so `vwma` has real weight
/// and the close legs are bit-identical to the reference evaluation.
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

/// Expected values for one MA arm through the shared kernel registry, using
/// the catalog's smoothing-aware parameters.
fn expected(rows: &[ChartRow], kind: &str, length: i64) -> Vec<Option<f64>> {
    let frame = source_frame(rows, protopyne_kernels::PriceSource::Close);
    let params = params_for_kernel(kind, length).expect("supported arm");
    kernel_series(kind, &params, &frame).expect("kernel evaluates")
}

#[test]
fn fast_mas_config_derives_component_and_validates_fail_closed() {
    let config = FastMasConfig::new().build();
    let names = config.field_names();
    for expected_name in [
        "first_ma_type",
        "first_ma_length",
        "first_ma_style",
        "second_ma_type",
        "second_ma_length",
        "second_ma_style",
        "signal_ma_type",
        "signal_ma_length",
        "signal_ma_style",
        "mean_ma_type",
        "mean_ma_length",
        "mean_ma_style",
        "trend_ma_type",
        "trend_ma_length",
        "trend_ma_style",
        "base_ma_type",
        "base_ma_length",
        "base_ma_style",
        "sma_style",
        "show_first_ma",
        "show_second_ma",
        "show_extra_mas",
        "show_smas",
    ] {
        assert!(
            names.contains(&expected_name),
            "{expected_name} is in the schema: {names:?}"
        );
    }
    let fields = fluent_egui::schema_fields(&config.describe());
    assert_eq!(
        fields.len(),
        names.len(),
        "the schema describes every field"
    );
    let (mut inputs, mut style, mut visibility) = (0_u32, 0_u32, 0_u32);
    for field in &fields {
        match fluent_egui::classify_schema_field(field) {
            fluent_egui::SettingsTab::Inputs => inputs += 1,
            fluent_egui::SettingsTab::Style => style += 1,
            fluent_egui::SettingsTab::Visibility => visibility += 1,
        }
    }
    assert!(
        inputs > 0 && style > 0 && visibility > 0,
        "fields partition across Inputs/Style/Visibility: {inputs}/{style}/{visibility}"
    );
    assert!(
        config.validate().is_ok(),
        "the default config is self-consistent"
    );
    // Sub-window lengths refuse on every arm, at write time.
    for field in [
        "first_ma_length",
        "second_ma_length",
        "signal_ma_length",
        "mean_ma_length",
        "trend_ma_length",
        "base_ma_length",
    ] {
        let mut candidate = FastMasConfig::new().build();
        assert!(
            candidate.set_field(field, "0").is_err(),
            "zero {field} refuses at write"
        );
        assert!(
            candidate.set_field(field, "201").is_err(),
            "over-window {field} refuses at write"
        );
    }
    // Unsupported MA kernel names refuse.
    for field in ["first_ma_type", "second_ma_type", "signal_ma_type"] {
        let mut candidate = FastMasConfig::new().build();
        candidate
            .set_field(field, "not-a-smoothing")
            .expect("field is writable");
        assert!(candidate.validate().is_err(), "unsupported {field} refuses");
    }
    // Blank style keys refuse.
    let mut blank = FastMasConfig::new().build();
    blank
        .set_field("first_ma_style", "   ")
        .expect("field is writable");
    assert!(blank.validate().is_err(), "a blank style key refuses");
    // The WorkUnit contract is contract-complete.
    let unit: &dyn fluent_wvr::WorkUnit = &config;
    assert_eq!(unit.name(), "fast_mas_config");
    assert!(unit.depends().is_empty());
    assert!(unit.provides().is_empty());
    assert!(
        unit.execute(&fluent_wvr::WorkContext::default()).is_ok(),
        "default executes"
    );
    let rows = ramp_rows();
    assert!(
        evaluate_fast_mas(
            &rows,
            &FastMasConfig::new().first_ma_length(0).build(),
            &SeriesCatalog::empty()
        )
        .is_err(),
        "the evaluator refuses a config its own validation refuses"
    );
}

/// The MA period contract, shared with every kernel MA: each line length
/// spans 2..200 in the schema (so the settings render sliders) and the
/// validation refuses both sides of the window.
#[test]
fn ma_line_lengths_span_two_to_two_hundred() {
    let lifted = fluent_egui::schema_fields(&FastMasConfig::new().build().describe());
    for field in [
        "first_ma_length",
        "second_ma_length",
        "signal_ma_length",
        "mean_ma_length",
        "trend_ma_length",
        "base_ma_length",
    ] {
        let lifted = lifted
            .iter()
            .find(|lifted| lifted.name == field)
            .unwrap_or_else(|| panic!("{field} lifts"));
        assert_eq!(
            (lifted.minimum, lifted.maximum),
            (Some(2.0), Some(200.0)),
            "{field} declares the shared window"
        );
        for rejected in ["0", "1", "201"] {
            let mut candidate = FastMasConfig::new().build();
            assert!(
                candidate.set_field(field, rejected).is_err(),
                "{field} refuses {rejected} at write"
            );
        }
    }
    for length in [0, 1, 201, 100_000] {
        let candidate = FastMasConfig::new().first_ma_length(length).build();
        assert!(
            candidate.validate().is_err(),
            "length {length} refuses validation"
        );
    }
    for length in [2, 14, 200] {
        let candidate = FastMasConfig::new().first_ma_length(length).build();
        assert!(candidate.validate().is_ok(), "length {length} validates");
    }
}

#[test]
fn evaluate_fast_mas_matches_builtin_kernels_and_respects_visibility_flags() {
    let rows = ramp_rows();
    let view = evaluate_fast_mas(
        &rows,
        &FastMasConfig::new().build(),
        &SeriesCatalog::empty(),
    )
    .expect("default view builds");
    let names: Vec<&str> = view.lines.iter().map(|line| line.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["First MA", "Signal MA", "Mean MA", "Trend MA", "Base MA"]
    );
    for line in &view.lines {
        assert_eq!(
            line.values.len(),
            rows.len(),
            "{} aligns with the rows",
            line.name
        );
    }
    // Every arm is the shared kernel's own output, never a re-implementation.
    assert_eq!(
        view.lines[0].values,
        expected(&rows, "wma", 3),
        "First MA is wma(3)"
    );
    assert_eq!(
        view.lines[1].values,
        expected(&rows, "ema", 8),
        "Signal MA is ema(8)"
    );
    assert_eq!(
        view.lines[2].values,
        expected(&rows, "ema", 21),
        "Mean MA is ema(21)"
    );
    assert_eq!(
        view.lines[3].values,
        expected(&rows, "ema", 34),
        "Trend MA is ema(34)"
    );
    assert_eq!(
        view.lines[4].values,
        expected(&rows, "ema", 55),
        "Base MA is ema(55)"
    );
    // The Second MA arm is opt-in and evaluates through `alma`.
    let with_second = evaluate_fast_mas(
        &rows,
        &FastMasConfig::new().show_second_ma(true).build(),
        &SeriesCatalog::empty(),
    )
    .expect("second arm builds");
    let second = with_second
        .lines
        .iter()
        .find(|line| line.name == "Second MA")
        .expect("Second MA is present when shown");
    assert_eq!(
        second.values,
        expected(&rows, "alma", 8),
        "Second MA is alma(8)"
    );
    assert_eq!(with_second.lines.len(), view.lines.len() + 1);
    // The SMA trio is opt-in.
    let with_smas = evaluate_fast_mas(
        &rows,
        &FastMasConfig::new().show_smas(true).build(),
        &SeriesCatalog::empty(),
    )
    .expect("sma arm builds");
    let sma_names: Vec<&str> = with_smas
        .lines
        .iter()
        .filter(|line| line.name.starts_with("SMA "))
        .map(|line| line.name.as_str())
        .collect();
    assert_eq!(sma_names, vec!["SMA 50", "SMA 100", "SMA 200"]);
    for (name, length) in [("SMA 50", 50), ("SMA 100", 100), ("SMA 200", 200)] {
        let line = with_smas
            .lines
            .iter()
            .find(|line| line.name == name)
            .expect("sma present");
        assert_eq!(
            line.values,
            expected(&rows, "sma", length),
            "{name} is sma({length})"
        );
    }
    // Extra MAs can be hidden entirely, leaving only the first arm.
    let first_only = evaluate_fast_mas(
        &rows,
        &FastMasConfig::new().show_extra_mas(false).build(),
        &SeriesCatalog::empty(),
    )
    .expect("first-only builds");
    assert_eq!(first_only.lines.len(), 1);
    assert_eq!(first_only.lines[0].name, "First MA");
}

#[test]
fn catalog_arms_validate_and_evaluate_through_the_fast_mas_path() {
    use protopyne_kernels::smoothing::SmoothingFamily;
    // Length-driven families beyond the legacy seven validate and evaluate.
    for kind in ["kalman", "zlema", "na"] {
        let config = FastMasConfig::new()
            .first_ma_type(kind.to_owned())
            .first_ma_length(14)
            .build();
        assert!(config.validate().is_ok(), "{kind} validates");
        let view = evaluate_fast_mas(&ramp_rows(), &config, &SeriesCatalog::empty())
            .expect("catalog arm evaluates");
        let first = view
            .lines
            .iter()
            .find(|line| line.name == "First MA")
            .expect("first arm present");
        assert_eq!(first.values.len(), 60);
        let family = SmoothingFamily::parse(kind).expect("catalog covers it");
        assert!(family.takes_length());
    }
    // `eit` names its missing alpha instead of miscomputing.
    let eit = FastMasConfig::new()
        .first_ma_type("eit".to_owned())
        .first_ma_length(14)
        .build();
    assert!(eit.validate().is_err(), "eit refuses without an alpha");
    assert!(evaluate_fast_mas(&ramp_rows(), &eit, &SeriesCatalog::empty()).is_err());
    // Unknown names still refuse.
    let unknown = FastMasConfig::new()
        .first_ma_type("not-a-smoothing".to_owned())
        .build();
    assert!(unknown.validate().is_err());
}

/// Settings audit against the shipped Fast MAs script: every reference input
/// is present with its reference default. The two deliberate deltas stay
/// recorded here rather than drifting silently: the shared 2..200 window
/// admits length 2 on the first two arms where the script floors at 3 (the
/// kernels evaluate length 2 soundly), and the three script SMA colors ride
/// one theme key until the color audit.
#[test]
fn reference_inputs_match_the_shipped_defaults() {
    let config = FastMasConfig::new().build();
    assert_eq!(config.source, "close", "reference source");
    assert_eq!(config.first_ma_type, "wma", "reference first type");
    assert_eq!(config.first_ma_length, 3, "reference first length");
    assert!(config.show_first_ma, "reference shows the first average");
    assert_eq!(config.second_ma_type, "alma", "reference second type");
    assert_eq!(config.second_ma_length, 8, "reference second length");
    assert!(!config.show_second_ma, "reference hides the second average");
    for (kind, length, name) in [
        (&config.signal_ma_type, config.signal_ma_length, "signal"),
        (&config.mean_ma_type, config.mean_ma_length, "mean"),
        (&config.trend_ma_type, config.trend_ma_length, "trend"),
        (&config.base_ma_type, config.base_ma_length, "base"),
    ] {
        assert_eq!(kind, "ema", "reference {name} type");
        assert!(
            [8, 21, 34, 55].contains(&length),
            "reference {name} length, got {length}"
        );
    }
    assert_eq!(config.signal_ma_length, 8, "reference signal length");
    assert_eq!(config.mean_ma_length, 21, "reference mean length");
    assert_eq!(config.trend_ma_length, 34, "reference trend length");
    assert_eq!(config.base_ma_length, 55, "reference base length");
    assert!(
        config.show_extra_mas,
        "reference shows the signal/mean/trend/base group"
    );
    assert!(!config.show_smas, "reference hides the SMA trio");
    for (field, value) in [
        ("source", "close"),
        ("first_ma_type", "wma"),
        ("first_ma_length", "3"),
        ("second_ma_type", "alma"),
        ("second_ma_length", "8"),
        ("signal_ma_length", "8"),
        ("mean_ma_length", "21"),
        ("trend_ma_length", "34"),
        ("base_ma_length", "55"),
    ] {
        assert_eq!(
            config.get_field(field).expect("field reads"),
            value,
            "{field} round-trips its reference default"
        );
    }
}

#[test]
fn fast_mas_and_integration_lite_settings_are_completely_isolated() {
    let lite = IntegrationLiteConfig::new().build();
    let mut fast = FastMasConfig::new().build();
    // Distinct schemas: neither config names the other's fields.
    for name in fast.field_names() {
        assert!(
            !lite.field_names().contains(name),
            "{name} is fast-MAs-only"
        );
    }
    for name in lite.field_names() {
        assert!(
            !fast.field_names().contains(name),
            "{name} is Integration-Lite-only"
        );
    }
    // Editing one config never reaches into the other.
    let lite_alpha = lite.get_field("eit_alpha").expect("lite field reads");
    fast.set_field("first_ma_length", "9")
        .expect("fast field edits");
    assert_eq!(fast.first_ma_length, 9, "the fast edit landed");
    assert_eq!(
        lite.get_field("eit_alpha").expect("lite field reads"),
        lite_alpha
    );
    assert_eq!(
        lite.eit_alpha.to_bits(),
        IntegrationLiteConfig::new().build().eit_alpha.to_bits(),
        "the lite config still reads its own default"
    );
    // Cache entries keyed by each module's own fingerprint never collide, and
    // editing the fast config leaves the Integration-Lite entry untouched.
    let mut cache = PaneCache::new();
    let lite_key = cache_key_for(
        "integration_lite",
        &ParamSet::new().with_float("eit_alpha", lite.eit_alpha),
    );
    let lite_revision = cache.publish(lite_key, vec![Some(1.0), Some(2.0), None]);
    let fast_key_before = cache_key_for(
        "fast_mas",
        &params_for_kernel("wma", FastMasConfig::new().build().first_ma_length).expect("supported"),
    );
    let fast_key_after = cache_key_for(
        "fast_mas",
        &params_for_kernel("wma", fast.first_ma_length).expect("supported"),
    );
    assert_ne!(
        fast_key_before, fast_key_after,
        "an edit moves the fast cache key"
    );
    assert_ne!(lite_key, fast_key_after, "the two modules key apart");
    cache.publish(fast_key_after, vec![Some(9.0)]);
    let resident = cache
        .get(lite_key)
        .expect("the Integration-Lite entry stays resident");
    assert_eq!(
        resident.revision, lite_revision,
        "the lite revision is untouched"
    );
    assert_eq!(resident.values, vec![Some(1.0), Some(2.0), None]);
    // A second fast edit keeps the lite entry pinned and adds a fresh key.
    fast.set_field("first_ma_length", "11")
        .expect("fast field edits");
    let fast_key_latest = cache_key_for(
        "fast_mas",
        &params_for_kernel("wma", fast.first_ma_length).expect("supported"),
    );
    assert_ne!(fast_key_latest, fast_key_after);
    assert_eq!(
        cache.get(lite_key).expect("still resident").values,
        vec![Some(1.0), Some(2.0), None],
        "the lite series never drifts when the fast config changes"
    );
}

/// Headless painter over a test context: the emitter records shapes into the
/// frame instead of needing a window.
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

fn price_viewport() -> Viewport {
    Viewport::new(PriceScaleMode::Normal, 90.0, 170.0, 200.0, 8.0, 90.0, false)
        .expect("test viewport builds")
}

#[test]
fn paint_fast_mas_blits_every_line_and_refuses_a_broken_theme() {
    let rows = ramp_rows();
    let view = evaluate_fast_mas(
        &rows,
        &FastMasConfig::new().build(),
        &SeriesCatalog::empty(),
    )
    .expect("view builds");
    let theme = ThemeConfig::default();
    let (ctx, painter) = test_painter();
    paint_fast_mas(
        &painter,
        &view,
        &price_viewport(),
        egui::Pos2::new(0.0, 0.0),
        0,
        640.0,
        &theme,
        &IndicatorStyle::default(),
    )
    .expect("healthy theme paints");
    assert!(!end_frame(&ctx).shapes.is_empty(), "the lines emit shapes");
    // The first line's default style key is "slow"; breaking it refuses by
    // name instead of painting silent gray.
    let broken = ThemeConfig::new().slow("not-a-color".to_owned()).build();
    let (_, painter) = test_painter();
    let refused = paint_fast_mas(
        &painter,
        &view,
        &price_viewport(),
        egui::Pos2::new(0.0, 0.0),
        0,
        640.0,
        &broken,
        &IndicatorStyle::default(),
    )
    .expect_err("a broken needed key refuses");
    assert!(
        refused.to_string().contains("slow"),
        "refusal names the key: {refused}"
    );
}

#[test]
fn paint_fast_mas_samples_per_column_at_sub_pixel_zoom() {
    // The custom overlay routes through the shared sampler pair: at
    // sub-pixel zoom each line emits one vertex per column run (the run's
    // last present value), while the native mapping above one pixel per bar
    // is untouched.
    use fluent_egui::{needs_column_aggregation, oscillator_points, sampled_line_points};
    let rows = ramp_rows();
    let view = evaluate_fast_mas(
        &rows,
        &FastMasConfig::new().build(),
        &SeriesCatalog::empty(),
    )
    .expect("view builds");
    let tight = Viewport::new(PriceScaleMode::Normal, 90.0, 170.0, 200.0, 0.2, 90.0, false)
        .expect("sub-pixel viewport builds");
    assert!(needs_column_aggregation(tight.scale().bar_spacing()));
    for line in &view.lines {
        let sampled = sampled_line_points(&line.values, &tight, 0);
        let native = oscillator_points(&line.values, &price_viewport());
        assert!(sampled.len() < native.len(), "sampling sheds shapes");
        assert!(
            sampled.iter().any(Option::is_some),
            "present values still paint"
        );
    }
    // And the painter accepts the sampled path without refusing.
    let (ctx, painter) = test_painter();
    paint_fast_mas(
        &painter,
        &view,
        &tight,
        egui::Pos2::new(0.0, 0.0),
        0,
        640.0,
        &ThemeConfig::default(),
        &IndicatorStyle::default(),
    )
    .expect("sampled zoom paints");
    assert!(
        !end_frame(&ctx).shapes.is_empty(),
        "sampled lines emit shapes"
    );
}
