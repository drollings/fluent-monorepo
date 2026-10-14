use super::*;
use chrono::TimeZone as _;
use fluent_egui::Viewport;
use protopyne_chart_model::{DailySessionProvider, find_session_breaks};
use protopyne_chart_scale::PriceScaleMode;
use protopyne_types::Bar;
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive as _;

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
