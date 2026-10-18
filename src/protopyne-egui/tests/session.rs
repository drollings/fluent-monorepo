use super::*;
use chrono::TimeZone as _;
use fluent_egui::Viewport;
use protopyne_chart_model::{find_session_breaks, ChartRow, DailySessionProvider};
use protopyne_chart_scale::PriceScaleMode;
use protopyne_types::Bar;
use rust_decimal::prelude::FromPrimitive as _;
use rust_decimal::Decimal;

fn row_at_utc(year: i32, month: u32, day: u32, hour: u32, minute: u32, close: f64) -> ChartRow {
    ChartRow {
        time: chrono::Utc
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .expect("valid time")
            .timestamp(),
        open: close,
        high: close,
        low: close,
        close,
        volume: 100,
    }
}

fn bar_at_utc(year: i32, month: u32, day: u32, hour: u32, price: f64) -> Bar {
    let px = Decimal::from_f64(price).expect("valid test decimal");
    Bar::new()
        .ts(chrono::Utc
            .with_ymd_and_hms(year, month, day, hour, 0, 0)
            .single()
            .expect("valid time"))
        .o(px)
        .h(px)
        .l(px)
        .c(px)
        .volume(100_u64)
        .maybe_adj(None)
        .build()
}

#[test]
fn break_lines_align_with_the_canonical_scan() {
    let bars = vec![
        bar_at_utc(2026, 1, 2, 12, 100.0),
        bar_at_utc(2026, 1, 2, 20, 101.0),
        bar_at_utc(2026, 1, 3, 1, 102.0),
        bar_at_utc(2026, 1, 3, 12, 103.0),
    ];
    let provider = DailySessionProvider::continuous();
    let pane = Viewport::new(PriceScaleMode::Normal, 90.0, 110.0, 400.0, 8.0, 90.0, false)
        .expect("valid viewport");
    let mut cache = SessionBreakCache::new();
    let lines = break_lines(&mut cache, &pane, &bars, &provider);
    let scanned = find_session_breaks(&bars, &provider);
    assert_eq!(
        lines.len(),
        scanned.len(),
        "every scanned break gets exactly one gridline"
    );
    assert_eq!(lines.len(), 1, "one midnight boundary in this fixture");
    for (line, (index, _)) in lines.iter().zip(scanned.iter()) {
        assert_eq!(line.index, *index);
        // The gridline sits on the scanned break's slot: the slot comes
        // from the independent scan, the spacing from the documented bar-zero
        // contract — never a second mapping call.
        assert_eq!(line.x.to_bits(), (*index as f32 * 8.0).to_bits());
    }
    assert!(break_lines(&mut cache, &pane, &[], &provider).is_empty());
}

#[test]
fn session_break_cache_matches_a_fresh_scan() {
    let bars = vec![
        bar_at_utc(2026, 1, 2, 12, 100.0),
        bar_at_utc(2026, 1, 2, 20, 101.0),
        bar_at_utc(2026, 1, 3, 12, 102.0),
    ];
    let provider = DailySessionProvider::continuous();
    let pane = Viewport::new(PriceScaleMode::Normal, 90.0, 110.0, 400.0, 8.0, 90.0, false)
        .expect("valid viewport");
    let mut cache = SessionBreakCache::new();
    cache.refresh(&bars, &provider);
    assert_eq!(
        cache.breaks(),
        find_session_breaks(&bars, &provider).as_slice()
    );
    let lines = cache.lines(&pane);
    assert_eq!(
        lines.len(),
        cache.breaks().len(),
        "one gridline per cached break"
    );
    for (line, (index, _)) in lines.iter().zip(cache.breaks().iter()) {
        assert_eq!(line.index, *index);
        assert_eq!(line.x.to_bits(), (*index as f32 * 8.0).to_bits());
    }
}

#[test]
fn identical_republish_skips_the_scan() {
    let bars = vec![
        bar_at_utc(2026, 1, 2, 12, 100.0),
        bar_at_utc(2026, 1, 2, 20, 101.0),
        bar_at_utc(2026, 1, 3, 12, 102.0),
    ];
    let provider = DailySessionProvider::continuous();
    let mut cache = SessionBreakCache::new();
    let first = cache.refresh(&bars, &provider);
    assert_eq!(cache.rebuilds(), 1, "the first refresh scans");
    // A republish of equal content keeps the revision and never re-scans —
    // the key is the content, not the slice's identity.
    assert_eq!(cache.refresh(&bars, &provider), first);
    let equal_clone = bars.clone();
    assert_eq!(cache.refresh(&equal_clone, &provider), first);
    assert_eq!(cache.rebuilds(), 1, "identical content skips the scan");
}

#[test]
fn a_changed_interior_recomputes_despite_fixed_endpoints() {
    // Two endpoints that never move; the interior changes a day so the break
    // count moves 1 -> 2. A two-endpoint key would serve the stale scan.
    let provider = DailySessionProvider::continuous();
    let mut bars = vec![
        bar_at_utc(2026, 1, 1, 12, 100.0),
        bar_at_utc(2026, 1, 1, 20, 101.0),
        bar_at_utc(2026, 1, 3, 12, 102.0),
    ];
    let mut cache = SessionBreakCache::new();
    let first = cache.refresh(&bars, &provider);
    assert_eq!(cache.breaks().len(), 1, "day 1 -> day 3 is one break");
    // Same first and last bars; the middle bar moves from day 1 to day 2.
    bars[1] = bar_at_utc(2026, 1, 2, 12, 101.0);
    let second = cache.refresh(&bars, &provider);
    assert_ne!(first, second, "changed content bumps the revision");
    assert_eq!(cache.rebuilds(), 2);
    assert_eq!(
        cache.breaks().len(),
        2,
        "day 1 -> day 2 -> day 3 is two breaks"
    );
}

#[test]
fn time_grid_lines_adapt_the_unit_to_the_window_span() {
    let pane = Viewport::new(PriceScaleMode::Normal, 90.0, 110.0, 400.0, 8.0, 90.0, false)
        .expect("valid viewport");

    // Five-minute bars grid at the hour boundary: only 11:50 -> 12:00 crosses.
    let intraday = vec![
        row_at_utc(2026, 1, 2, 11, 45, 100.0),
        row_at_utc(2026, 1, 2, 11, 50, 101.0),
        row_at_utc(2026, 1, 2, 12, 0, 102.0),
        row_at_utc(2026, 1, 2, 12, 5, 103.0),
    ];
    let hour_lines = time_grid_lines(&pane, &intraday, 300);
    assert_eq!(hour_lines.len(), 1, "one hour boundary");
    assert_eq!(
        hour_lines[0].index, 2,
        "the line sits on the first bar of the new hour"
    );
    assert_eq!(
        hour_lines[0].x.to_bits(),
        (2.0_f32 * 8.0).to_bits(),
        "x rides the shared mapping"
    );

    // Hourly bars grid at day boundaries instead.
    let multiday = vec![
        row_at_utc(2026, 1, 1, 12, 0, 100.0),
        row_at_utc(2026, 1, 2, 12, 0, 101.0),
        row_at_utc(2026, 1, 3, 12, 0, 102.0),
        row_at_utc(2026, 1, 4, 12, 0, 103.0),
    ];
    let day_lines = time_grid_lines(&pane, &multiday, 3_600);
    assert_eq!(
        day_lines.iter().map(|line| line.index).collect::<Vec<_>>(),
        vec![1, 2, 3],
        "every day change is a line"
    );

    // Fewer than two rows, or rows that never cross a unit, yield no lines.
    assert!(time_grid_lines(&pane, &[], 300).is_empty());
    assert!(time_grid_lines(&pane, &intraday[..1], 300).is_empty());
    let same_hour = vec![
        row_at_utc(2026, 1, 2, 12, 0, 100.0),
        row_at_utc(2026, 1, 2, 12, 5, 101.0),
    ];
    assert!(
        time_grid_lines(&pane, &same_hour, 300).is_empty(),
        "no boundary inside one hour"
    );
}

/// The major unit is the next sensible boundary above the bar's own
/// magnitude: one- and five-minute bars grid hourly, quarter-hour through
/// hourly bars every four hours, a four-hour chart daily, and a daily chart
/// weekly. A single candles-per-unit threshold cannot express this ladder, so
/// the mapping is stated directly.
#[test]
fn time_major_unit_stays_above_the_chart_timeframe() {
    assert_eq!(time_major_unit(60), 3_600, "1M bars grid hourly");
    assert_eq!(time_major_unit(300), 3_600, "5M bars grid hourly");
    assert_eq!(time_major_unit(900), 14_400, "15M bars grid four-hourly");
    assert_eq!(time_major_unit(3_600), 14_400, "1H bars grid four-hourly");
    assert_eq!(time_major_unit(14_400), 86_400, "4H bars grid daily");
    assert_eq!(time_major_unit(86_400), 604_800, "daily bars grid weekly");
}

/// A boundary label reads at the detail of the largest unit that has passed:
/// clock time for an hourly grid, date and clock time for a daily grid, and
/// the date alone for a weekly grid.
#[test]
fn boundary_labels_take_their_detail_from_the_grid_unit() {
    use chrono::TimeZone as _;
    let hour = chrono::Utc
        .with_ymd_and_hms(2026, 3, 2, 15, 0, 0)
        .single()
        .expect("time");
    assert_eq!(fmt_boundary_time(hour.timestamp(), 3_600), "15:00");
    assert_eq!(fmt_boundary_time(hour.timestamp(), 14_400), "15:00");
    assert_eq!(fmt_boundary_time(hour.timestamp(), 86_400), "03/02 15:00");
    assert_eq!(fmt_boundary_time(hour.timestamp(), 604_800), "03/02");
}

/// Time-axis labels follow the chart timeframe, never the window span: any
/// sub-daily timeframe reads hour and minute down in the strip, while daily
/// bars and coarser read calendar dates.
#[test]
fn time_labels_follow_the_timeframe_not_the_window_span() {
    for bar_seconds in [60, 300, 900, 3_600, 14_400] {
        assert!(
            labels_show_clock_time(bar_seconds),
            "{bar_seconds}s bars label by clock time"
        );
    }
    for bar_seconds in [86_400, 604_800] {
        assert!(
            !labels_show_clock_time(bar_seconds),
            "{bar_seconds}s bars label by date"
        );
    }
}

/// The rewind/forward step unit steps up the calendar ladder as bars widen:
/// a 1M/5M chart moves a day, a 15M/1H chart a week, a 4H chart a month, and
/// a daily chart a year — each at least `PAN_UNIT_MIN_BARS` bars.
#[test]
fn pan_unit_steps_up_the_calendar_ladder() {
    assert_eq!(pan_unit_seconds(60), DAY_SECONDS, "1M bars step by a day");
    assert_eq!(pan_unit_seconds(300), DAY_SECONDS, "5M bars step by a day");
    assert_eq!(
        pan_unit_seconds(900),
        WEEK_SECONDS,
        "15M bars step by a week"
    );
    assert_eq!(
        pan_unit_seconds(3_600),
        WEEK_SECONDS,
        "1H bars step by a week"
    );
    assert_eq!(
        pan_unit_seconds(14_400),
        MONTH_SECONDS,
        "4H bars step by a month"
    );
    assert_eq!(
        pan_unit_seconds(86_400),
        YEAR_SECONDS,
        "daily bars step by a year"
    );
    // A zero spacing refuses to the one-second floor instead of dividing by
    // zero, and still resolves to the narrowest unit covering the floor.
    assert_eq!(pan_unit_seconds(0), DAY_SECONDS);
    // Every step covers at least the minimum bar count.
    for bar_seconds in [60_u64, 300, 900, 3_600, 14_400, 86_400] {
        let unit = pan_unit_seconds(bar_seconds) as u64;
        assert!(
            unit / bar_seconds >= PAN_UNIT_MIN_BARS,
            "{bar_seconds}s bars step at least {PAN_UNIT_MIN_BARS} bars"
        );
    }
}

#[test]
fn timestamp_refresh_shares_the_memo_with_bar_refresh() {
    use protopyne_chart_model::session_breaks_between;
    let bars = vec![
        bar_at_utc(2026, 1, 2, 12, 100.0),
        bar_at_utc(2026, 1, 2, 20, 101.0),
        bar_at_utc(2026, 1, 3, 12, 102.0),
    ];
    let provider = DailySessionProvider::continuous();
    let times: Vec<_> = bars.iter().map(|bar| bar.ts).collect();
    let pane = Viewport::new(PriceScaleMode::Normal, 90.0, 110.0, 400.0, 8.0, 90.0, false)
        .expect("valid viewport");
    // Bar refresh and timestamp refresh agree break for break: one core,
    // two entries.
    let mut by_bars = SessionBreakCache::new();
    let bar_lines = break_lines(&mut by_bars, &pane, &bars, &provider);
    let mut by_times = SessionBreakCache::new();
    let time_lines = break_lines_times(&mut by_times, &pane, &times, &provider);
    assert_eq!(bar_lines, time_lines);
    assert_eq!(
        by_times.breaks(),
        session_breaks_between(&times, &provider).as_slice()
    );
    // The second entry rides the memo: same revision, no second scan.
    let revision = by_times.refresh_times(&times, &provider);
    assert_eq!(by_times.rebuilds(), 1, "identical content never rescans");
    assert_eq!(revision, by_times.revision());
    // A changed interior with fixed endpoints still recomputes.
    let mut moved = times.clone();
    moved[1] += chrono::Duration::days(1);
    by_times.refresh_times(&moved, &provider);
    assert_eq!(by_times.rebuilds(), 2);
}
