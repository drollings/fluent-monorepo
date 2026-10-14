use super::*;
use protopyne_chart_model::DateRange;

#[test]
fn mtf_shape_and_toolbar_labels_come_from_canonical_lists() {
    let rows = vec![
        MtfRow { timeframe: "M15".to_owned(), bias: "bull".to_owned(), strength: 120.0 },
        MtfRow { timeframe: "H1".to_owned(), bias: "flat".to_owned(), strength: 10.0 },
    ];
    assert_eq!(mtf_table_shape(&rows), (3, 2));
    assert_eq!(mtf_table_shape(&[]), (3, 0));
    let labels = toolbar_labels();
    let expected: Vec<&str> =
        DateRange::default_presets().iter().map(|preset| preset.label()).collect();
    assert_eq!(labels, expected);
    assert!(labels.contains(&"1M"));
}

#[test]
fn timeframe_labels_follow_key_order() {
    assert_eq!(timeframe_labels(), vec!["1M", "5M", "15M", "1H", "4H"]);
}

#[test]
fn mtf_table_renders_headlessly() {
    let rows =
        vec![MtfRow { timeframe: "15M".to_owned(), bias: "bull".to_owned(), strength: 12.0 }];
    egui::__run_test_ui(|ui| {
        paint_mtf_table(ui, &rows);
        paint_mtf_table(ui, &[]);
    });
}

#[test]
fn top_bar_renders_default_gld_ticker_timeframes_and_search_triggers() {
    let mut raised: Option<TopBarAction> = None;
    egui::__run_test_ui(|ui| {
        raised = paint_top_bar(ui, "GLD", TimeframeKey::default(), DateRange::All, ChartType::Candles);
    });
    assert!(raised.is_none(), "an untouched top bar raises no action: {raised:?}");
    assert_eq!(SYMBOL_SEARCH_LABEL, "Symbols");
    assert_eq!(INDICATOR_SEARCH_LABEL, "Indicators");
    assert_eq!(TOP_BAR_CHART_TYPES.len(), 3, "the workspace offers candles and the close line/area");
    for kind in TOP_BAR_CHART_TYPES {
        assert!(!chart_type_label(kind).is_empty(), "{kind:?} has a label");
    }
    assert_eq!(chart_type_label(ChartType::Candles), "Candles");
    // The selectors render straight from the canonical lists.
    assert_eq!(timeframe_labels(), vec!["1M", "5M", "15M", "1H", "4H"]);
    assert!(!toolbar_labels().is_empty(), "range presets render");
}
