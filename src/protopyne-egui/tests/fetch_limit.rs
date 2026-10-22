use super::*;
use crate::chart::fetch_for_window;
use protopyne_chart_model::ChartRow;
use protopyne_view::{
    ArmResponse, ChartBar, ChartBars, GatewayError, HealthView, JournalRowView, OverlaySeries,
    PortfolioView, ProposalView, ReplayView, WorstCase,
};
use std::collections::BTreeMap;
use std::sync::Mutex;

fn m1_rows(count: usize) -> Vec<ChartRow> {
    let base = 1_700_000_000_i64;
    (0..count)
        .map(|i| {
            let close = 100.0 + i as f64 * 0.001;
            ChartRow {
                time: base + i as i64 * 60,
                open: close - 0.05,
                high: close + 0.05,
                low: close - 0.05,
                close,
                volume: 10,
            }
        })
        .collect()
}

/// A gateway holding a fixed history that honors `limit` as a trailing take
/// and records every bounded read, like the tape-backed server read.
struct LimitGateway {
    rows: Vec<ChartRow>,
    calls: Mutex<Vec<(String, String, Option<usize>)>>,
}

impl LimitGateway {
    fn new(rows: Vec<ChartRow>) -> Self {
        Self {
            rows,
            calls: Mutex::new(Vec::new()),
        }
    }

    fn limits_for(&self, timeframe: &str) -> Vec<Option<usize>> {
        self.calls
            .lock()
            .expect("call log locks")
            .iter()
            .filter(|(_, label, _)| label == timeframe)
            .map(|(_, _, limit)| *limit)
            .collect()
    }
}

fn unavailable() -> GatewayError {
    GatewayError::Unavailable("limit gateway".to_owned())
}

fn to_bars(rows: &[ChartRow]) -> ChartBars {
    ChartBars {
        rows: rows
            .iter()
            .map(|row| ChartBar {
                time: row.time,
                open: row.open,
                high: row.high,
                low: row.low,
                close: row.close,
                volume: row.volume,
            })
            .collect(),
        dropped: 0,
    }
}

impl protopyne_view::TerminalGateway for LimitGateway {
    fn health(&self) -> Result<HealthView, GatewayError> {
        Err(unavailable())
    }
    fn proposals(&self) -> Result<Vec<ProposalView>, GatewayError> {
        Err(unavailable())
    }
    fn portfolio(&self) -> Result<PortfolioView, GatewayError> {
        Err(unavailable())
    }
    fn journal(&self) -> Result<Vec<JournalRowView>, GatewayError> {
        Err(unavailable())
    }
    fn chart_bars(&self) -> Result<ChartBars, GatewayError> {
        Ok(to_bars(&self.rows))
    }
    fn chart_overlays(&self, _kind: &str, _period: i64) -> Result<OverlaySeries, GatewayError> {
        Err(unavailable())
    }
    fn replay(&self) -> Result<ReplayView, GatewayError> {
        Err(unavailable())
    }
    fn preview_arm(&self, _patch: serde_json::Value) -> Result<WorstCase, GatewayError> {
        Err(unavailable())
    }
    fn grant_tiers(&self) -> Result<BTreeMap<String, String>, GatewayError> {
        Err(unavailable())
    }
    fn arm(&self, _grant: serde_json::Value) -> Result<ArmResponse, GatewayError> {
        Err(unavailable())
    }
    fn chart_bars_for_limit(
        &self,
        symbol: &str,
        timeframe: &str,
        limit: Option<usize>,
    ) -> Result<ChartBars, GatewayError> {
        protopyne_view::validate_symbol_timeframe(symbol, timeframe)?;
        self.calls.lock().expect("call log locks").push((
            symbol.to_owned(),
            timeframe.to_owned(),
            limit,
        ));
        let take = limit.unwrap_or(self.rows.len()).min(self.rows.len());
        Ok(to_bars(&self.rows[self.rows.len() - take..]))
    }
}

#[test]
fn default_fetch_at_m5_covers_the_one_month_default() {
    let gateway = LimitGateway::new(m1_rows(150_000));
    let workspace = ChartWorkspace::with_gateway(&gateway).expect("gateway boots the workspace");
    let limits = gateway.limits_for("5M");
    let need = fetch_for_window(DateRange::Month1, 300, FETCH_WARMUP_BARS);
    assert_eq!(need, 8_840, "one month of five-minute slots plus warmup");
    assert!(
        limits.contains(&Some(need)),
        "boot requests the selected range's need"
    );
    assert_eq!(
        workspace.rows().len(),
        need,
        "boot serves exactly the one-month window"
    );
}

#[test]
fn selecting_a_larger_preset_grows_the_trailing_read() {
    let gateway = LimitGateway::new(m1_rows(300_000));
    let mut workspace =
        ChartWorkspace::with_gateway(&gateway).expect("gateway boots the workspace");
    let one = fetch_for_window(DateRange::Month1, 300, FETCH_WARMUP_BARS);
    assert_eq!(
        workspace.rows().len(),
        one,
        "boot holds the one-month default"
    );

    workspace
        .select_range(DateRange::Month3, Some(&gateway))
        .expect("a larger preset grows the read");
    let three = fetch_for_window(DateRange::Month3, 300, FETCH_WARMUP_BARS);
    assert_eq!(workspace.range(), DateRange::Month3);
    assert_eq!(
        workspace.rows().len(),
        three,
        "the read grew to the three-month need"
    );
    assert!(
        gateway.limits_for("5M").contains(&Some(three)),
        "the growth issued a bounded read at the new need"
    );

    let calls = gateway.limits_for("5M").len();
    workspace
        .select_range(DateRange::Day1, Some(&gateway))
        .expect("a smaller preset unloads");
    assert_eq!(workspace.range(), DateRange::Day1);
    assert_eq!(
        workspace.rows().len(),
        fetch_for_window(DateRange::Day1, 300, FETCH_WARMUP_BARS),
        "the loaded window unloads (prunes) to the range"
    );
    assert_eq!(
        gateway.limits_for("5M").len(),
        calls,
        "an unload issues no bounded read"
    );
    assert!(
        workspace.effective_window().len() <= 288,
        "the visible window pruned to one day of five-minute bars"
    );
}

#[test]
fn selecting_the_active_preset_is_a_no_op() {
    let gateway = LimitGateway::new(m1_rows(50_000));
    let mut workspace =
        ChartWorkspace::with_gateway(&gateway).expect("gateway boots the workspace");
    let calls = gateway.limits_for("5M").len();
    workspace
        .select_range(DateRange::Month1, Some(&gateway))
        .expect("the active preset holds");
    assert_eq!(
        gateway.limits_for("5M").len(),
        calls,
        "an active pick re-reads nothing"
    );
}

/// The menu-bar Refresh (no live feed) verifies the chart range only: a
/// covered range re-reads nothing and pulls no dock/replay bundle, while a
/// short window is re-read.
#[test]
fn refresh_verifies_the_chart_range_without_touching_the_dock() {
    let gateway = LimitGateway::new(m1_rows(50_000));
    let mut workspace =
        ChartWorkspace::with_gateway(&gateway).expect("gateway boots the workspace");
    let calls = gateway.limits_for("5M").len();
    workspace
        .verify_chart_range(&gateway)
        .expect("verify applies");
    assert_eq!(
        gateway.limits_for("5M").len(),
        calls,
        "a covered range re-reads nothing"
    );
    assert!(
        workspace.dock_bundle.is_none() && workspace.dock_bundle_error.is_none(),
        "the refresh never serializes the dock bundle"
    );
    // A pruned window below the range need is re-read.
    workspace.rows.truncate(10);
    workspace
        .verify_chart_range(&gateway)
        .expect("verify re-reads");
    assert!(workspace.rows().len() > 10, "the short window is re-read");
    assert!(
        workspace.dock_bundle.is_none() && workspace.dock_bundle_error.is_none(),
        "the dock stays untouched on the re-read too"
    );
}

#[test]
fn load_more_grows_against_a_limit_honoring_gateway() {
    let gateway = LimitGateway::new(m1_rows(200_000));
    let mut workspace =
        ChartWorkspace::with_gateway(&gateway).expect("gateway boots the workspace");
    let before = workspace.rows().len();
    assert!(
        before < 200_000,
        "boot serves the selected range, not the whole store"
    );
    let first_before = workspace
        .rows()
        .first()
        .map(|row| row.time)
        .expect("rows load");
    assert!(
        workspace
            .load_previous_month(Some(&gateway))
            .expect("load-more reads"),
        "load-more grows"
    );
    assert!(
        workspace.rows().len() > before,
        "the window holds more bars after load-more"
    );
    let first_after = workspace
        .rows()
        .first()
        .map(|row| row.time)
        .expect("rows load");
    assert!(first_after < first_before, "growth reaches earlier bars");
}

/// Refresh (no live feed) refits the Y axis in place: it never trails to the
/// newest bar — that is the forward-to-end button's job. The bars under the
/// viewport stay the same bars, whether the range was already covered or the
/// read grew the window.
#[test]
fn refresh_refits_y_in_place_without_trailing_to_the_end() {
    use crate::workspace::PANE_WIDTH_PX;
    let full = m1_rows(100_000);
    let gateway = LimitGateway::new(full.clone());
    // Seed the trailing 1,000 bars directly so the test controls the window;
    // the gateway holds the full 100k behind it.
    let mut workspace =
        ChartWorkspace::with_rows(full[full.len() - 1_000..].to_vec()).expect("workspace boots");
    assert_eq!(workspace.rows().len(), 1_000);
    // Pan away from the trailing edge: bar 100 sits at the right edge.
    workspace
        .place_bar_at_right(100, PANE_WIDTH_PX)
        .expect("pan places the bar");
    let main = workspace.main_viewport().expect("a viewport binds");
    let (old_start, old_end) = main.visible_bar_range(PANE_WIDTH_PX, workspace.rows().len());
    assert!(
        old_end < 500,
        "the view sits away from the tail: {old_start}..{old_end}"
    );
    let old_first_time = workspace.rows()[old_start].time;

    workspace
        .verify_chart_range(&gateway)
        .expect("refresh verifies");
    assert!(
        workspace.rows().len() > 1_000,
        "the short window is re-read against the full store"
    );
    let main = workspace.main_viewport().expect("a viewport binds");
    let (new_start, _) = main.visible_bar_range(PANE_WIDTH_PX, workspace.rows().len());
    assert_eq!(
        workspace.rows()[new_start].time,
        old_first_time,
        "the same bar opens the view after the re-read"
    );

    // A covered range re-reads nothing and moves nothing at all.
    let offset = main.x_offset_px();
    let calls = gateway.limits_for("5M").len();
    workspace
        .verify_chart_range(&gateway)
        .expect("verify applies");
    assert_eq!(
        gateway.limits_for("5M").len(),
        calls,
        "a covered range re-reads nothing"
    );
    let main = workspace.main_viewport().expect("a viewport binds");
    assert_eq!(
        main.x_offset_px().to_bits(),
        offset.to_bits(),
        "a covered refresh holds the X position exactly"
    );
}
