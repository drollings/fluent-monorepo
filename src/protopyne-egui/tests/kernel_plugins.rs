//! Schema-driven kernel plugins: multi-output goldens, legacy length
//! migration, and unknown-kernel/parameter refusal.
//!
//! Every plugin evaluates through the shared kernel registry, so these pin
//! the plugin contract — one series per declared output, primary node
//! naming, secondary nodes, cache keys, and readouts — never the kernel
//! math itself.

use super::*;

use std::collections::HashSet;

use fluent_egui::SlotAttachment;

use crate::topology::SeriesCatalog;

/// Read one effective int parameter.
fn effective_int(params: &protopyne_kernels::ParamSet, kernel: &str, name: &str) -> Option<i64> {
    params.int(kernel, name).ok()
}

/// 120 rising bars: long enough that every golden kernel (including the
/// 52-bar senkou span) warms past its leading hole.
fn golden_rows() -> Vec<ChartRow> {
    (0..120)
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

/// One multi-output golden: the declared node names, the first finite index
/// per series (warmup), and the exact last value per series.
struct MultiOutputGolden {
    id: &'static str,
    nodes: Vec<&'static str>,
    first: Vec<usize>,
    last: Vec<f64>,
    attachment: SlotAttachment,
}

fn multi_output_goldens() -> Vec<MultiOutputGolden> {
    vec![
        MultiOutputGolden {
            id: "macd",
            nodes: vec!["kernel:macd", "kernel:macd:signal", "kernel:macd:hist"],
            first: vec![25, 33, 33],
            last: vec![7.0, 7.000000000000002, -1.7763568394002505e-15],
            attachment: SlotAttachment::SubPane,
        },
        MultiOutputGolden {
            id: "bbands",
            nodes: vec![
                "kernel:bbands",
                "kernel:bbands:upper",
                "kernel:bbands:lower",
            ],
            first: vec![19, 19, 19],
            last: vec![209.5, 221.0325625946708, 197.9674374053292],
            attachment: SlotAttachment::SubPane,
        },
        MultiOutputGolden {
            id: "stoch",
            nodes: vec!["kernel:stoch", "kernel:stoch:d"],
            first: vec![13, 15],
            last: vec![100.0, 100.0],
            attachment: SlotAttachment::SubPane,
        },
        MultiOutputGolden {
            id: "ichimoku",
            nodes: vec![
                "kernel:ichimoku",
                "kernel:ichimoku:kijun",
                "kernel:ichimoku:senkou_a",
                "kernel:ichimoku:senkou_b",
                "kernel:ichimoku:chikou",
            ],
            first: vec![8, 25, 25, 51, 0],
            last: vec![215.0, 206.5, 210.75, 193.5, 219.0],
            attachment: SlotAttachment::SubPane,
        },
        MultiOutputGolden {
            id: "sma",
            nodes: vec!["kernel:sma"],
            first: vec![13],
            last: vec![212.5],
            attachment: SlotAttachment::MainOverlay,
        },
    ]
}

/// One series per declared output: exact node names, row alignment,
/// distinct cache keys, live readouts, and the golden warmup/value pins.
#[test]
fn multi_output_goldens_emit_one_series_per_declared_output() {
    let rows = golden_rows();
    assert_eq!(
        KernelIndicator::new("sma").expect("builds").attachment(),
        SlotAttachment::MainOverlay,
        "length smoothings overlay; everything else stays sub-pane"
    );
    for golden in multi_output_goldens() {
        let plugin = KernelIndicator::new(golden.id).expect("kernel plugin builds");
        assert_eq!(
            plugin.attachment(),
            golden.attachment,
            "{} paints where its family says",
            golden.id
        );
        let config = plugin.default_config();
        let nodes = plugin.plan_nodes(&*config).expect("nodes list");
        assert_eq!(
            nodes, golden.nodes,
            "{} plans one node per declared output",
            golden.id
        );
        assert!(
            plugin.plan_deps(&*config).expect("deps list").is_empty(),
            "{} reads bar input by default, so it declares no edges",
            golden.id
        );
        let evaluation = plugin
            .evaluate(&rows, &*config, &SeriesCatalog::empty())
            .expect("evaluates");
        assert_eq!(
            evaluation.series.len(),
            golden.nodes.len(),
            "{} emits one series per declared output",
            golden.id
        );
        let mut keys = HashSet::new();
        for (series, (node, (first, last))) in evaluation.series.iter().zip(
            golden
                .nodes
                .iter()
                .zip(golden.first.iter().zip(golden.last.iter())),
        ) {
            assert_eq!(&series.node, node, "the node names the output");
            assert_eq!(
                series.values.len(),
                rows.len(),
                "{} aligns with the rows",
                series.node
            );
            assert!(
                keys.insert(series.key),
                "{} has its own cache key",
                series.node
            );
            let observed = series
                .values
                .iter()
                .position(Option::is_some)
                .expect("some finite value");
            assert_eq!(observed, *first, "{} warms up as recorded", series.node);
            assert_eq!(
                series.values.last().copied().flatten(),
                Some(*last),
                "{} closes as recorded",
                series.node
            );
        }
        let readouts = plugin.readouts(&evaluation);
        assert_eq!(
            readouts.len(),
            golden.nodes.len(),
            "{} reads out every series",
            golden.id
        );
        assert!(
            readouts.iter().all(|readout| readout.value.is_some()),
            "{} readouts are live on a full ramp",
            golden.id
        );
    }
}

/// Declared defaults seed the settings surface: the golden kernels open
/// with their schema defaults, not zeros.
#[test]
fn golden_kernels_seed_declared_defaults() {
    let macd = KernelIndicatorConfig::defaults_for("macd");
    assert_eq!(macd.get_field("fast_length").expect("reads"), "12");
    assert_eq!(macd.get_field("slow_length").expect("reads"), "26");
    assert_eq!(macd.get_field("signal_length").expect("reads"), "9");
    let bbands = KernelIndicatorConfig::defaults_for("bbands");
    assert_eq!(bbands.get_field("length").expect("reads"), "20");
    assert_eq!(bbands.get_field("nbdevup").expect("reads"), "2");
    let stoch = KernelIndicatorConfig::defaults_for("stoch");
    assert_eq!(stoch.get_field("length").expect("reads"), "14");
    let ichimoku = KernelIndicatorConfig::defaults_for("ichimoku");
    assert_eq!(ichimoku.get_field("tenkan_length").expect("reads"), "9");
    assert_eq!(ichimoku.get_field("kijun_length").expect("reads"), "26");
    assert_eq!(ichimoku.get_field("senkou_b_length").expect("reads"), "52");
    let sma = KernelIndicatorConfig::defaults_for("sma");
    assert_eq!(sma.get_field("length").expect("reads"), "14");
}

/// The legacy `length` alias migrates old preferences: stored prefs from
/// before the schema surface carry `kernel` plus `length` and no `params`
/// map, so the legacy field must still drive length kernels while declared
/// values win and length-less kernels ignore it.
#[test]
fn legacy_length_migration_is_pinned() {
    // Old prefs JSON: no params map at all.
    let migrated: KernelIndicatorConfig =
        serde_json::from_str(r#"{"kernel":"sma","length":10,"style":"fast","source":"close"}"#)
            .expect("old prefs decode");
    assert!(migrated.params.is_empty(), "old prefs stored no params");
    let effective = migrated.effective_params().expect("effective params");
    assert_eq!(
        effective_int(&effective, "sma", "length"),
        Some(10),
        "the legacy alias drives a length kernel"
    );
    assert!(migrated.validate().is_ok(), "the migrated config validates");

    // A declared length wins over the legacy alias.
    let mut declared = KernelIndicatorConfig::new()
        .kernel("sma".to_owned())
        .length(10)
        .build();
    declared
        .set_field("length", "20")
        .expect("declared length edits");
    assert_eq!(
        effective_int(
            &declared.effective_params().expect("effective"),
            "sma",
            "length"
        ),
        Some(20),
        "the declared value wins"
    );

    // A kernel with no length parameter ignores the legacy alias.
    let macd = KernelIndicatorConfig::new()
        .kernel("macd".to_owned())
        .length(99)
        .build();
    let effective = macd.effective_params().expect("effective params");
    assert_eq!(effective_int(&effective, "macd", "fast_length"), Some(12));
    assert_eq!(effective_int(&effective, "macd", "slow_length"), Some(26));
    assert_eq!(effective_int(&effective, "macd", "signal_length"), Some(9));
    assert_eq!(
        effective_int(&effective, "sma", "length"),
        None,
        "no length is smuggled into a length-less kernel"
    );
    assert!(macd.validate().is_ok(), "the alias never breaks validation");
}

/// Unknown kernels and parameters refuse at every surface: construction,
/// schema, validation, effective params, field edits, reads, and
/// evaluation.
#[test]
fn unknown_kernel_and_parameter_refuse_at_every_surface() {
    assert!(
        KernelIndicator::new("not-a-kernel").is_err(),
        "unregistered kernels refuse to bind"
    );
    assert!(
        instantiate_indicator("not-a-kernel").is_err(),
        "unregistered ids refuse to instantiate"
    );
    let unknown = KernelIndicatorConfig::new()
        .kernel("not-a-kernel".to_owned())
        .build();
    assert!(unknown.schema().is_err(), "no schema for unknown kernels");
    assert!(
        unknown.validate().is_err(),
        "unknown kernels fail validation"
    );
    assert!(
        unknown.effective_params().is_err(),
        "unknown kernels have no effective params"
    );
    let plugin = KernelIndicator::new("sma").expect("builds");
    let evaluation = plugin.evaluate(&golden_rows(), &unknown, &SeriesCatalog::empty());
    assert!(evaluation.is_err(), "evaluation refuses the unknown kernel");

    // Unknown parameters refuse on a known kernel.
    let mut sma = KernelIndicatorConfig::defaults_for("sma");
    assert!(
        sma.set_field("not_a_param", "1").is_err(),
        "unknown parameters refuse to edit"
    );
    assert!(
        sma.get_field("not_a_param").is_err(),
        "unknown parameters refuse to read"
    );
    let mut params = sma.params.clone();
    params.insert("not_a_param".to_owned(), "1".to_owned());
    sma.params = params;
    assert!(
        sma.validate().is_err(),
        "stored unknown parameters fail validation"
    );
    assert!(
        sma.effective_params().is_err(),
        "stored unknown parameters have no effective set"
    );

    // Wrong kinds refuse with the field named.
    assert!(
        sma.set_field("length", "abc").is_err(),
        "an unparsable length refuses"
    );
    let mut macd = KernelIndicatorConfig::defaults_for("macd");
    assert!(
        macd.set_field("fast_length", "abc").is_err(),
        "an unparsable int refuses"
    );
    assert!(
        macd.set_field("fast_length", "0").is_ok(),
        "parsing accepts range; validation judges it"
    );
    assert!(
        macd.validate().is_err(),
        "a zero fast length fails the declared minimum"
    );
}
