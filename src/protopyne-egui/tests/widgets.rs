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
        let mut press = PlayPressState::default();
        let mut speed = ReplaySpeed::default();
        let mut symbol_anchor = None;
        let mut indicator_anchor = None;
        let mut bookmark_anchor = None;
        let mut jump_anchor = None;
        raised = paint_top_bar(
            ui,
            "SPY",
            TimeframeKey::default(),
            ChartType::Candles,
            true,
            false,
            &mut press,
            &mut speed,
            &mut symbol_anchor,
            &mut indicator_anchor,
            &mut bookmark_anchor,
            &mut jump_anchor,
        );
    });
    assert!(
        raised.is_none(),
        "an untouched top bar raises no action: {raised:?}"
    );
    assert_eq!(SYMBOL_SEARCH_LABEL, "Symbols");
    assert_eq!(INDICATOR_SEARCH_LABEL, "Indicators");
    assert_eq!(BOOKMARK_SEARCH_LABEL, "Bookmarks");
    assert_eq!(
        TOP_BAR_CHART_TYPES.len(),
        3,
        "the workspace offers candles, line, and Heikin"
    );
    for kind in TOP_BAR_CHART_TYPES {
        assert!(!chart_type_label(kind).is_empty(), "{kind:?} has a label");
    }
    assert_eq!(chart_type_label(ChartType::Candles), "Candles");
    assert_eq!(chart_type_label(ChartType::Heikin), "Heikin-Ashi");
    // The timeframe selector renders straight from the canonical list and the
    // dock range section offers the coarse chart presets on demand.
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
        "the range section offers year-to-date"
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
fn range_picker_paints_headlessly() {
    let mut draft = Some(DateRange::Month3);
    let mut beginning = String::new();
    let mut calendar = CalendarState::today();
    egui::__run_test_ui(|ui| {
        let choice = paint_range_dialog(
            ui,
            &mut draft,
            &mut beginning,
            &mut calendar,
            Some(DateRange::Month1),
        );
        assert!(choice.is_none(), "an untouched range picker raises nothing");
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
            "test-calendar",
            &mut state,
            Some(chrono::NaiveDate::from_ymd_opt(2026, 3, 2).expect("fixture date")),
        );
        assert!(picked.is_none(), "an untouched calendar picks nothing");
    });
}

#[test]
fn top_bar_carries_play_state_headlessly() {
    for playing in [false, true] {
        let mut raised: Option<TopBarAction> = None;
        egui::__run_test_ui(|ui| {
            let mut press = PlayPressState::default();
            let mut speed = ReplaySpeed::default();
            let mut symbol_anchor = None;
            let mut indicator_anchor = None;
            let mut bookmark_anchor = None;
            let mut jump_anchor = None;
            raised = paint_top_bar(
                ui,
                "SPY",
                TimeframeKey::default(),
                ChartType::Candles,
                false,
                playing,
                &mut press,
                &mut speed,
                &mut symbol_anchor,
                &mut indicator_anchor,
                &mut bookmark_anchor,
                &mut jump_anchor,
            );
        });
        assert!(
            raised.is_none(),
            "an untouched top bar raises no action: {raised:?}"
        );
    }
    // Two calendars on the same month must not share a grid id: paint both
    // with their distinct salts and require no misfire.
    egui::__run_test_ui(|ui| {
        let mut start_cal = CalendarState {
            year: 2026,
            month: 3,
        };
        let mut end_cal = CalendarState {
            year: 2026,
            month: 3,
        };
        let selected = chrono::NaiveDate::from_ymd_opt(2026, 3, 2);
        let first = paint_calendar(ui, "replay-start", &mut start_cal, selected);
        let second = paint_calendar(ui, "replay-end", &mut end_cal, selected);
        assert!(first.is_none() && second.is_none());
    });
}

#[test]
fn play_press_holds_open_the_speed_popup_and_consumes_the_release() {
    let mut press = PlayPressState::default();
    let t0 = 1000.0_f64;
    // A short tap toggles: the release consumes nothing.
    press.press_started(t0);
    assert!(!press.poll_hold(t0 + 0.1));
    assert!(press.release(true), "a short tap toggles play");
    // A hold past the threshold opens the popup once; the release that ends
    // it is consumed instead of toggling.
    press.press_started(t0);
    assert!(!press.poll_hold(t0 + 0.1));
    assert!(press.poll_hold(t0 + PLAY_LONG_PRESS_SECS));
    assert!(!press.poll_hold(t0 + PLAY_LONG_PRESS_SECS + 1.0));
    assert!(press.popup_open, "the hold opens the speed popup");
    assert!(
        !press.release(true),
        "the long-press release toggles nothing"
    );
    assert!(!press.long_fired, "the consume resets the latch");
    // Releasing anywhere else arms nothing: the next tap still toggles.
    press.long_fired = true;
    assert!(!press.release(false), "a release elsewhere toggles nothing");
    press.press_started(t0);
    assert!(press.release(true), "a later tap toggles again");
}
