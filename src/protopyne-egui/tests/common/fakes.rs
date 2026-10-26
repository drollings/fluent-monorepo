//! Configurable recording gateway double for terminal tests: every required
//! verb MockGateway serves is inherited by delegation, so the ten refusing
//! one-liners the old doubles hand-wrote are one shared home; the verbs a
//! test drives are overridden with recording.
//!
//! Two independent knob groups, one per migrating double — a file touches
//! only its own group:
//!
//! * Backtest verbs (the backtest form): `backtest_run` records the envelope
//!   and serves the canned reference, or refuses when `refuse_runs` is set;
//!   `backtest_state` always serves the canned view; `backtest_stop` clears.
//!   Reads are never called by the backtest panel, so they ride the mock.
//! * Chart verbs (the fetch-limit workspace): `chart_bars` serves the fixed
//!   rows and `chart_bars_for_limit` records the bounded read before serving
//!   its trailing take. `symbols` stays on the trait default (empty), exactly
//!   as the old double: the workspace boot branches on it, so inheriting the
//!   mock catalog would change which symbol boots.

use std::collections::BTreeMap;
use std::sync::Mutex;

use protopyne_chart_model::ChartRow;
use protopyne_view::{
    ArmResponse, BacktestRunParams, BacktestRunRef, BacktestRunView, ChartBar, ChartBars,
    GatewayError, HealthView, JournalRowView, MockGateway, OverlaySeries, PortfolioView,
    ProposalView, ReplayView, TerminalGateway, WorstCase,
};

/// The canned stored-run projection the backtest verbs serve.
#[must_use]
pub(crate) fn canned_backtest_view(run_id: &str) -> BacktestRunView {
    BacktestRunView {
        run_id: run_id.to_owned(),
        manifest_json: "{}".to_owned(),
        metrics: vec![protopyne_view::BacktestMetricView {
            label: "fills".to_owned(),
            value: Some("2".to_owned()),
        }],
        equity: vec![],
        trades: vec![],
        markers: vec![],
        status: None,
        warnings: vec![],
    }
}

/// One recording double for the terminal suites: mock-backed reads plus the
/// backtest and bounded-chart verbs the tests drive.
pub(crate) struct FakeGateway {
    mock: MockGateway,
    envelopes: Mutex<Vec<BacktestRunParams>>,
    refuse_run: Mutex<bool>,
    rows: Vec<ChartRow>,
    calls: Mutex<Vec<(String, String, Option<usize>)>>,
}

impl FakeGateway {
    /// A backtest-driving double: run envelopes record, state serves canned.
    #[must_use]
    pub(crate) fn backtest() -> Self {
        Self::with_rows(Vec::new())
    }

    /// The same double with runs refusing, for the refused-run path.
    #[must_use]
    pub(crate) fn refusing_runs() -> Self {
        let fake = Self::backtest();
        *fake.refuse_run.lock().expect("refusal flag locks") = true;
        fake
    }

    /// A chart-driving double over fixed rows, recording bounded reads.
    #[must_use]
    pub(crate) fn with_rows(rows: Vec<ChartRow>) -> Self {
        Self {
            mock: MockGateway::new(),
            envelopes: Mutex::new(Vec::new()),
            refuse_run: Mutex::new(false),
            rows,
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Envelopes the double was asked to run, in order.
    #[must_use]
    pub(crate) fn envelopes(&self) -> Vec<BacktestRunParams> {
        self.envelopes.lock().expect("envelopes lock").clone()
    }

    /// Bounded reads the double served, in order.
    #[must_use]
    pub(crate) fn limits_for(&self, timeframe: &str) -> Vec<Option<usize>> {
        self.calls
            .lock()
            .expect("call log locks")
            .iter()
            .filter(|(_, label, _)| label == timeframe)
            .map(|(_, _, limit)| *limit)
            .collect()
    }
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

impl TerminalGateway for FakeGateway {
    fn health(&self) -> Result<HealthView, GatewayError> {
        self.mock.health()
    }

    fn proposals(&self) -> Result<Vec<ProposalView>, GatewayError> {
        self.mock.proposals()
    }

    fn portfolio(&self) -> Result<PortfolioView, GatewayError> {
        self.mock.portfolio()
    }

    fn journal(&self) -> Result<Vec<JournalRowView>, GatewayError> {
        self.mock.journal()
    }

    fn chart_bars(&self) -> Result<ChartBars, GatewayError> {
        Ok(to_bars(&self.rows))
    }

    fn chart_overlays(&self, kind: &str, period: i64) -> Result<OverlaySeries, GatewayError> {
        self.mock.chart_overlays(kind, period)
    }

    fn replay(&self) -> Result<ReplayView, GatewayError> {
        self.mock.replay()
    }

    fn preview_arm(&self, patch: serde_json::Value) -> Result<WorstCase, GatewayError> {
        self.mock.preview_arm(patch)
    }

    fn grant_tiers(&self) -> Result<BTreeMap<String, String>, GatewayError> {
        self.mock.grant_tiers()
    }

    fn arm(&self, grant: serde_json::Value) -> Result<ArmResponse, GatewayError> {
        self.mock.arm(grant)
    }

    fn backtest_run(&self, params: BacktestRunParams) -> Result<BacktestRunRef, GatewayError> {
        if *self.refuse_run.lock().expect("refusal flag locks") {
            return Err(GatewayError::Unavailable("core refused the run".to_owned()));
        }
        self.envelopes.lock().expect("envelopes lock").push(params);
        Ok(BacktestRunRef {
            run_id: "backtest-07".to_owned(),
        })
    }

    fn backtest_state(&self, run_id: &str) -> Result<BacktestRunView, GatewayError> {
        Ok(canned_backtest_view(run_id))
    }

    fn backtest_stop(&self, _run_id: &str) -> Result<(), GatewayError> {
        Ok(())
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
