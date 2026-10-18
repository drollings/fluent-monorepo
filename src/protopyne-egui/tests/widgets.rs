use super::*;
use protopyne_chart_model::DateRange;

#[test]
fn mtf_shape_and_toolbar_labels_come_from_canonical_lists() {
    let rows = vec![
        MtfRow {
            timeframe: "M15".to_owned(),
            bias: "bull".to_owned(),
            strength: 120.0,
        },
        MtfRow {
            timeframe: "H1".to_owned(),
            bias: "flat".to_owned(),
            strength: 10.0,
        },
    ];
    assert_eq!(mtf_table_shape(&rows), (3, 2));
    assert_eq!(mtf_table_shape(&[]), (3, 0));
    let labels = toolbar_labels();
    let expected: Vec<&str> = DateRange::default_presets()
        .iter()
        .map(|preset| preset.label())
        .collect();
    assert_eq!(labels, expected);
    assert!(labels.contains(&"1M"));
}

#[test]
fn timeframe_labels_follow_key_order() {
    assert_eq!(
        timeframe_labels(),
        vec!["1M", "5M", "15M", "1H", "4H", "1D"]
    );
}

#[test]
fn mtf_table_renders_headlessly() {
    let rows = vec![MtfRow {
        timeframe: "15M".to_owned(),
        bias: "bull".to_owned(),
        strength: 12.0,
    }];
    egui::__run_test_ui(|ui| {
        paint_mtf_table(ui, &rows);
        paint_mtf_table(ui, &[]);
    });
}

#[test]
fn top_bar_renders_default_ticker_timeframes_and_search_triggers() {
    let mut raised: Option<TopBarAction> = None;
    egui::__run_test_ui(|ui| {
        raised = paint_top_bar(
            ui,
            "SPY",
            TimeframeKey::default(),
            "1M",
            ChartType::Candles,
            true,
        );
    });
    assert!(
        raised.is_none(),
        "an untouched top bar raises no action: {raised:?}"
    );
    assert_eq!(SYMBOL_SEARCH_LABEL, "Symbols");
    assert_eq!(INDICATOR_SEARCH_LABEL, "Indicators");
    assert_eq!(
        TOP_BAR_CHART_TYPES.len(),
        4,
        "the workspace offers candles, line, area, and Heikin"
    );
    for kind in TOP_BAR_CHART_TYPES {
        assert!(!chart_type_label(kind).is_empty(), "{kind:?} has a label");
    }
    assert_eq!(chart_type_label(ChartType::Candles), "Candles");
    assert_eq!(chart_type_label(ChartType::Heikin), "Heikin");
    // The timeframe selector renders straight from the canonical list and the
    // range menu offers the coarse chart presets on demand.
    assert_eq!(
        timeframe_labels(),
        vec!["1M", "5M", "15M", "1H", "4H", "1D"]
    );
    let presets: Vec<&str> = DateRange::chart_presets()
        .iter()
        .map(|preset| preset.label())
        .collect();
    assert_eq!(presets, vec!["1M", "3M", "YTD", "1Y", "5Y", "All"]);
    assert!(
        presets.contains(&"YTD"),
        "the range menu offers year-to-date"
    );
}

#[test]
fn calendar_state_navigates_months_and_lays_out_weekday_cells() {
    // March 2026 starts on a Sunday: six Monday-first lead cells; 31 days.
    let mut state = CalendarState {
        year: 2026,
        month: 3,
    };
    let cells = state.cells();
    assert_eq!(cells.iter().filter(|cell| cell.is_some()).count(), 31);
    assert_eq!(cells.iter().take_while(|cell| cell.is_none()).count(), 6);
    assert_eq!(cells.iter().flatten().next(), Some(&1));
    state.shift(-3);
    assert_eq!(
        (state.year, state.month),
        (2025, 12),
        "stepping back crosses the year"
    );
    state.shift(2);
    assert_eq!((state.year, state.month), (2026, 2));
    assert_eq!(
        CalendarState {
            year: 2024,
            month: 2
        }
        .cells()
        .iter()
        .flatten()
        .count(),
        29,
        "leap February"
    );
}

#[test]
fn range_dialog_paints_headlessly() {
    let mut draft = Some(DateRange::Month3);
    let mut beginning = String::new();
    let mut jump = String::new();
    let mut calendar = CalendarState::today();
    egui::__run_test_ui(|ui| {
        let choice = paint_range_dialog(
            ui,
            &mut draft,
            &mut beginning,
            &mut jump,
            &mut calendar,
            Some(DateRange::Month1),
        );
        assert!(choice.is_none(), "an untouched range dialog raises nothing");
    });
}

#[test]
fn top_bar_buttons_share_one_height_and_label_style() {
    // The constants are the single height/size every control reads; the
    // builder is the one label treatment. Bind them so the values are used.
    let (height, size) = (TOP_BAR_BUTTON_HEIGHT, TOP_BAR_FONT_SIZE);
    assert!(
        (height - 30.0).abs() < 1e-6,
        "the shared top-bar control height"
    );
    assert!((size - 15.0).abs() < 1e-6, "the shared top-bar label size");
    let _ = bar_text("Range");
}

#[test]
fn calendar_paints_headlessly() {
    let mut state = CalendarState {
        year: 2026,
        month: 3,
    };
    egui::__run_test_ui(|ui| {
        let picked = paint_calendar(
            ui,
            &mut state,
            Some(chrono::NaiveDate::from_ymd_opt(2026, 3, 2).expect("fixture date")),
        );
        assert!(picked.is_none(), "an untouched calendar picks nothing");
    });
}
