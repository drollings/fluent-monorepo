use super::*;
use crate::chart::TimeframeKey;
use protopyne_chart_model::{ChartRow, DateRange};
use protopyne_view::{
    ArmResponse, ChartBars, GatewayError, HealthView, JournalRowView, OverlaySeries, PortfolioView,
    ProposalView, ReplayView, WorstCase,
};
use std::collections::BTreeMap;

/// A gateway whose bounded read always refuses, so switch paths must leave
/// the published triple untouched.
struct RefusingGateway;

fn refused() -> GatewayError {
    GatewayError::Unavailable("refusing gateway".to_owned())
}

impl protopyne_view::TerminalGateway for RefusingGateway {
    fn health(&self) -> Result<HealthView, GatewayError> {
        Err(refused())
    }
    fn proposals(&self) -> Result<Vec<ProposalView>, GatewayError> {
        Err(refused())
    }
    fn portfolio(&self) -> Result<PortfolioView, GatewayError> {
        Err(refused())
    }
    fn journal(&self) -> Result<Vec<JournalRowView>, GatewayError> {
        Err(refused())
    }
    fn chart_bars(&self) -> Result<ChartBars, GatewayError> {
        Err(refused())
    }
    fn chart_overlays(&self, _kind: &str, _period: i64) -> Result<OverlaySeries, GatewayError> {
        Err(refused())
    }
    fn replay(&self) -> Result<ReplayView, GatewayError> {
        Err(refused())
    }
    fn preview_arm(&self, _patch: serde_json::Value) -> Result<WorstCase, GatewayError> {
        Err(refused())
    }
    fn grant_tiers(&self) -> Result<BTreeMap<String, String>, GatewayError> {
        Err(refused())
    }
    fn arm(&self, _grant: serde_json::Value) -> Result<ArmResponse, GatewayError> {
        Err(refused())
    }
    fn chart_bars_for_limit(
        &self,
        symbol: &str,
        timeframe: &str,
        _limit: Option<usize>,
    ) -> Result<ChartBars, GatewayError> {
        protopyne_view::validate_symbol_timeframe(symbol, timeframe)?;
        Err(refused())
    }
}

fn switch_rows() -> Vec<ChartRow> {
    (0..40)
        .map(|i| {
            let close = 100.0 + (i as f64);
            ChartRow {
                time: 1_700_000_000 + i * 900,
                open: close - 1.0,
                high: close + 1.0,
                low: close - 2.0,
                close,
                volume: 10,
            }
        })
        .collect()
}

fn dated_switch_rows() -> Vec<ChartRow> {
    let base = chrono::NaiveDate::from_ymd_opt(2025, 1, 1)
        .expect("fixture date builds")
        .and_hms_opt(0, 0, 0)
        .map_or(0, |dt| dt.and_utc().timestamp());
    (0..120_i64)
        .map(|i| {
            let close = 100.0 + (i as f64);
            ChartRow {
                time: base + i * 86_400,
                open: close - 0.5,
                high: close + 0.5,
                low: close - 1.0,
                close,
                volume: 10,
            }
        })
        .collect()
}

#[test]
fn failed_switch_keeps_triple() {
    let rows = switch_rows();
    let mut workspace = ChartWorkspace::with_rows(rows.clone()).expect("workspace boots");
    let symbol = workspace.active_symbol().to_owned();
    let timeframe = workspace.timeframe();
    let gateway = RefusingGateway;

    assert!(
        workspace.switch_symbol("SPY", &gateway).is_err(),
        "refused fetch errors"
    );
    assert_eq!(workspace.active_symbol(), symbol, "the symbol label holds");
    assert_eq!(workspace.timeframe(), timeframe, "the timeframe holds");
    assert_eq!(workspace.rows(), &rows, "the rows hold");

    assert!(
        workspace
            .switch_timeframe(TimeframeKey::H1, &gateway)
            .is_err(),
        "refused fetch errors"
    );
    assert_eq!(workspace.active_symbol(), symbol, "the symbol label holds");
    assert_eq!(workspace.timeframe(), timeframe, "the timeframe holds");
    assert_eq!(workspace.rows(), &rows, "the rows hold");
}

#[test]
fn window_helper_single_source() {
    let mut workspace = ChartWorkspace::with_rows(dated_switch_rows()).expect("workspace boots");
    workspace
        .set_beginning("2025-02-01", None)
        .expect("date pick applies");
    let start = chrono::NaiveDate::from_ymd_opt(2025, 2, 1)
        .expect("fixture date builds")
        .and_hms_opt(0, 0, 0)
        .map_or(0, |dt| dt.and_utc().timestamp());
    let window = workspace.effective_window().to_vec();
    assert!(!window.is_empty(), "a mid-range start keeps a window");
    assert!(
        window.iter().all(|row| row.time >= start),
        "the helper filters the start"
    );
    let first = workspace
        .rows()
        .iter()
        .find(|row| row.time >= start)
        .expect("a bar lands on the start");
    assert_eq!(
        window.first(),
        Some(first),
        "the helper opens on the first covered bar"
    );
    assert_eq!(
        workspace.range(),
        DateRange::All,
        "a pick widens to the full span"
    );
    // The rebound viewport fits exactly the helper's window: one source for
    // the range, not a second filter kept in sync by hand.
    let viewport = workspace.main_viewport().expect("pick rebinds");
    let extent = price_extent(&window);
    assert_eq!(
        (viewport.scale().range().min, viewport.scale().range().max),
        (extent.0, extent.1),
        "rebind fits the helper window"
    );
}
