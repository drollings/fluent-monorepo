//! Backtest overlay geometry: markers land on their bars, holes break
//! lines, a flat run paints no line, and an off-window marker paints
//! nothing.

use chrono::{TimeZone, Utc};
use fluent_egui::{sampled_line_points, Viewport};
use protopyne_chart_model::ChartRow;
use protopyne_chart_scale::PriceScaleMode;
use protopyne_view::{
    BacktestMarkerView, BacktestMetricView, BacktestRunView, BacktestStatusView, BacktestTradeView,
    EquityPointView,
};

fn marker(kind: &str, at: String, price: &str) -> BacktestMarkerView {
    BacktestMarkerView {
        at,
        price: price.to_owned(),
        kind: kind.to_owned(),
    }
}

use super::{
    BacktestOverlay, BacktestPointKind, EquityPane, BACKTEST_EQUITY_ID, BACKTEST_EQUITY_PANE,
    BACKTEST_OVERLAY_ID,
};
use crate::indicators::ChartIndicator;

fn bar_time(index: usize) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 5, 15, 0, 0)
        .single()
        .expect("time")
        + chrono::Duration::minutes(index as i64)
}

fn rows(closes: &[f64]) -> Vec<ChartRow> {
    closes
        .iter()
        .enumerate()
        .map(|(index, close)| ChartRow {
            time: bar_time(index).timestamp(),
            open: close - 0.2,
            high: close + 0.5,
            low: close - 0.5,
            close: *close,
            volume: 1_000,
        })
        .collect()
}

fn view() -> BacktestRunView {
    let at = |index: usize| bar_time(index).to_rfc3339();
    BacktestRunView {
        run_id: "backtest-07".to_owned(),
        manifest_json: "{}".to_owned(),
        metrics: vec![BacktestMetricView {
            label: "fills".to_owned(),
            value: Some("1".to_owned()),
        }],
        equity: vec![
            EquityPointView {
                at: at(0),
                equity: "100000".to_owned(),
            },
            EquityPointView {
                at: at(1),
                equity: "100050".to_owned(),
            },
            EquityPointView {
                at: at(2),
                equity: "99900".to_owned(),
            },
        ],
        trades: vec![BacktestTradeView {
            long: true,
            quantity: 10,
            entry_at: at(0),
            entry_price: "500.00".to_owned(),
            exit_at: at(2),
            exit_price: "505.00".to_owned(),
            pnl: "50.00".to_owned(),
            bars_held: 3,
            exit_reason: "take-profit".to_owned(),
        }],
        markers: vec![
            marker("stop", at(0), "495.00"),
            marker("stop", at(1), "497.00"),
            marker("target", at(0), "510.00"),
            marker("target", at(1), "510.00"),
        ],
        status: None,
        warnings: vec![],
    }
}

fn viewport(rows: &[ChartRow]) -> Viewport {
    let lo = rows.iter().map(|row| row.low).fold(f64::INFINITY, f64::min);
    let hi = rows
        .iter()
        .map(|row| row.high)
        .fold(f64::NEG_INFINITY, f64::max);
    Viewport::new(PriceScaleMode::Normal, lo, hi, 200.0, 8.0, lo, false).expect("viewport builds")
}

#[test]
fn markers_land_on_their_named_bars() {
    let rows = rows(&[500.0, 502.0, 505.0]);
    let seen = view();
    let (overlay, _) = BacktestOverlay::with_run("backtest-07", &seen);
    let evaluation = overlay
        .evaluate(
            &rows,
            &crate::overlay::BacktestOverlayConfig {
                run_id: "backtest-07".to_owned(),
            },
            &crate::topology::SeriesCatalog::empty(),
        )
        .expect("evaluate reads its own run");
    let entry = evaluation
        .series
        .iter()
        .find(|series| series.node == "backtest:entry")
        .expect("entry series");
    assert_eq!(entry.values, vec![Some(500.0), None, None]);
    let exit = evaluation
        .series
        .iter()
        .find(|series| series.node == "backtest:exit")
        .expect("exit series");
    assert_eq!(exit.values, vec![None, None, Some(505.0)]);
}

#[test]
fn holes_break_lines_and_flat_bars_paint_nothing() {
    let rows = rows(&[500.0, 502.0, 505.0, 507.0]);
    let seen = view();
    let (overlay, _) = BacktestOverlay::with_run("backtest-07", &seen);
    let evaluation = overlay
        .evaluate(
            &rows,
            &crate::overlay::BacktestOverlayConfig {
                run_id: "backtest-07".to_owned(),
            },
            &crate::topology::SeriesCatalog::empty(),
        )
        .expect("evaluate reads its own run");
    // The stop rests on bars 0-1 only: bar 2 carries no point, so the run
    // holds no stop there and the polyline must break instead of bridging.
    let stop = evaluation
        .series
        .iter()
        .find(|series| series.node == "backtest:stop")
        .expect("stop series");
    assert_eq!(
        stop.values,
        vec![Some(495.0), Some(497.0), None, None],
        "flat bars carry no resting level"
    );
    let placed = sampled_line_points(&stop.values, &viewport(&rows), 0);
    assert!(placed[2].is_none(), "the hole survives the sampler");
    // Entry/exit markers are points: no dot where no trade printed.
    let entry = evaluation
        .series
        .iter()
        .find(|series| series.node == "backtest:entry")
        .expect("entry series");
    assert!(entry.values[3].is_none());
}

#[test]
fn a_flat_run_paints_no_line() {
    let rows = rows(&[500.0, 501.0, 502.0]);
    let mut seen = view();
    seen.trades.clear();
    seen.markers.clear();
    seen.equity.clear();
    let (overlay, _) = BacktestOverlay::with_run("backtest-07", &seen);
    let evaluation = overlay
        .evaluate(
            &rows,
            &crate::overlay::BacktestOverlayConfig {
                run_id: "backtest-07".to_owned(),
            },
            &crate::topology::SeriesCatalog::empty(),
        )
        .expect("evaluate reads its own run");
    for series in &evaluation.series {
        assert!(
            series.values.iter().all(Option::is_none),
            "a flat run paints no line: {}",
            series.node
        );
    }
}

#[test]
fn an_off_window_marker_paints_nothing() {
    use fluent_egui::{IndicatorStyle, ThemeConfig};
    let rows = rows(&[500.0, 502.0, 505.0]);
    let seen = view();
    let (overlay, _) = BacktestOverlay::with_run("backtest-07", &seen);
    let config = crate::overlay::BacktestOverlayConfig {
        run_id: "backtest-07".to_owned(),
    };
    let evaluation = overlay
        .evaluate(&rows, &config, &crate::topology::SeriesCatalog::empty())
        .expect("evaluate reads its own run");
    let tight = Viewport::new(
        PriceScaleMode::Normal,
        600.0,
        700.0,
        200.0,
        8.0,
        600.0,
        false,
    )
    .expect("tight viewport builds");
    // Every marker and level price sits below the window: the paint path
    // runs clean and places nothing.
    for price in [500.0, 505.0, 495.0, 510.0, 497.0] {
        assert!(
            tight.windowed_y(price).is_none(),
            "the control window excludes every run price"
        );
    }
    egui::__run_test_ui(|ui| {
        overlay
            .paint_main_overlay(
                ui.painter(),
                &evaluation,
                &tight,
                egui::Pos2::ZERO,
                0,
                200.0,
                &ThemeConfig::default(),
                &config,
                &IndicatorStyle::default(),
            )
            .expect("off-window paint skips, never fails");
    });
}

#[test]
fn the_equity_pane_owns_its_viewport_and_values() {
    let rows = rows(&[500.0, 502.0, 505.0]);
    let seen = view();
    let (pane, _) = EquityPane::with_run("backtest-07", &seen);
    assert_eq!(pane.id(), BACKTEST_EQUITY_ID);
    assert_eq!(pane.attachment(), fluent_egui::SlotAttachment::SubPane);
    let evaluation = pane
        .evaluate(
            &rows,
            &crate::overlay::BacktestOverlayConfig {
                run_id: "backtest-07".to_owned(),
            },
            &crate::topology::SeriesCatalog::empty(),
        )
        .expect("evaluate reads its own run");
    assert_eq!(evaluation.series.len(), 1);
    let equity = &evaluation.series[0];
    assert_eq!(equity.pane.as_deref(), Some(BACKTEST_EQUITY_PANE));
    assert_eq!(
        equity.values,
        vec![Some(100000.0), Some(100050.0), Some(99900.0)]
    );
}

#[test]
fn a_mismatched_run_id_paints_nothing() {
    let rows = rows(&[500.0, 502.0, 505.0]);
    let seen = view();
    let (overlay, _) = BacktestOverlay::with_run("backtest-07", &seen);
    let evaluation = overlay
        .evaluate(
            &rows,
            &crate::overlay::BacktestOverlayConfig {
                run_id: "backtest-08".to_owned(),
            },
            &crate::topology::SeriesCatalog::empty(),
        )
        .expect("mismatch fail-closes, never errors");
    assert!(
        evaluation.series.is_empty(),
        "a mislabelled run paints nothing"
    );
}

#[test]
fn the_overlay_ids_are_stable() {
    assert_eq!(BACKTEST_OVERLAY_ID, "backtest_overlay");
    assert_eq!(BACKTEST_EQUITY_ID, "backtest_equity");
}

#[test]
fn the_status_table_paints_headlessly() {
    let status = BacktestStatusView {
        at: "2026-01-05T15:05:00+00:00".to_owned(),
        position: "Flat".to_owned(),
        day_pnl_pct: "0.05%".to_owned(),
        trades_today: "1".to_owned(),
        loss_streak: 0,
        cooldown: "Ready".to_owned(),
        status: "Active".to_owned(),
        win_rate: "100.0%".to_owned(),
        net_pnl: "50.00".to_owned(),
    };
    egui::__run_test_ui(|ui| {
        crate::widgets::paint_status_table(ui, &status);
        crate::widgets::paint_mtf_table(
            ui,
            &[crate::widgets::MtfRow {
                timeframe: "M15".to_owned(),
                bias: "bull".to_owned(),
                strength: 1.5,
            }],
        );
    });
}

#[test]
fn points_reject_an_unparseable_price() {
    assert!(
        super::parse_point("2026-01-05T15:00:00+00:00", "nan", BacktestPointKind::Entry).is_none()
    );
    assert!(super::parse_point("not-a-time", "500.00", BacktestPointKind::Entry).is_none());
}

#[test]
fn duplicate_timestamps_bind_nothing() {
    let mut dup = rows(&[500.0, 502.0, 505.0]);
    dup[2].time = dup[0].time;
    let seen = view();
    let (overlay, _) = BacktestOverlay::with_run("backtest-07", &seen);
    let evaluation = overlay
        .evaluate(
            &dup,
            &crate::overlay::BacktestOverlayConfig {
                run_id: "backtest-07".to_owned(),
            },
            &crate::topology::SeriesCatalog::empty(),
        )
        .expect("evaluate reads its own run");
    let entry = evaluation
        .series
        .iter()
        .find(|series| series.node == "backtest:entry")
        .expect("entry series");
    // Bar 0's timestamp repeats at bar 2: neither binds the entry marker.
    assert_eq!(entry.values, vec![None, None, None]);
}
