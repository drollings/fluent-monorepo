use super::*;

#[test]
fn labels_match_toolbar_buttons() {
    assert_eq!(DateRange::Day1.label(), "1D");
    assert_eq!(DateRange::Month1.label(), "1M");
    assert_eq!(DateRange::All.label(), "All");
    assert_eq!(format!("{}", DateRange::Year1), "1Y");
}

#[test]
fn presets_cover_nine_windows() {
    assert_eq!(DateRange::default_presets().len(), 9);
    assert_eq!(DateRange::default(), DateRange::Month1);
}

#[test]
fn chart_presets_are_the_coarse_spans_in_display_order() {
    assert_eq!(
        DateRange::chart_presets(),
        vec![
            DateRange::Month1,
            DateRange::Month3,
            DateRange::Ytd,
            DateRange::Year1,
            DateRange::Year5,
            DateRange::All,
        ]
    );
    let labels: Vec<&str> = DateRange::chart_presets()
        .iter()
        .map(|preset| preset.label())
        .collect();
    assert_eq!(labels, vec!["1M", "3M", "YTD", "1Y", "5Y", "All"]);
    assert_eq!(DateRange::Month1.lookback_months(), Some(1));
    assert_eq!(DateRange::Ytd.lookback_months(), Some(12));
    assert_eq!(DateRange::Year1.lookback_months(), Some(12));
    assert_eq!(DateRange::All.lookback_months(), None);
}

#[test]
fn presets_share_one_span_definition() {
    // `estimated_bars` is exactly `span_seconds / spacing`, so the fetch
    // planner and the window math cannot drift apart about a preset's width.
    assert_eq!(DateRange::Month1.span_seconds(), MONTH_SECONDS as u64);
    assert_eq!(DateRange::Month3.span_seconds(), 3 * MONTH_SECONDS as u64);
    for preset in DateRange::default_presets() {
        for spacing in [60_u64, 300, 900, 3_600, 86_400] {
            assert_eq!(
                preset.estimated_bars(spacing),
                (preset.span_seconds() / spacing) as usize,
                "{preset:?} at {spacing}s"
            );
        }
    }
}

#[test]
fn all_has_no_start() {
    assert!(DateRange::All.start_timestamp().is_none());
    assert!(DateRange::Day1.start_timestamp().is_some());
    assert!(DateRange::Ytd.start_timestamp().is_some());
}

#[test]
fn estimates_scale_with_spacing() {
    let one_min = DateRange::Day1.estimated_bars(60);
    let five_min = DateRange::Day1.estimated_bars(300);
    assert_eq!(one_min, 1_440);
    assert_eq!(five_min, 288);
    assert!(one_min > five_min);
}

#[test]
fn zero_spacing_saturates_to_one_second() {
    assert_eq!(DateRange::Day1.estimated_bars(0), 24 * 3_600);
}
