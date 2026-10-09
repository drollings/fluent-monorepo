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
