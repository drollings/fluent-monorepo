use super::*;
use protopyne_chart_model::convert::{ChartRow, to_display_float};
use protopyne_view::{ChartBars, OverlaySeries};

/// Exact float comparison without tripping the strict-comparison lint:
/// the export round-trips must be bit-identical, not merely close.
fn assert_same_float(got: f64, want: f64, leg: &str) {
    assert_eq!(got.to_bits(), want.to_bits(), "{leg} round-trips bit-identically");
}

fn replay_rows() -> Vec<ChartRow> {
    (0..8)
        .map(|i| {
            let open = 100.0 + i as f64;
            ChartRow {
                time: 1_700_000_000 + i * 60,
                open,
                high: open + 1.0,
                low: open - 1.0,
                close: open + 0.5,
                volume: 1_000 + i as u64,
            }
        })
        .collect()
}

#[test]
fn export_refusals_stay_fail_closed() {
    let bad_row = ChartRow {
        time: 1,
        open: f64::NAN,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 1,
    };
    assert!(export_rows_csv(std::slice::from_ref(&bad_row)).is_err());
    assert!(export_rows_json(&[bad_row], 0).is_err());
    assert!(export_overlay_json("sma", 14, &[Some(f64::INFINITY)], 0).is_err());
    assert!(export_overlay_json("  ", 14, &[Some(1.0)], 0).is_err());
    assert!(export_overlay_json("sma", 14, &[None, Some(1.0)], 1).is_ok());
}

#[test]
fn csv_export_round_trips_through_the_server_ingest() {
    let rows = replay_rows();
    let text = export_rows_csv(&rows).expect("valid rows export");
    let header = text.lines().next().expect("header line");
    assert_eq!(header, EXPORT_CSV_HEADER);
    assert_eq!(header, "time,open,high,low,close,volume");
    let bars = protopyne_server::parse_bars_csv(&text).expect("server ingest reads our CSV");
    assert_eq!(bars.len(), rows.len());
    for (bar, row) in bars.iter().zip(rows.iter()) {
        assert_eq!(bar.ts.timestamp(), row.time);
        assert_same_float(to_display_float(bar.o).expect("finite leg"), row.open, "open");
        assert_same_float(to_display_float(bar.h).expect("finite leg"), row.high, "high");
        assert_same_float(to_display_float(bar.l).expect("finite leg"), row.low, "low");
        assert_same_float(to_display_float(bar.c).expect("finite leg"), row.close, "close");
        assert_eq!(bar.volume, row.volume);
    }
}

#[test]
fn rows_json_round_trips_through_the_server_shape() {
    let rows = replay_rows();
    let text = export_rows_json(&rows, 2).expect("valid rows export");
    let shaped: ChartBars = serde_json::from_str(&text).expect("server ChartBars reads our JSON");
    assert_eq!(shaped.dropped, 2);
    assert_eq!(shaped.rows.len(), rows.len());
    for (got, want) in shaped.rows.iter().zip(rows.iter()) {
        assert_eq!(got.time, want.time);
        assert_same_float(got.open, want.open, "open");
        assert_same_float(got.high, want.high, "high");
        assert_same_float(got.low, want.low, "low");
        assert_same_float(got.close, want.close, "close");
        assert_eq!(got.volume, want.volume);
    }
}

#[test]
fn overlay_json_round_trips_through_the_server_shape() {
    let values = vec![None, Some(1.5), Some(2.5), None];
    let text = export_overlay_json("sma", 14, &values, 1).expect("valid overlay exports");
    let shaped: OverlaySeries =
        serde_json::from_str(&text).expect("server OverlaySeries reads our JSON");
    assert_eq!(shaped.kind, "sma");
    assert_eq!(shaped.period, 14);
    assert_eq!(shaped.values, values);
    assert_eq!(shaped.dropped, 1);
}
