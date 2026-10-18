//! Chart-bar contract: the server route and the headless conversion are one
//! row shape, field for field.
//!
//! The wire row/container (`protopyne_view::ChartBar`/`ChartBars`) is the
//! single home. The server's route builder and
//! `protopyne_chart_model::rows_of` must produce the same container over the
//! same bars (no field-wise twin to drift), and the route JSON must decode
//! back into that container unchanged.

use super::chart_bars_of;
use chrono::TimeZone as _;
use protopyne_chart_model::{rows_of, to_display_float, ChartBars, ChartRow};
use protopyne_types::Bar;
use rust_decimal::Decimal;

fn dec(value: &str) -> Decimal {
    value.parse().expect("valid test decimal")
}

fn corpus() -> Vec<Bar> {
    (0..12u32)
        .map(|i| {
            let open = 100.0 + f64::from(i) * 0.5;
            let leg = |v: f64| dec(&format!("{v:.2}"));
            Bar::new()
                .ts(chrono::Utc
                    .timestamp_opt(1_700_000_000 + i64::from(i) * 60, 0)
                    .single()
                    .expect("valid time"))
                .o(leg(open))
                .h(leg(open + 1.0))
                .l(leg(open - 1.0))
                .c(leg(open + 0.5))
                .volume(1_000 + u64::from(i))
                .maybe_adj(None)
                .build()
        })
        .collect()
}

#[test]
fn route_bars_and_headless_rows_are_one_shape() {
    let bars = corpus();
    let route = chart_bars_of(&bars);
    let headless = rows_of(&bars);
    assert_eq!(
        route, headless,
        "route and headless build the same container"
    );
    assert_eq!(route.dropped, 0, "finite legs never drop");
    assert_eq!(route.rows.len(), bars.len());
    for (bar, row) in bars.iter().zip(route.rows.iter()) {
        assert_eq!(row.time, bar.ts.timestamp());
        assert_eq!(row.volume, bar.volume);
        for (got, want) in [
            (row.open, bar.o),
            (row.high, bar.h),
            (row.low, bar.l),
            (row.close, bar.c),
        ] {
            let want = to_display_float(want).expect("finite leg converts");
            assert_eq!(
                got.to_bits(),
                want.to_bits(),
                "leg round-trips bit-identically"
            );
        }
    }
}

#[test]
fn route_json_round_trips_through_the_row_container() {
    let route = chart_bars_of(&corpus());
    let json = serde_json::to_string(&route).expect("serialize the route payload");
    assert!(
        json.contains("\"rows\""),
        "the container keeps its wire keys: {json}"
    );
    assert!(
        json.contains("\"dropped\""),
        "the container keeps its wire keys: {json}"
    );
    let back: ChartBars = serde_json::from_str(&json).expect("decode into the wire container");
    assert_eq!(back, route, "the container JSON round-trips unchanged");
    let row_json = serde_json::to_string(&route.rows[0]).expect("serialize one row");
    let row: ChartRow = serde_json::from_str(&row_json).expect("decode one row");
    assert_eq!(row, route.rows[0], "the row JSON round-trips unchanged");
}
