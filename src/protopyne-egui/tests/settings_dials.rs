use super::*;
use crate::oscillators::normalize_series;

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
        "1",
        "gain defaults to parity"
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
fn defaults_reproduce_the_mapped_normalized_values() {
    let rows = ramp_rows(120);
    let plugin = KernelIndicator::new("rsi").expect("builds");
    let config = KernelIndicatorConfig::defaults_for("rsi");
    let boxed: Box<dyn fluent_wvr::Component> = Box::new(config);
    let evaluation = plugin
        .evaluate(&rows, &*boxed, &crate::topology::SeriesCatalog::empty())
        .expect("evaluates");
    let series = evaluation.series.first().expect("one series");
    let expected = normalize_series(&series.values, crate::oscillators::OscillatorKind::Rsi)
        .expect("kind evaluates");
    assert_eq!(
        series.display_values, expected,
        "defaults are byte-identical to the catalog squash"
    );
}
