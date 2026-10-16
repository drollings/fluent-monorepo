use super::*;
use chrono::TimeZone as _;
use fluent_egui::Viewport;
use protopyne_chart_model::{ChartRow, DailySessionProvider, find_session_breaks};
use protopyne_chart_scale::PriceScaleMode;
use protopyne_types::Bar;
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive as _;

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

fn bar_at_utc(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    price: f64,
) -> Bar {
    let px = Decimal::from_f64(price).expect("valid test decimal");
    Bar::new()
        .ts(
            chrono::Utc
                .with_ymd_and_hms(year, month, day, hour, 0, 0)
                .single()
                .expect("valid time"),
        )
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
    let lines = break_lines(&pane, &bars, &provider);
    let scanned = find_session_breaks(&bars, &provider);
    assert_eq!(lines.len(), scanned.len(), "every scanned break gets exactly one gridline");
    assert_eq!(lines.len(), 1, "one midnight boundary in this fixture");
    for (line, (index, _)) in lines.iter().zip(scanned.iter()) {
        assert_eq!(line.index, *index);
        // The gridline sits on the scanned break's slot: the slot comes
        // from the independent scan, the spacing from the documented bar-zero
        // contract — never a second mapping call.
        assert_eq!(line.x.to_bits(), (*index as f32 * 8.0).to_bits());
    }
    assert!(break_lines(&pane, &[], &provider).is_empty());
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
    assert_eq!(hour_lines[0].index, 2, "the line sits on the first bar of the new hour");
    assert_eq!(hour_lines[0].x.to_bits(), (2.0_f32 * 8.0).to_bits(), "x rides the shared mapping");

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
    assert!(time_grid_lines(&pane, &same_hour, 300).is_empty(), "no boundary inside one hour");
}

/// The major unit is always the narrowest nice interval at or above five
/// candles: `15M` grids four-hourly, hourly grids daily, and a one-minute
/// chart still grids hourly rather than on a unit below its own timeframe.
#[test]
fn time_major_unit_stays_above_the_chart_timeframe() {
    assert_eq!(time_major_unit(60), 3_600, "1M bars grid hourly (60 candles)");
    assert_eq!(time_major_unit(300), 3_600, "5M bars grid hourly (12 candles)");
    assert_eq!(time_major_unit(900), 14_400, "15M bars grid four-hourly (16 candles)");
    assert_eq!(time_major_unit(3_600), 86_400, "1H bars grid daily (24 candles)");
    assert_eq!(time_major_unit(14_400), 86_400, "4H bars grid daily (6 candles)");
    assert_eq!(time_major_unit(86_400), 604_800, "daily bars grid weekly (7 candles)");
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
        assert!(!labels_show_clock_time(bar_seconds), "{bar_seconds}s bars label by date");
    }
}
