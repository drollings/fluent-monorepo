use super::*;
use protopyne_chart_model::DateRange;

#[test]
fn floor_holds_for_small_needs() {
    // Day1 at H1 needs 24 bars, All at D1 needs 3650: both stay on the floor.
    assert_eq!(fetch_for_window(DateRange::Day1, 3_600, 0), 5_000);
    assert_eq!(fetch_for_window(DateRange::All, 86_400, 0), 5_000);
    // Warmup that still fits under the floor changes nothing.
    assert_eq!(fetch_for_window(DateRange::Day1, 3_600, 100), 5_000);
}

#[test]
fn month3_intraday_floors_pin_exact() {
    // 90 days = 7_776_000 seconds, divided by the bar spacing.
    assert_eq!(fetch_for_window(DateRange::Month3, 60, 0), 129_600);
    assert_eq!(fetch_for_window(DateRange::Month3, 300, 0), 25_920);
    assert_eq!(fetch_for_window(DateRange::Month3, 900, 0), 8_640);
}

#[test]
fn fetch_is_need_plus_warmup_not_more() {
    // Above the floor the fetch is exactly need + warmup: equality pins
    // the precision, not just a lower bound.
    assert_eq!(fetch_for_window(DateRange::Month3, 900, 500), 8_640 + 500);
    assert_eq!(fetch_for_window(DateRange::Month1, 60, 0), 43_200);
    // A small need plus a large warmup lands exactly past the floor.
    assert_eq!(fetch_for_window(DateRange::Day1, 60, 4_000), 1_440 + 4_000);
    // All at D1 needs 3650; warmup past the floor adds exactly.
    assert_eq!(
        fetch_for_window(DateRange::All, 86_400, 2_000),
        3_650 + 2_000
    );
}

#[test]
fn span_helper_is_the_one_sizing_rule() {
    // A custom Beginning span sizes through the same helper the presets use:
    // `fetch_for_window` is `fetch_for_span` once the preset's span is fixed,
    // so a pick and a preset can never plan different reads.
    for (range, spacing) in [
        (DateRange::Month1, 60_u64),
        (DateRange::Month3, 300),
        (DateRange::Year1, 900),
        (DateRange::All, 86_400),
    ] {
        assert_eq!(
            fetch_for_window(range, spacing, 200),
            fetch_for_span(range.span_seconds(), spacing, 200),
            "{range:?} at {spacing}s"
        );
    }
    // A three-month custom span at 5M equals the three-month preset need.
    assert_eq!(
        fetch_for_span(90 * 24 * 3_600, 300, 200),
        fetch_for_window(DateRange::Month3, 300, 200)
    );
}

#[test]
fn weekend_bias_documents_overfetch() {
    // 90 calendar days from a Monday hold 12 full weeks plus Mon-Sat:
    // 12 * 5 + 5 = 65 weekday sessions of 24 H1 bars = 1560 real bars.
    let weekday_h1 = (12 * 5 + 5) * 24;
    assert_eq!(weekday_h1, 1_560);
    let estimate = DateRange::Month3.estimated_bars(3_600);
    assert_eq!(estimate, 2_160);
    // The H1 need sits under the floor, so the fetch stays at the bound.
    assert_eq!(fetch_for_window(DateRange::Month3, 3_600, 0), 5_000);
    assert!(
        weekday_h1 < estimate,
        "calendar math over-fetches by the weekend bars, by design"
    );
}
