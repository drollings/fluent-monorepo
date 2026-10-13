//! Read abstraction plus mock: render over `&dyn TerminalGateway`.
//!
//! The trait is object-safe and `Send + Sync` so the terminal can hold one
//! behind a trait object across threads. Reads return owned DTOs; the single
//! write (`arm`) forwards an opaque grant document to the core, which alone
//! validates (tighten-only stays core-side).

use std::fmt::{Display, Formatter};

use protopyne_types::Money;

use crate::views::{
    ArmResponse, ChartBar, ChartBars, HealthView, JournalRowView, OverlaySeries, PortfolioView,
    PositionView, ProposalView, WorstCase,
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

/// Neutral read contract: every view the terminal renders, plus the one
/// core-forwarded submit.
///
/// Read methods mirror the server routes (`health`, `proposals`,
/// `portfolio`, `journal`, `chart_bars`, `chart_overlays`); `arm` forwards
/// an opaque grant JSON document — the terminal never mints grants, and the
/// core alone validates (tighten-only, conflict on loosening).
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
    /// Forward a grant document to the core for arming.
    fn arm(&self, grant: serde_json::Value) -> Result<ArmResponse, GatewayError>;
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
