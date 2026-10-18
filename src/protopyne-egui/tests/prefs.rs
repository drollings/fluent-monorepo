use super::*;

fn ramp(count: i64) -> Vec<ChartRow> {
    (0..count)
        .map(|i| {
            let close = 100.0 + i as f64;
            ChartRow {
                time: 1_700_000_000 + i * 300,
                open: close - 1.0,
                high: close + 1.0,
                low: close - 2.0,
                close,
                volume: 10,
            }
        })
        .collect()
}

/// The persisted document round-trips: favorites, the session selection, the
/// chart's non-default settings, and a loadout field all restore onto a fresh
/// workspace, and a second snapshot equals the first.
#[test]
fn prefs_round_trip_restores_favorites_loadout_and_settings() {
    let mut source = ChartWorkspace::with_rows(ramp(120)).expect("workspace boots");
    source.favorite_symbols.insert("SPY".to_owned());
    source.favorite_symbols.insert("QQQ".to_owned());
    source.favorite_indicators.insert("fast_mas".to_owned());
    source.chart_type = ChartType::Line;
    source.pane_share = 0.6;
    source.dock_open = true;
    source
        .set_slot_style_field("fast_mas", "line_width", "3")
        .expect("a style field accepts");
    let prefs = source.prefs();

    let mut restored = ChartWorkspace::with_rows(ramp(120)).expect("workspace boots");
    restored.apply_prefs(&prefs).expect("prefs apply");
    assert_eq!(
        restored.favorite_symbols, source.favorite_symbols,
        "favorites restore"
    );
    assert_eq!(restored.favorite_indicators, source.favorite_indicators);
    assert_eq!(
        restored.chart_type,
        ChartType::Line,
        "the chart type restores"
    );
    assert!(
        (restored.pane_share - 0.6).abs() < 1e-6,
        "the pane share restores"
    );
    assert!(restored.dock_open, "the dock state restores");
    assert_eq!(
        restored
            .slot_style_field("fast_mas", "line_width")
            .expect("style reads"),
        "3",
        "the loadout style field restores"
    );
    assert_eq!(restored.prefs(), prefs, "a restored round trip is stable");
}

/// An unknown timeframe, chart type, or indicator id in a partial document is
/// skipped, never fatal: a stale file must not strand the terminal.
#[test]
fn prefs_apply_skips_unknown_fields_instead_of_failing() {
    let mut workspace = ChartWorkspace::with_rows(ramp(60)).expect("workspace boots");
    let prefs = TerminalPrefs {
        symbol: Some("SPY".to_owned()),
        timeframe: Some("9X".to_owned()),
        chart_type: Some("Not A Type".to_owned()),
        range: Some("1Y".to_owned()),
        ..TerminalPrefs::default()
    };
    workspace
        .apply_prefs(&prefs)
        .expect("an unknown field is skipped");
    assert_eq!(
        workspace.timeframe(),
        TimeframeKey::default(),
        "unknown timeframe skipped"
    );
    assert_eq!(workspace.range(), DateRange::Year1, "a known range applies");
}

/// The prefs path anchors beside the historical root, so the file lands next
/// to the historical/indicator/library folders: the shipped
/// `data/protopyne-rs/` tree carries `indicators/` and `libraries/` beside
/// `historical/`.
#[test]
fn prefs_path_anchors_at_the_data_root() {
    let historical =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs/historical");
    let data_root = historical.parent().expect("historical has a parent");
    assert_eq!(
        crate::prefs::prefs_path(&historical),
        data_root.join("protopyne.json"),
        "the file lands beside the historical/indicator/library roots"
    );
    assert_eq!(
        crate::prefs::prefs_path(&historical.join("SPY_1M.csv")),
        data_root.join("protopyne.json"),
        "a file entry point anchors the same way"
    );
}
