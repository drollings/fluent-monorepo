//! Indicator outputs as inputs: `SeriesSource` selection, plan-ordered
//! evaluation, cycle refusal, warmup composition, and transitive cache keys.
//!
//! Fast MA lines publish under stable nodes (`fast_mas:first`, ...); any
//! other indicator may name one as its input series. Ticker-style
//! availability rules apply: a missing series refuses, it never lowers a
//! score.

use super::*;
use protopyne_chart_model::ChartRow;
use protopyne_kernels::smoothing::params_for_kernel;
use protopyne_kernels::PriceSource;

use crate::indicators::{FastMasIndicator, KernelIndicator, KernelIndicatorConfig};
use crate::topology::{kernel_warmup, SeriesCatalog, SeriesSource};

/// Sixty rising bars, so every arm has clean windows past its warmup.
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

/// Deterministic pseudo-random rows: an LCG walk with distinct volume, so the
/// warmup property cannot lean on monotonic input.
fn walk_rows(seed: u64, count: usize) -> Vec<ChartRow> {
    let mut state = seed;
    let mut next = || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) as f64 / f64::from(u32::MAX)
    };
    let mut close = 100.0;
    (0..count)
        .map(|i| {
            close += next() - 0.48;
            let spread = 0.2 + next() * 0.6;
            ChartRow {
                time: i as i64,
                open: close - spread,
                high: close + spread,
                low: close - spread * 2.0,
                close,
                volume: 500 + (next() * 2_000.0) as u64,
            }
        })
        .collect()
}

#[test]
fn series_source_spellings_parse_and_display() {
    assert_eq!(
        SeriesSource::parse("close"),
        Some(SeriesSource::Bars(PriceSource::Close))
    );
    assert_eq!(
        SeriesSource::parse("HL2"),
        Some(SeriesSource::Bars(PriceSource::Hl2))
    );
    assert_eq!(
        SeriesSource::parse("series:fast_mas:first"),
        Some(SeriesSource::Series("fast_mas:first".to_owned()))
    );
    assert_eq!(
        SeriesSource::default(),
        SeriesSource::Bars(PriceSource::Close)
    );
    assert_eq!(SeriesSource::default().to_string(), "close");
    assert_eq!(
        SeriesSource::Series("fast_mas:first".to_owned()).to_string(),
        "series:fast_mas:first"
    );
    assert_eq!(
        SeriesSource::Series("kernel:sma".to_owned()).series_node(),
        Some("kernel:sma")
    );
    assert_eq!(SeriesSource::Bars(PriceSource::Close).series_node(), None);
    for bad in [
        "",
        "series:",
        "series:  ",
        "fast_mas:first",
        "closes",
        "series",
    ] {
        assert_eq!(SeriesSource::parse(bad), None, "{bad:?} refuses");
    }
}

#[test]
fn series_catalog_tracks_leading_holes_as_producer_warmup() {
    let mut catalog = SeriesCatalog::empty();
    assert_eq!(catalog.resolve("fast_mas:first"), None);
    assert_eq!(catalog.leading_holes("fast_mas:first"), None);
    catalog.publish(
        "fast_mas:first",
        11,
        vec![None, None, None, Some(1.0), Some(2.0)],
    );
    assert_eq!(
        catalog.resolve("fast_mas:first"),
        Some([None, None, None, Some(1.0), Some(2.0)].as_slice())
    );
    assert_eq!(catalog.leading_holes("fast_mas:first"), Some(3));
    // A republish replaces the row, holes recomputed.
    catalog.publish("fast_mas:first", 12, vec![Some(1.0), Some(2.0)]);
    assert_eq!(catalog.leading_holes("fast_mas:first"), Some(0));
}

#[test]
fn default_bars_close_evaluations_match_independent_kernel_truth() {
    // The default-source pin: with `Bars(Close)` a Fast MA line and a kernel
    // line are byte-identical to the pre-change close-frame evaluation.
    let rows = ramp_rows();
    let empty = SeriesCatalog::empty();
    let fast = FastMasIndicator::new();
    let evaluation = fast
        .evaluate(&rows, &*fast.default_config(), &empty)
        .expect("fast mas evaluates");
    let close_frame = crate::topology::source_frame(&rows, PriceSource::Close);
    assert!(
        crate::topology::series_frame(&rows, &[]).is_err(),
        "a length-mismatched frame refuses"
    );
    let arms = [
        ("wma", 3),
        ("ema", 8),
        ("ema", 21),
        ("ema", 34),
        ("ema", 55),
    ];
    assert_eq!(evaluation.series.len(), arms.len());
    for (series, (kind, length)) in evaluation.series.iter().zip(arms) {
        let params = params_for_kernel(kind, length).expect("supported arm");
        let truth =
            crate::topology::kernel_series(kind, &params, &close_frame).expect("kernel evaluates");
        assert_eq!(series.values, truth, "{} is byte-identical", series.node);
    }
    let kernel = KernelIndicator::new("sma").expect("a kernel plugin builds");
    let evaluation = kernel
        .evaluate(&rows, &*kernel.default_config(), &empty)
        .expect("kernel evaluates");
    let params = params_for_kernel("sma", 14).expect("supported arm");
    let truth =
        crate::topology::kernel_series("sma", &params, &close_frame).expect("kernel evaluates");
    assert_eq!(
        evaluation.series[0].values, truth,
        "kernel default is byte-identical"
    );
}

#[test]
fn consumer_cache_key_composes_the_producer_fingerprint_transitively() {
    let rows = ramp_rows();
    let producer = FastMasIndicator::new();
    let produced = producer
        .evaluate(&rows, &*producer.default_config(), &SeriesCatalog::empty())
        .expect("producer evaluates");
    let first = produced
        .series
        .iter()
        .find(|series| series.node == "fast_mas:first")
        .expect("first publishes");
    let mut first_catalog = SeriesCatalog::empty();
    first_catalog.publish(&first.node, first.key, first.values.clone());
    let mut second_catalog = SeriesCatalog::empty();
    second_catalog.publish(&first.node, first.key.wrapping_add(1), first.values.clone());
    let kernel = KernelIndicator::new("sma").expect("a kernel plugin builds");
    let config = KernelIndicatorConfig::new()
        .kernel("sma".to_owned())
        .length(10)
        .source("series:fast_mas:first".to_owned())
        .build();
    let first_key = kernel
        .evaluate(&rows, &config, &first_catalog)
        .expect("consumes")
        .series[0]
        .key;
    let second_key = kernel
        .evaluate(&rows, &config, &second_catalog)
        .expect("consumes")
        .series[0]
        .key;
    assert_ne!(
        first_key, second_key,
        "a producer fingerprint change invalidates the consumer with its config untouched"
    );
    let stable = kernel
        .evaluate(&rows, &config, &first_catalog)
        .expect("consumes")
        .series[0]
        .key;
    assert_eq!(
        first_key, stable,
        "an unchanged producer keeps the consumer key stable"
    );
}

#[test]
fn warmup_sums_along_producer_consumer_chains() {
    // Property: on randomized series, consumer holes cover at least the
    // composed warmup (producer leading holes + own warmup).
    for trial in 0usize..25 {
        let rows = walk_rows(0x9E3779B9 ^ (trial as u64 * 0x85EBCA6B), 60);
        let producer_length = 2 + (trial % 14) as i64;
        let consumer_length = 2 + ((trial * 7) % 19) as i64;
        let producer_config = crate::fast_mas::FastMasConfig::new()
            .first_ma_type("wma".to_owned())
            .first_ma_length(producer_length)
            .show_extra_mas(false)
            .build();
        let produced =
            crate::fast_mas::evaluate_fast_mas(&rows, &producer_config, &SeriesCatalog::empty())
                .expect("producer evaluates");
        let line = &produced.lines[0];
        let producer_holes = line
            .values
            .iter()
            .take_while(|value| value.is_none())
            .count();
        let mut catalog = SeriesCatalog::empty();
        catalog.publish("fast_mas:first", 1, line.values.clone());
        let consumer_config = KernelIndicatorConfig::new()
            .kernel("sma".to_owned())
            .length(consumer_length)
            .source("series:fast_mas:first".to_owned())
            .build();
        let kernel = KernelIndicator::new("sma").expect("a kernel plugin builds");
        let consumed = kernel
            .evaluate(&rows, &consumer_config, &catalog)
            .expect("consumer evaluates");
        let params = params_for_kernel("sma", consumer_length).expect("supported arm");
        let own = kernel_warmup("sma", &params).expect("warmup reads");
        let composed = producer_holes + own;
        let holes = consumed.series[0]
            .values
            .iter()
            .take_while(|value| value.is_none())
            .count();
        assert!(
            holes >= composed.min(60),
            "trial {trial}: {holes} holes cover the composed warmup {composed} (producer {producer_holes} + own {own})"
        );
    }
}

#[test]
fn fast_mas_consumes_a_published_line_with_per_arm_warmup() {
    let rows = walk_rows(0x12345678, 60);
    let producer_config = crate::fast_mas::FastMasConfig::new()
        .first_ma_type("wma".to_owned())
        .first_ma_length(5)
        .show_extra_mas(false)
        .build();
    let produced =
        crate::fast_mas::evaluate_fast_mas(&rows, &producer_config, &SeriesCatalog::empty())
            .expect("producer evaluates");
    let line = &produced.lines[0];
    let producer_holes = line
        .values
        .iter()
        .take_while(|value| value.is_none())
        .count();
    let mut catalog = SeriesCatalog::empty();
    catalog.publish("fast_mas:first", 1, line.values.clone());
    let consumer_config = crate::fast_mas::FastMasConfig::new()
        .first_ma_type("ema".to_owned())
        .first_ma_length(8)
        .show_extra_mas(false)
        .source("series:fast_mas:first".to_owned())
        .build();
    let consumed = crate::fast_mas::evaluate_fast_mas(&rows, &consumer_config, &catalog)
        .expect("consumer evaluates");
    let own = protopyne_kernels::smoothing::SmoothingFamily::Ema
        .warmup_for_length(8)
        .expect("warmup reads");
    let holes = consumed.lines[0]
        .values
        .iter()
        .take_while(|value| value.is_none())
        .count();
    assert!(
        holes >= (producer_holes + own).min(60),
        "{holes} holes cover producer {producer_holes} + own {own}"
    );
}

#[test]
fn self_cycle_and_pair_cycle_refuse_with_the_cycle_named() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows()).expect("the workspace boots");
    let err = workspace
        .set_slot_field("fast_mas", "source", "series:fast_mas:first")
        .expect_err("a self-cycle refuses");
    let message = err.to_string();
    assert!(
        message.contains("fast_mas:first"),
        "the refusal names the node: {message}"
    );
    assert!(
        message.contains("cycle"),
        "the refusal names the cycle: {message}"
    );
}

#[test]
fn crossed_kernel_sources_refuse_naming_both_nodes() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows()).expect("the workspace boots");
    workspace.slot_indicator("sma").expect("sma slots");
    workspace.slot_indicator("ema").expect("ema slots");
    workspace
        .set_slot_field("sma", "source", "series:kernel:ema")
        .expect("one leg slots cleanly");
    let err = workspace
        .set_slot_field("ema", "source", "series:kernel:sma")
        .expect_err("the crossed leg refuses");
    let message = err.to_string();
    assert!(
        message.contains("kernel:sma"),
        "the refusal names the sma node: {message}"
    );
    assert!(
        message.contains("kernel:ema"),
        "the refusal names the ema node: {message}"
    );
}

#[test]
fn unknown_series_and_blank_sources_refuse_with_reason() {
    let rows = ramp_rows();
    let kernel = KernelIndicator::new("sma").expect("a kernel plugin builds");
    let missing = KernelIndicatorConfig::new()
        .kernel("sma".to_owned())
        .length(10)
        .source("series:kernel:nope".to_owned())
        .build();
    let err = kernel
        .evaluate(&rows, &missing, &SeriesCatalog::empty())
        .expect_err("missing series refuses");
    assert!(
        err.to_string().contains("kernel:nope"),
        "the refusal names the series: {err}"
    );
    let blank = KernelIndicatorConfig::new()
        .kernel("sma".to_owned())
        .length(10)
        .source("not a source".to_owned())
        .build();
    assert!(
        blank.validate().is_err(),
        "an unparsable source fails validation"
    );
}
