//! Read abstraction plus mock: render over `&dyn TerminalGateway`.
//!
//! The trait is object-safe and `Send + Sync` so the terminal can hold one
//! behind a trait object across threads. Reads return owned DTOs; the single
//! write (`arm`) forwards an opaque grant document to the core, which alone
//! validates (tighten-only stays core-side).

use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};

use protopyne_types::Money;

use crate::views::{
    ArmResponse, ChartBar, ChartBars, HealthView, JournalRowView, OverlaySeries, PortfolioView,
    PositionView, ProposalView, ReplayStepView, ReplayView, SymbolInfoView, WorstCase,
};

/// Terminal-gateway failure: transport, shape, or refused submit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayError {
    /// The gateway could not reach or read the core.
    Unavailable(String),
    /// The core returned a payload the contract cannot parse.
    Malformed(String),
    /// The grant document was refused before reaching the core.
    InvalidGrant(String),
}

impl Display for GatewayError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(reason) => write!(formatter, "terminal gateway unavailable: {reason}"),
            Self::Malformed(reason) => write!(formatter, "terminal gateway payload malformed: {reason}"),
            Self::InvalidGrant(reason) => write!(formatter, "grant refused: {reason}"),
        }
    }
}

impl std::error::Error for GatewayError {}

/// Refuse a blank symbol or timeframe before any read.
///
/// Shared by the default symbol-scoped trait methods and by any gateway that
/// overrides them, so every gateway refuses the same malformed identifiers the
/// same way. Whitespace-only values count as blank.
pub fn validate_symbol_timeframe(symbol: &str, timeframe: &str) -> Result<(), GatewayError> {
    if symbol.trim().is_empty() {
        return Err(GatewayError::Malformed("symbol must not be empty".to_owned()));
    }
    if timeframe.trim().is_empty() {
        return Err(GatewayError::Malformed("timeframe must not be empty".to_owned()));
    }
    Ok(())
}

/// Neutral read contract: every view the terminal renders, plus the one
/// core-forwarded submit.
///
/// Read methods mirror the server routes (`health`, `proposals`,
/// `portfolio`, `journal`, `chart_bars`, `chart_overlays`, `replay`);
/// `arm` forwards an opaque grant JSON document — the terminal never mints
/// grants, and the core alone validates (tighten-only, conflict on
/// loosening). The envelope gains a documented `patch` branch
/// (`{"patch": {tier: "<tier document text>"}}`): the schema-driven form
/// tightens the live grant tier by tier through the core's own
/// `Grant::tighten`, with whole-grant arming byte-identical beside it.
/// `preview_arm` previews that same patch without storing anything, and
/// `grant_tiers` reads the live grant's current tier texts for display.
pub trait TerminalGateway: Send + Sync {
    /// Liveness plus command-plane status.
    fn health(&self) -> Result<HealthView, GatewayError>;
    /// Strategy inbox rows.
    fn proposals(&self) -> Result<Vec<ProposalView>, GatewayError>;
    /// Aggregate portfolio over the latest run.
    fn portfolio(&self) -> Result<PortfolioView, GatewayError>;
    /// Journaled lifecycle events in sequence order.
    fn journal(&self) -> Result<Vec<JournalRowView>, GatewayError>;
    /// Chartable bars plus the drop count.
    fn chart_bars(&self) -> Result<ChartBars, GatewayError>;
    /// One indicator overlay over the chart bars (`sma`, `ema`, `bb`,
    /// `trima`; `period` within `[1, 500]` like the route).
    fn chart_overlays(&self, kind: &str, period: i64) -> Result<OverlaySeries, GatewayError>;
    /// The replayable window: where the scrub stands plus the ordered steps.
    ///
    /// A read, never a write: the cursor it carries is the core's recorded
    /// position, and a terminal that scrubs holds the moved cursor itself.
    fn replay(&self) -> Result<ReplayView, GatewayError>;
    /// Preview a patch envelope's worst case without storing anything: the
    /// core reads the live grant, tightens it in memory, and returns the
    /// worst case the patched grant would authorize. The confirmation
    /// surface renders this before submit is actionable; a preview is never
    /// an arm, and a patch with no live grant behind it refuses.
    fn preview_arm(&self, patch: serde_json::Value) -> Result<WorstCase, GatewayError>;
    /// Current tier texts of the live grant (tier name to boundary-string
    /// document), for display beside the form's editors. Encoding matches
    /// what a patch carries back: composite tiers as JSON document text,
    /// the window end as bare RFC 3339. Display text only — never a
    /// decision — and empty when no grant is live.
    fn grant_tiers(&self) -> Result<BTreeMap<String, String>, GatewayError>;
    /// Forward a grant document to the core for arming.
    fn arm(&self, grant: serde_json::Value) -> Result<ArmResponse, GatewayError>;

    /// Selectable instruments, in deterministic order.
    ///
    /// The default is an empty catalog: a gateway that serves a single dataset
    /// curates none, and the terminal's Symbol Selection Modal is driven by a
    /// gateway that does (the mock, or a future instrument-master read).
    fn symbols(&self) -> Result<Vec<SymbolInfoView>, GatewayError> {
        Ok(Vec::new())
    }

    /// Chartable bars for one symbol and timeframe.
    ///
    /// The default refuses blank identifiers, then delegates to the
    /// slot-agnostic `chart_bars`, so a gateway serving one dataset stays
    /// correct without a second read path. A gateway that can resample or
    /// serve several symbols overrides this.
    fn chart_bars_for(&self, symbol: &str, timeframe: &str) -> Result<ChartBars, GatewayError> {
        validate_symbol_timeframe(symbol, timeframe)?;
        self.chart_bars()
    }

    /// One indicator overlay for one symbol and timeframe.
    ///
    /// The default refuses blank identifiers, then delegates to
    /// `chart_overlays` (same `kind`/`period` contract).
    fn chart_overlays_for(
        &self,
        symbol: &str,
        timeframe: &str,
        kind: &str,
        period: i64,
    ) -> Result<OverlaySeries, GatewayError> {
        validate_symbol_timeframe(symbol, timeframe)?;
        self.chart_overlays(kind, period)
    }
}

/// Headless stand-in: deterministic canned views for terminal tests.
///
/// Every method succeeds on well-formed input; `arm` accepts any JSON
/// object naming a string `grant_id` and refuses the rest before any core
/// could see it — the same shape as the core's own refusal, without the
/// trading path.
#[derive(Debug, Clone, Default)]
pub struct MockGateway;

impl MockGateway {
    /// Canned views.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl TerminalGateway for MockGateway {
    fn health(&self) -> Result<HealthView, GatewayError> {
        Ok(HealthView {
            status: "ok".to_owned(),
            version: "mock".to_owned(),
            trading_state: "Enabled".to_owned(),
            allows_entries: true,
            allows_exits: true,
            last_command: None,
        })
    }

    fn proposals(&self) -> Result<Vec<ProposalView>, GatewayError> {
        Ok(vec![ProposalView {
            id: "mock-prop-1".to_owned(),
            strategy: "mock".to_owned(),
            underlying: "MOCK".to_owned(),
            direction: "Long".to_owned(),
            structure: "Vertical".to_owned(),
            limit_price: Money::from(100),
            conviction: 0.5,
            state: "Proposed".to_owned(),
        }])
    }

    fn portfolio(&self) -> Result<PortfolioView, GatewayError> {
        Ok(PortfolioView {
            positions: vec![PositionView {
                underlying: "MOCK".to_owned(),
                quantity: 1,
                avg_price: Money::from(100),
            }],
            filled_orders: 1,
            total_fees: Money::from(2),
            open_contracts: 1,
            trading_state: "Enabled".to_owned(),
        })
    }

    fn journal(&self) -> Result<Vec<JournalRowView>, GatewayError> {
        Ok(vec![
            JournalRowView {
                seq: 1,
                client_id: "mock-1".to_owned(),
                kind: "intent".to_owned(),
                detail: "{}".to_owned(),
                at_ms: 1_700_000_000_000,
            },
            JournalRowView {
                seq: 2,
                client_id: "mock-1".to_owned(),
                kind: "filled".to_owned(),
                detail: "{}".to_owned(),
                at_ms: 1_700_000_060_000,
            },
        ])
    }

    fn chart_bars(&self) -> Result<ChartBars, GatewayError> {
        Ok(ChartBars {
            rows: vec![
                ChartBar {
                    time: 1_700_000_000,
                    open: 100.0,
                    high: 101.0,
                    low: 99.0,
                    close: 100.5,
                    volume: 10,
                },
                ChartBar {
                    time: 1_700_000_060,
                    open: 100.5,
                    high: 102.0,
                    low: 100.0,
                    close: 101.5,
                    volume: 12,
                },
                ChartBar {
                    time: 1_700_000_120,
                    open: 101.5,
                    high: 103.0,
                    low: 101.0,
                    close: 102.5,
                    volume: 14,
                },
            ],
            dropped: 0,
        })
    }

    fn chart_overlays(&self, kind: &str, period: i64) -> Result<OverlaySeries, GatewayError> {
        Ok(OverlaySeries {
            kind: kind.to_owned(),
            period,
            values: vec![None, Some(100.75), Some(101.75)],
            dropped: 0,
        })
    }

    fn symbols(&self) -> Result<Vec<SymbolInfoView>, GatewayError> {
        // A small deterministic catalog spanning every asset class, with the
        // terminal's default ticker first, so the Symbol Selection Modal's
        // category tabs are never empty in headless tests.
        Ok(vec![
            SymbolInfoView {
                symbol: "GLD".to_owned(),
                name: "SPDR Gold Shares".to_owned(),
                asset_class: "Funds".to_owned(),
            },
            SymbolInfoView {
                symbol: "SPY".to_owned(),
                name: "SPDR S&P 500 ETF Trust".to_owned(),
                asset_class: "Stocks".to_owned(),
            },
            SymbolInfoView {
                symbol: "QQQ".to_owned(),
                name: "Invesco QQQ Trust".to_owned(),
                asset_class: "Stocks".to_owned(),
            },
            SymbolInfoView {
                symbol: "BTC-USD".to_owned(),
                name: "Bitcoin".to_owned(),
                asset_class: "Crypto".to_owned(),
            },
            SymbolInfoView {
                symbol: "ES".to_owned(),
                name: "E-mini S&P 500".to_owned(),
                asset_class: "Futures".to_owned(),
            },
        ])
    }

    fn replay(&self) -> Result<ReplayView, GatewayError> {
        // Happy-path-only mock over a three-bar window at the head: one
        // checkpoint on bar 0, matching what the core publishes for a short
        // history at the default cadence. Refusal and cadence shapes are not
        // mocked — tests needing them drive the server gateway instead.
        let steps: Vec<ReplayStepView> =
            ["2023-11-14T22:13:20+00:00", "2023-11-14T22:28:20+00:00", "2023-11-14T22:43:20+00:00"]
                .into_iter()
                .enumerate()
                .map(|(bar_index, ts)| ReplayStepView {
                    bar_index: bar_index as u64,
                    ts: ts.to_owned(),
                    done: true,
                    checkpoint: bar_index == 0,
                })
                .collect();
        let total_bars = steps.len() as u64;
        Ok(ReplayView { cursor: total_bars, total_bars, steps })
    }

    fn preview_arm(&self, _patch: serde_json::Value) -> Result<WorstCase, GatewayError> {
        // Happy-path-only mock: no core behind the mock, so no live grant
        // to tighten and no refusal to surface. The canned worst case
        // matches the canned arm, and a form driven against the mock
        // previews exactly what its submit would confirm. Loosening and
        // no-grant refusals are covered against the server gateway, not here.
        Ok(WorstCase {
            max_trades: 1,
            max_contracts: 1,
            max_loss: Money::from(100),
            max_notional: Money::from(1_000),
        })
    }

    fn grant_tiers(&self) -> Result<BTreeMap<String, String>, GatewayError> {
        // Tier texts consistent with the canned worst case above: the form's
        // placeholders show what a submit would tighten.
        Ok(BTreeMap::from([
            (
                "breakers".to_owned(),
                r#"{"max_consecutive_errors":3}"#.to_owned(),
            ),
            (
                "budgets".to_owned(),
                r#"{"max_trades":1,"max_contracts":1,"max_loss":"100","max_notional":"1000"}"#
                    .to_owned(),
            ),
            (
                "portfolio".to_owned(),
                r#"{"max_open_risk":"10000","quote_max_age_secs":60}"#.to_owned(),
            ),
            (
                "signal".to_owned(),
                r#"{"min_conviction":0.0,"min_activity_score":0.0}"#.to_owned(),
            ),
            (
                "trade".to_owned(),
                r#"{"max_loss_per_trade":"10","max_spread_width":"5","min_dte_days":1,"max_dte_days":30}"#
                    .to_owned(),
            ),
            ("valid_until".to_owned(), "2027-01-01T00:00:00Z".to_owned()),
        ]))
    }

    fn arm(&self, grant: serde_json::Value) -> Result<ArmResponse, GatewayError> {
        let grant_id = grant
            .get("grant_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| GatewayError::InvalidGrant("grant document names no grant_id".to_owned()))?;
        Ok(ArmResponse {
            grant_id: grant_id.to_owned(),
            worst_case: WorstCase {
                max_trades: 1,
                max_contracts: 1,
                max_loss: Money::from(100),
                max_notional: Money::from(1_000),
            },
            armed: true,
        })
    }
}

#[cfg(test)]
#[path = "../tests/gateway.rs"]
mod tests;
