use super::*;
use crate::oscillators::{
    normalize_oscillator, normalize_series, OscillatorKind, DISPLAY_EXTREME_BAND, DISPLAY_RULE_BAND,
};
use fluent_egui::band_squash;

fn ramp_rows(count: usize) -> Vec<ChartRow> {
    (0..count)
        .map(|i| {
            let close = 100.0 + i as f64;
            ChartRow {
                time: i as i64,
                open: close - 0.5,
                high: close + 0.5,
                low: close - 1.0,
                close,
                volume: 10,
            }
        })
        .collect()
}

#[test]
fn schema_renders_four_dials_with_blessed_defaults() {
    let config = KernelIndicatorConfig::defaults_for("rsi");
    let described = config.describe();
    let properties = described
        .get("properties")
        .expect("describe has properties");
    for field in ["normalize", "osc_gain", "osc_trim", "osc_threshold"] {
        assert!(
            properties.get(field).is_some(),
            "{field} renders from describe()"
        );
    }
    assert_eq!(
        config.get_field("normalize").expect("reads"),
        "true",
        "mapped kinds default to normalizing"
    );
    assert_eq!(
        config.get_field("osc_gain").expect("reads"),
        "",
        "gain defaults to following the kind default"
    );
    assert_eq!(
        config.get_field("osc_trim").expect("reads"),
        "0",
        "trim defaults to parity"
    );
    assert_eq!(
        config.get_field("osc_threshold").expect("reads"),
        "100",
        "threshold defaults to the frozen value"
    );
    let unmapped = KernelIndicatorConfig::defaults_for("sma");
    assert_eq!(
        unmapped.get_field("normalize").expect("reads"),
        "false",
        "unmapped kernels default off"
    );
}

#[test]
fn flipping_gain_changes_the_config_fingerprint() {
    let plugin = KernelIndicator::new("rsi").expect("builds");
    let config = plugin.default_config();
    let before = plugin.config_key(&*config).expect("fingerprints");
    let mut dialed = KernelIndicatorConfig::defaults_for("rsi");
    dialed.set_field("osc_gain", "2").expect("gain edits");
    let boxed: Box<dyn fluent_wvr::Component> = Box::new(dialed);
    let after = plugin.config_key(&*boxed).expect("fingerprints");
    assert_ne!(before, after, "gain joins the fingerprint");
    let rows = ramp_rows(60);
    let empty = crate::topology::SeriesCatalog::empty();
    let first = plugin.evaluate(&rows, &*config, &empty).expect("evaluates");
    let second = plugin.evaluate(&rows, &*boxed, &empty).expect("evaluates");
    assert_ne!(
        first.series.first().expect("one").key,
        second.series.first().expect("one").key,
        "a gain edit can never serve the stale series"
    );
}

#[test]
fn dial_refusals_name_the_field() {
    let mut gain = KernelIndicatorConfig::defaults_for("rsi");
    gain.set_field("osc_gain", "NaN").expect("parses");
    let error = format!("{}", gain.validate().expect_err("NaN gain refuses"));
    assert!(error.contains("osc_gain"), "NaN gain names the field");

    let mut negative = KernelIndicatorConfig::defaults_for("rsi");
    negative.set_field("osc_gain", "-1").expect("parses");
    let error = format!(
        "{}",
        negative.validate().expect_err("negative gain refuses")
    );
    assert!(error.contains("osc_gain"), "negative gain names the field");

    let mut threshold = KernelIndicatorConfig::defaults_for("rsi");
    threshold.set_field("osc_threshold", "0").expect("parses");
    let error = format!(
        "{}",
        threshold
            .validate()
            .expect_err("non-positive threshold refuses")
    );
    assert!(
        error.contains("osc_threshold"),
        "non-positive threshold names the field"
    );

    let mut unmapped = KernelIndicatorConfig::defaults_for("sma");
    unmapped.set_field("normalize", "true").expect("parses");
    let error = format!(
        "{}",
        unmapped
            .validate()
            .expect_err("normalize on unmapped refuses")
    );
    assert!(
        error.contains("normalize"),
        "normalize on unmapped names the field"
    );
}

#[test]
fn triple_dials_default_to_unset_and_follow_the_catalog() {
    let config = KernelIndicatorConfig::defaults_for("rsi");
    assert_eq!(config.osc_offset, None, "offset is unset at defaults");
    assert_eq!(config.osc_k, None, "k is unset at defaults");
    assert_eq!(config.osc_mult, None, "multiplier is unset at defaults");
    assert_eq!(
        config.get_field("osc_offset").expect("reads"),
        "",
        "an unset dial reads as empty, like an uninherited style override"
    );
    let described = config.describe();
    let properties = described
        .get("properties")
        .expect("describe has properties");
    for field in ["osc_offset", "osc_k", "osc_mult"] {
        let hint = properties
            .get(field)
            .unwrap_or_else(|| panic!("{field} renders from describe()"));
        assert_eq!(
            hint.get("x-tab").and_then(serde_json::Value::as_str),
            Some("style"),
            "{field} renders on the Style tab"
        );
    }
}

#[test]
fn triple_dial_refusals_name_the_field() {
    for (field, bad) in [
        ("osc_offset", "NaN"),
        ("osc_offset", "inf"),
        ("osc_k", "NaN"),
        ("osc_k", "0"),
        ("osc_k", "-0.5"),
        ("osc_mult", "NaN"),
        ("osc_mult", "0"),
        ("osc_mult", "-740"),
    ] {
        let mut dialed = KernelIndicatorConfig::defaults_for("rsi");
        dialed.set_field(field, bad).expect("parses");
        let error = format!("{}", dialed.validate().expect_err("refuses"));
        assert!(
            error.contains(field),
            "{field}={bad} names the field: {error}"
        );
    }
    let mut cleared = KernelIndicatorConfig::defaults_for("rsi");
    cleared.set_field("osc_k", "2.0").expect("parses");
    cleared.set_field("osc_k", "").expect("clearing parses");
    assert_eq!(cleared.osc_k, None, "empty restores the catalog default");
    assert!(cleared.validate().is_ok(), "the cleared dial validates");
}

#[test]
fn explicit_triple_reshapes_display_against_the_catalog_row() {
    use fluent_egui::affine_squash;
    let rows = ramp_rows(120);
    let plugin = KernelIndicator::new("rsi").expect("builds");
    let mut config = KernelIndicatorConfig::defaults_for("rsi");
    config.set_field("osc_offset", "-40.0").expect("parses");
    config.set_field("osc_k", "0.0168").expect("parses");
    config.set_field("osc_mult", "370.0").expect("parses");
    assert!(config.validate().is_ok(), "the explicit triple validates");
    let boxed: Box<dyn fluent_wvr::Component> = Box::new(config);
    let evaluation = plugin
        .evaluate(&rows, &*boxed, &crate::topology::SeriesCatalog::empty())
        .expect("evaluates");
    let series = evaluation.series.first().expect("one series");
    let expected: Vec<Option<f64>> = series
        .values
        .iter()
        .map(|slot| {
            slot.map(|raw| {
                band_squash(
                    affine_squash(raw, -40.0, 0.0168, 370.0),
                    DISPLAY_RULE_BAND,
                    DISPLAY_EXTREME_BAND,
                )
            })
        })
        .collect();
    assert_eq!(
        series.display_values, expected,
        "an explicit triple drives the display leg"
    );
    assert_ne!(
        series.display_values,
        normalize_series(&series.values, OscillatorKind::Rsi)
            .expect("kind evaluates")
            .iter()
            .map(|slot| slot.map(|value| band_squash(
                value,
                DISPLAY_RULE_BAND,
                DISPLAY_EXTREME_BAND
            )))
            .collect::<Vec<_>>(),
        "the triple visibly departs from catalog defaults"
    );
}

#[test]
fn defaults_reproduce_the_mapped_normalized_values() {
    let rows = ramp_rows(120);
    let plugin = KernelIndicator::new("rsi").expect("builds");
    let config = KernelIndicatorConfig::defaults_for("rsi");
    let boxed: Box<dyn fluent_wvr::Component> = Box::new(config);
    let evaluation = plugin
        .evaluate(&rows, &*boxed, &crate::topology::SeriesCatalog::empty())
        .expect("evaluates");
    let series = evaluation.series.first().expect("one series");
    let catalog = normalize_series(&series.values, OscillatorKind::Rsi).expect("kind evaluates");
    let expected: Vec<Option<f64>> = catalog
        .iter()
        .map(|slot| slot.map(|value| band_squash(value, DISPLAY_RULE_BAND, DISPLAY_EXTREME_BAND)))
        .collect();
    assert_eq!(
        series.display_values, expected,
        "defaults are byte-identical to the band-constrained catalog squash"
    );
}

#[test]
fn smoothing_dials_default_to_passthrough() {
    let config = KernelIndicatorConfig::defaults_for("rsi");
    assert_eq!(
        config.get_field("smooth_type").expect("reads"),
        "na",
        "no smoothing by default"
    );
    assert_eq!(
        config.get_field("smooth_period").expect("reads"),
        "3",
        "the Pine-standard smoothing window"
    );
    let described = config.describe();
    let properties = described
        .get("properties")
        .expect("describe has properties");
    for field in ["smooth_type", "smooth_period"] {
        let hint = properties
            .get(field)
            .unwrap_or_else(|| panic!("{field} renders from describe()"));
        assert_eq!(
            hint.get("x-tab").and_then(serde_json::Value::as_str),
            Some("style"),
            "{field} renders on the Style tab"
        );
    }
    let period = properties.get("smooth_period").expect("period renders");
    assert_eq!(
        period.get("minimum").and_then(serde_json::Value::as_f64),
        Some(2.0),
        "the smoothing window shares the MA floor"
    );
    assert_eq!(
        period.get("maximum").and_then(serde_json::Value::as_f64),
        Some(200.0),
        "the smoothing window shares the MA ceiling"
    );
}

#[test]
fn smoothing_refusals_name_the_field() {
    for smooth_type in ["not-a-smoothing", "eit"] {
        let mut candidate = KernelIndicatorConfig::defaults_for("rsi");
        candidate
            .set_field("smooth_type", smooth_type)
            .expect("parses");
        let error = format!("{}", candidate.validate().expect_err("refuses"));
        assert!(
            error.contains("smooth_type"),
            "{smooth_type} names the field: {error}"
        );
    }
    for period in ["1", "201"] {
        let mut candidate = KernelIndicatorConfig::defaults_for("rsi");
        candidate.set_field("smooth_type", "sma").expect("parses");
        candidate
            .set_field("smooth_period", period)
            .expect("parses");
        let error = format!("{}", candidate.validate().expect_err("refuses"));
        assert!(
            error.contains("smooth_period"),
            "period {period} names the field: {error}"
        );
    }
}

#[test]
fn smoothed_legs_match_the_smoothing_kernel_then_the_display_pipeline() {
    use crate::topology::series_frame;
    use protopyne_kernels::smoothing::SmoothingFamily;
    let rows = ramp_rows(120);
    let plugin = KernelIndicator::new("rsi").expect("builds");
    let mut config = KernelIndicatorConfig::defaults_for("rsi");
    config.set_field("smooth_type", "sma").expect("parses");
    config.set_field("smooth_period", "7").expect("parses");
    assert!(config.validate().is_ok(), "the armed smoothing validates");
    let boxed: Box<dyn fluent_wvr::Component> = Box::new(config);
    let evaluation = plugin
        .evaluate(&rows, &*boxed, &crate::topology::SeriesCatalog::empty())
        .expect("evaluates");
    let series = evaluation.series.first().expect("one series");
    let family = SmoothingFamily::parse("sma").expect("parses");
    let frame = series_frame(&rows, &series.values).expect("frames");
    let params = family.params_for_length(7).expect("length params build");
    let smoothed = protopyne_kernels::shared_registry()
        .expect("registry builds")
        .get("sma")
        .expect("sma registers")
        .batch(&frame.views(), &params)
        .expect("smooths")
        .values;
    let expected: Vec<Option<f64>> = smoothed
        .iter()
        .map(|slot| {
            slot.and_then(|raw| {
                normalize_oscillator(raw, OscillatorKind::Rsi)
                    .ok()
                    .map(|value| band_squash(value, DISPLAY_RULE_BAND, DISPLAY_EXTREME_BAND))
            })
            .filter(|value| value.is_finite())
        })
        .collect();
    // The house warmup contract: the smoothing kernel propagates holes, and
    // the implementation masks producer + own + smoothing warmup on top.
    // Bar inputs carry no producer warmup, so the pinned shape is the
    // kernel's own holes plus the smoothing window.
    assert_eq!(
        series.display_values, expected,
        "the smoothed leg equals the smoothing kernel then the display pipeline"
    );
    let first_live_raw = series
        .values
        .iter()
        .position(Option::is_some)
        .expect("live raw");
    let first_live_display = series
        .display_values
        .iter()
        .position(Option::is_some)
        .expect("live display");
    assert_eq!(
        first_live_display,
        first_live_raw + 7 - 1,
        "SMA(7) needs a full window of live inputs"
    );
    assert!(
        series.display_values.iter().any(Option::is_some),
        "smoothing leaves live values past warmup"
    );
}
