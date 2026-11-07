//! Pine indicator plugin: header overlay detection, row conversion,
//! linking (including refusal), and evaluation over synthetic bars.
//! Everything runs the real interpreter — no mocks — on hermetic inputs.

use super::*;
use fluent_egui::SlotAttachment;
use protopyne_chart_model::ChartRow;

use crate::topology::SeriesCatalog;

fn series_catalog() -> SeriesCatalog {
    SeriesCatalog::empty()
}

fn rows(closes: &[f64]) -> Vec<ChartRow> {
    closes
        .iter()
        .enumerate()
        .map(|(i, close)| ChartRow {
            time: 1_767_344_400 + i as i64 * 60,
            open: close - 0.25,
            high: close + 0.5,
            low: close - 0.5,
            close: *close,
            volume: 1_000,
        })
        .collect()
}

fn plot_close_script() -> &'static str {
    "indicator(\"T\", overlay=true)\nplot(close)\nplot(close * 2.0)\n"
}

fn linked(script: &str) -> PineIndicatorPlugin {
    PineIndicatorPlugin::link("pine:_T", "🌲 T", script, &std::collections::HashMap::new())
        .expect("inline script links")
}

#[test]
fn pine_overlay_header_shapes() {
    // Named flag wins in both spacings.
    assert!(pine_overlay_of(
        "indicator(\"T\", overlay=true)\nplot(close)"
    ));
    assert!(pine_overlay_of(
        "indicator(\"T\", overlay = true)\nplot(close)"
    ));
    assert!(!pine_overlay_of(
        "indicator(\"T\", overlay=false)\nplot(close)"
    ));
    // Positional third slot carries the flag when it is a bare literal.
    assert!(pine_overlay_of("indicator('T', 'T', true)\nplot(close)"));
    assert!(!pine_overlay_of("indicator('T', 'T', false)\nplot(close)"));
    // Absent, unparseable, or missing header: the Pine default (off).
    assert!(!pine_overlay_of("indicator(\"T\")\nplot(close)"));
    assert!(!pine_overlay_of("plot(close)"));
    assert!(!pine_overlay_of("not pine at all"));
}

#[test]
fn pine_rows_convert_to_kernel_bars() {
    let bars = rows_to_bars(&rows(&[100.0, 101.5])).expect("finite rows convert");
    assert_eq!(bars.len(), 2);
    assert_eq!(bars[1].c.to_string(), "101.5");
    assert_eq!(bars[1].volume, 1_000);
    // A non-finite leg refuses naming the bar, never inventing a price.
    let mut bad = rows(&[100.0]);
    bad[0].high = f64::NAN;
    assert!(
        rows_to_bars(&bad).is_err(),
        "NaN refuses with the bar named"
    );
    let mut bad_ts = rows(&[100.0]);
    bad_ts[0].time = i64::MAX;
    assert!(
        rows_to_bars(&bad_ts).is_err(),
        "an unrepresentable timestamp refuses"
    );
}

#[test]
fn pine_plots_evaluate_per_bar_over_synthetic_tape() {
    let plugin = linked(plot_close_script());
    assert_eq!(plugin.id(), "pine:_T");
    let config = PineRuntimeConfig::new().build();
    assert_eq!(plugin.plan_nodes(&config).expect("nodes"), vec!["pine:_T"]);
    let evaluation = plugin
        .evaluate(&rows(&[100.0, 101.0, 102.0]), &config, &series_catalog())
        .expect("plots evaluate");
    assert_eq!(evaluation.series.len(), 2, "one series per plot");
    assert_eq!(evaluation.series[0].node, "pine:_T#p0");
    assert_eq!(evaluation.series[1].node, "pine:_T#p1");
    let first: Vec<Option<f64>> = evaluation.series[0].values.clone();
    assert_eq!(first, vec![Some(100.0), Some(101.0), Some(102.0)]);
    let second: Vec<Option<f64>> = evaluation.series[1].values.clone();
    assert_eq!(second, vec![Some(200.0), Some(202.0), Some(204.0)]);
}

#[test]
fn pine_linking_refuses_unknown_imports() {
    let err = PineIndicatorPlugin::link(
        "pine:_NeedsLib",
        "NeedsLib",
        "import showmethegrail/libMissing/1 as M\nplot(close)\n",
        &std::collections::HashMap::new(),
    )
    .expect_err("an unresolvable import must refuse");
    assert!(
        err.to_string().contains("libMissing"),
        "the refusal names the library: {err}"
    );
}

#[test]
fn pine_plugin_config_round_trips() {
    let plugin = linked(plot_close_script());
    let config = plugin.default_config();
    let key = plugin.config_key(config.as_ref()).expect("key");
    assert_ne!(key, 0, "the fingerprint is a real hash");
    let back = plugin.default_config();
    assert_eq!(
        plugin.config_key(back.as_ref()).expect("key"),
        key,
        "default configs fingerprint identically"
    );
}

#[test]
fn pine_data_root_finds_the_sibling_indicators_dir() {
    let root = tempfile::tempdir().expect("temp root builds");
    std::fs::create_dir_all(root.path().join("historical")).expect("history dir builds");
    std::fs::create_dir_all(root.path().join("indicators")).expect("indicators dir builds");
    let history_file = root.path().join("historical/SPY_1M.csv");
    std::fs::write(&history_file, "o,h,l,c\n").expect("history file builds");
    assert_eq!(
        pine_data_root(&history_file),
        Some(root.path().to_path_buf()),
        "a history file resolves to the data root above it"
    );
    assert_eq!(
        pine_data_root(&root.path().join("historical")),
        Some(root.path().to_path_buf()),
        "a history dir resolves the same way"
    );
    assert_eq!(
        pine_data_root(&root.path().join("elsewhere")),
        Some(root.path().to_path_buf()),
        "a history path anywhere under the data root still resolves it"
    );
    let bare = tempfile::tempdir().expect("bare root builds");
    let bare_file = bare.path().join("SPY_1M.csv");
    std::fs::write(&bare_file, "o,h,l,c\n").expect("bare file builds");
    assert_eq!(
        pine_data_root(&bare_file),
        None,
        "no indicators dir nearby means no Pine catalog"
    );
}

#[test]
fn every_shipped_indicator_either_links_or_names_its_gap() {
    // Directory-driven over the volatile live root: every indicator file
    // either links or refuses, and every refusal names its structural
    // cause (unsupported version, unknown import, or unparseable text).
    // No stem list lives here: adding, removing, or renaming a script
    // changes only which per-file rows this probe produces.
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs");
    let (indicators, libraries) =
        protopyne_kernels::load_pine_sources(&root).expect("the shipped root loads");
    assert!(
        !indicators.is_empty(),
        "the data root ships indicators to check"
    );
    let libs: std::collections::HashMap<String, String> = libraries
        .iter()
        .map(|file| (file.relpath.clone(), file.text.clone()))
        .collect();
    for file in &indicators {
        let stem = file
            .relpath
            .rsplit('/')
            .next()
            .unwrap_or(&file.relpath)
            .strip_suffix(".pine")
            .unwrap_or(&file.relpath);
        let id = format!("pine:{stem}");
        match PineIndicatorPlugin::link(&id, stem, &file.text, &libs) {
            Ok(_) => {}
            Err(err) => {
                let reason = err.to_string();
                assert!(
                    reason.contains("parse")
                        || reason.contains("import")
                        || reason.contains("link")
                        || reason.contains("librar"),
                    "{stem} refuses with a structural reason: {reason}"
                );
            }
        }
    }
}

#[test]
fn fixture_fast_mas_evaluates_through_the_plugin() {
    // Hermetic fixture (server `tests/data/pine_closure`, checked in): the shipped
    // Fast MAs links as `pine:_FastMAs`, attaches MainOverlay, and
    // evaluates to plotted series — without depending on the volatile
    // live root.
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../protopyne-server/tests/data/pine_closure");
    let (indicators, libraries) =
        protopyne_kernels::load_pine_sources(&dir).expect("the fixture root loads");
    let file = indicators
        .iter()
        .find(|file| file.relpath == "indicators/_FastMAs.pine")
        .expect("fixture Fast MAs present");
    let libs: std::collections::HashMap<String, String> = libraries
        .iter()
        .map(|file| (file.relpath.clone(), file.text.clone()))
        .collect();
    let plugin = PineIndicatorPlugin::link("pine:_FastMAs", "🌲 Fast MAs", &file.text, &libs)
        .expect("fixture Fast MAs links");
    assert_eq!(plugin.attachment(), SlotAttachment::MainOverlay);
    let config = PineRuntimeConfig::new().build();
    let evaluation = plugin
        .evaluate(&rows(&[100.0; 60]), &config, &series_catalog())
        .expect("fixture Fast MAs evaluates");
    assert!(!evaluation.series.is_empty(), "plots publish series");
    let finite: usize = evaluation
        .series
        .iter()
        .flat_map(|s| &s.values)
        .flatten()
        .count();
    assert!(
        finite > 0,
        "at least one plot resolves finite values on the ramp"
    );
}

#[test]
fn drawings_project_to_overlay_primitives() {
    // Idiomatic markers carry a price gated by the condition
    // (`cond ? high : na`); a bare bool series has no price to place and
    // projects to nothing.
    let plugin = linked(
        "indicator(\"T\", overlay=true)\nplot(close, \"A\")\nplotshape(close > open ? high : na, style=shape.triangleup)\nhline(100, \"base\")\n",
    );
    let evaluation = plugin
        .evaluate(
            &rows(&[100.0, 101.0, 99.0]),
            &PineRuntimeConfig::new().build(),
            &series_catalog(),
        )
        .expect("drawings evaluate");
    assert_eq!(evaluation.series.len(), 1, "the plot still paints");
    let overlay = evaluation.overlay.expect("drawings project");
    assert!(
        overlay
            .iter()
            .any(|primitive| matches!(primitive, protopyne_kernels::PlotPrimitive::Marker { .. })),
        "plotshape becomes a marker: {overlay:?}"
    );
    assert!(
        overlay
            .iter()
            .any(|primitive| matches!(primitive, protopyne_kernels::PlotPrimitive::Level { .. })),
        "hline becomes a level: {overlay:?}"
    );
    assert!(
        !overlay
            .iter()
            .any(|primitive| matches!(primitive, protopyne_kernels::PlotPrimitive::Series { .. })),
        "series stay on the series path, never duplicated into the overlay"
    );
}

#[test]
fn unprojectable_constructs_refuse_the_overlay_but_keep_the_series() {
    let plugin = linked("indicator(\"T\", overlay=true)\nplot(close, \"A\")\nplotshape(close > open ? high : na)\nplotcandle(open, high, low, close)\n");
    let evaluation = plugin
        .evaluate(
            &rows(&[100.0, 101.0]),
            &PineRuntimeConfig::new().build(),
            &series_catalog(),
        )
        .expect("evaluation still folds");
    assert!(
        !evaluation.series.is_empty(),
        "captured plots still paint as series"
    );
    assert!(
        evaluation.overlay.is_err(),
        "plotcandle refuses the whole overlay instead of painting it partly"
    );
}

#[test]
fn pine_overlay_legacy_study_header_shapes() {
    // Legacy `study(...)` headers read overlay the same way: named flag,
    // positional third slot, or the Pine default (off).
    assert!(pine_overlay_of("study(\"T\", overlay=true)\nplot(close)"));
    assert!(!pine_overlay_of(
        "study(shorttitle=\"S\", title=\"T\", overlay=false)\nplot(close)"
    ));
    assert!(pine_overlay_of("study('T', 'T', true)\nplot(close)"));
    assert!(!pine_overlay_of("study(\"T\")\nplot(close)"));
}

#[test]
fn terminal_fixed_corpus_runs_through_the_compiled_path() {
    // Terminal wiring proof over a fixed inline corpus (never the volatile
    // live root): every script links and evaluates through
    // `PineIndicatorPlugin` — which constructs `PineEngine::new` with no
    // mode override, i.e. the default-compiled loop — and refusals surface
    // the engine's reason instead of invented values.
    assert_eq!(
        protopyne_kernels::pine::BytecodeMode::default(),
        protopyne_kernels::pine::BytecodeMode::Compiled,
        "the plugin's default-constructed engine is the compiled loop"
    );
    type ExpectSeries = Vec<Vec<Option<f64>>>;
    struct TerminalCase {
        id: &'static str,
        script: &'static str,
        attachment: SlotAttachment,
        wants: ExpectSeries,
    }
    let cases = [
        TerminalCase {
            id: "pine:_TermOverlay",
            script: "indicator(\"T\", overlay=true)\nplot(close)\n",
            attachment: SlotAttachment::MainOverlay,
            wants: vec![vec![Some(100.0), Some(101.0), Some(102.0)]],
        },
        TerminalCase {
            id: "pine:_TermSubpane",
            script: "indicator(\"T\", overlay=false)\nplot(close * 2.0)\n",
            attachment: SlotAttachment::SubPane,
            wants: vec![vec![Some(200.0), Some(202.0), Some(204.0)]],
        },
        TerminalCase {
            id: "pine:_TermStudy",
            script: "study(\"S\", overlay=false)\nplot(close + 1.0)\n",
            attachment: SlotAttachment::SubPane,
            wants: vec![vec![Some(101.0), Some(102.0), Some(103.0)]],
        },
    ];
    let tape = rows(&[100.0, 101.0, 102.0]);
    for case in &cases {
        let plugin = PineIndicatorPlugin::link(
            case.id,
            case.id,
            case.script,
            &std::collections::HashMap::new(),
        )
        .expect("fixed corpus links");
        assert_eq!(
            plugin.attachment(),
            case.attachment,
            "{} attaches from its header",
            case.id
        );
        let evaluation = plugin
            .evaluate(&tape, &PineRuntimeConfig::new().build(), &series_catalog())
            .expect("fixed corpus evaluates");
        assert_eq!(
            evaluation.series.len(),
            case.wants.len(),
            "{} publishes one series per plot",
            case.id
        );
        for (series, want) in evaluation.series.iter().zip(case.wants.iter()) {
            assert_eq!(&series.values, want, "{} paints compiled values", case.id);
        }
    }
    // A script that cannot run headlessly refuses with the engine's
    // reason — the slot paints nothing invented.
    let plugin = PineIndicatorPlugin::link(
        "pine:_TermRefuse",
        "_TermRefuse",
        "indicator(\"T\")\nx = nosuchfn(close)\nplot(x)\n",
        &std::collections::HashMap::new(),
    )
    .expect("unrunnable text still links");
    let err = plugin
        .evaluate(&tape, &PineRuntimeConfig::new().build(), &series_catalog())
        .expect_err("the engine refusal surfaces");
    assert!(
        err.to_string().contains("nosuchfn"),
        "the refusal names the engine's reason: {err}"
    );
}
