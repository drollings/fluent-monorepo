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
    ArmResponse, BacktestRunParams, BacktestRunRef, BacktestRunView, BacktestSchemaView,
    BrokerAccountView, BrokerCapabilityView, BrokerDealView, BrokerOrderView, ChartBar, ChartBars,
    HealthView, JournalRowView, OverlaySeries, PortfolioView, PositionView, ProposalView,
    RehearsalEconomics, ReplayStartParams, ReplayStateView, ReplayStepView, ReplayView,
    StructureKindHint, SymbolInfoView, TicketDraft, TicketDriftView, TicketView, WorstCase,
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
            Self::Unavailable(reason) => {
                write!(formatter, "terminal gateway unavailable: {reason}")
            }
            Self::Malformed(reason) => {
                write!(formatter, "terminal gateway payload malformed: {reason}")
            }
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
        return Err(GatewayError::Malformed(
            "symbol must not be empty".to_owned(),
        ));
    }
    if timeframe.trim().is_empty() {
        return Err(GatewayError::Malformed(
            "timeframe must not be empty".to_owned(),
        ));
    }
    Ok(())
}

/// Neutral read contract: every view the terminal renders, plus the
/// core-forwarded submits.
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
    /// The venue-booked portfolio: what actually filled on the book, folded
    /// through the same aggregate the rehearsal projection uses. Additive and
    /// default-refused — a gateway with no venue book serves [`portfolio`]
    /// instead.
    fn portfolio_booked(&self) -> Result<PortfolioView, GatewayError> {
        Err(GatewayError::Unavailable(
            "booked portfolio is not served by this gateway".to_owned(),
        ))
    }
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

    /// Broker venues and their capabilities. Additive and default-refused:
    /// a gateway with no broker directory names the absence.
    fn broker_venues(&self) -> Result<Vec<BrokerCapabilityView>, GatewayError> {
        Err(GatewayError::Unavailable(
            "broker reads are not served by this gateway".to_owned(),
        ))
    }

    /// Broker accounts across every registered venue.
    fn broker_accounts(&self) -> Result<Vec<BrokerAccountView>, GatewayError> {
        Err(GatewayError::Unavailable(
            "broker reads are not served by this gateway".to_owned(),
        ))
    }

    /// One broker account summary.
    fn broker_account(&self, account: &str) -> Result<BrokerAccountView, GatewayError> {
        let _ = account;
        Err(GatewayError::Unavailable(
            "broker reads are not served by this gateway".to_owned(),
        ))
    }

    /// Orders for one broker account.
    fn broker_orders(&self, account: &str) -> Result<Vec<BrokerOrderView>, GatewayError> {
        let _ = account;
        Err(GatewayError::Unavailable(
            "broker reads are not served by this gateway".to_owned(),
        ))
    }

    /// Deals for one broker account.
    fn broker_deals(&self, account: &str) -> Result<Vec<BrokerDealView>, GatewayError> {
        let _ = account;
        Err(GatewayError::Unavailable(
            "broker reads are not served by this gateway".to_owned(),
        ))
    }

    /// Approve a pending proposal by id: journal the human decision through
    /// the core's command plane. Additive and default-refused — only a gateway
    /// backed by a journal over the approval inbox serves it, and the refusal
    /// names that.
    fn approve(&self, proposal_id: &str) -> Result<(), GatewayError> {
        let _ = proposal_id;
        Err(GatewayError::Unavailable(
            "proposal decisions are not served by this gateway".to_owned(),
        ))
    }

    /// Reject a pending proposal, journaling the human's reason. Additive and
    /// default-refused beside [`approve`](Self::approve).
    fn reject(&self, proposal_id: &str, reason: &str) -> Result<(), GatewayError> {
        let _ = (proposal_id, reason);
        Err(GatewayError::Unavailable(
            "proposal decisions are not served by this gateway".to_owned(),
        ))
    }

    /// Revoke the live grant by id: journal the revoke row, then re-fold. A
    /// wrong or absent id refuses. Additive and default-refused — only a
    /// gateway backed by a journal serves it.
    fn revoke_grant(&self, grant_id: &str) -> Result<(), GatewayError> {
        let _ = grant_id;
        Err(GatewayError::Unavailable(
            "grant revocation is not served by this gateway".to_owned(),
        ))
    }

    /// Re-attest the live grant by id: journal the attest row, re-arming
    /// entries for another cadence. A wrong or absent id refuses. Additive
    /// and default-refused — only a gateway backed by a journal serves it,
    /// and the mock stays honest by refusing.
    fn attest_grant(&self, grant_id: &str) -> Result<(), GatewayError> {
        let _ = grant_id;
        Err(GatewayError::Unavailable(
            "grant attestation is not served by this gateway".to_owned(),
        ))
    }

    /// Close every open group in one portfolio: journal the close-all command
    /// row for the loop to execute. Additive and default-refused — only a
    /// gateway backed by the command journal serves it, and execution belongs
    /// to the loop (exits route even while entries halt), never to the read
    /// path.
    fn close_all(&self, portfolio: &str) -> Result<(), GatewayError> {
        let _ = portfolio;
        Err(GatewayError::Unavailable(
            "operator close is not served by this gateway".to_owned(),
        ))
    }

    /// Cancel every working order in one portfolio: journal the cancel-all
    /// command row for the loop to execute. Additive and default-refused
    /// beside [`close_all`](Self::close_all).
    fn cancel_all(&self, portfolio: &str) -> Result<(), GatewayError> {
        let _ = portfolio;
        Err(GatewayError::Unavailable(
            "operator cancel is not served by this gateway".to_owned(),
        ))
    }

    /// The economics the core books on every rehearsal proposal over
    /// `underlying` in `structure`: strategy name, per-proposal worst-case
    /// loss, notional, and days to expiry, plus the grant they came from.
    /// Additive and default-refused — the terminal sends what the core
    /// declares and never substitutes a literal of its own.
    fn rehearsal_economics(
        &self,
        underlying: &str,
        structure: StructureKindHint,
    ) -> Result<RehearsalEconomics, GatewayError> {
        let _ = (underlying, structure);
        Err(GatewayError::Unavailable(
            "rehearsal economics are not served by this gateway".to_owned(),
        ))
    }

    /// Tickets still waiting for the human's fill report, in ticket order.
    /// Additive and default-refused — only a gateway backed by the ticket
    /// journal serves it.
    fn tickets_waiting(&self) -> Result<Vec<TicketView>, GatewayError> {
        Err(GatewayError::Unavailable(
            "manual tickets are not served by this gateway".to_owned(),
        ))
    }

    /// Flagged disagreements between tickets, fills, and the import, oldest
    /// first. Additive and default-refused beside
    /// [`tickets_waiting`](Self::tickets_waiting).
    fn ticket_drifts(&self) -> Result<Vec<TicketDriftView>, GatewayError> {
        Err(GatewayError::Unavailable(
            "manual tickets are not served by this gateway".to_owned(),
        ))
    }

    /// Issue a manual ticket: journal the issued row so the ticket waits as
    /// unconfirmed. The terminal fills a [`TicketDraft`]; the core mints the
    /// ticket identity, builds the venue intent, and validates it with its
    /// own parser. Additive and default-refused — only a gateway backed by
    /// the ticket journal serves it.
    fn issue_ticket(&self, ticket: TicketDraft) -> Result<(), GatewayError> {
        let _ = ticket;
        Err(GatewayError::Unavailable(
            "manual tickets are not served by this gateway".to_owned(),
        ))
    }

    /// Confirm a ticket against the human's fill report: journal the confirm
    /// row, clear the wait, and flag any drift. The fill rides an opaque
    /// document (quantity plus average price); an unknown ticket refuses.
    /// Additive and default-refused beside
    /// [`issue_ticket`](Self::issue_ticket).
    fn confirm_ticket(&self, ticket_id: &str, fill: serde_json::Value) -> Result<(), GatewayError> {
        let _ = (ticket_id, fill);
        Err(GatewayError::Unavailable(
            "manual tickets are not served by this gateway".to_owned(),
        ))
    }

    /// Apply one read-only import snapshot over the human-executed account:
    /// the snapshot rides an opaque position list, and a failing read
    /// reports without touching ticket state. Additive and default-refused
    /// beside [`issue_ticket`](Self::issue_ticket).
    fn import_tickets(&self, snapshot: serde_json::Value) -> Result<(), GatewayError> {
        let _ = snapshot;
        Err(GatewayError::Unavailable(
            "manual tickets are not served by this gateway".to_owned(),
        ))
    }

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

    /// Chartable bars for one symbol and timeframe, bounded to at most
    /// `limit` trailing bars when a durable tape serves the read.
    ///
    /// This is an **additive, default-implemented** read: the default refuses
    /// blank identifiers and delegates to
    /// [`chart_bars_for`](Self::chart_bars_for) — a gateway with no durable
    /// tape serves exactly what it always did, and every existing implementor
    /// keeps compiling without an edit. A gateway that can plan a bounded
    /// store read overrides this; `limit` is a trailing-bar cap, never a
    /// range, so it can only narrow the read, never widen it.
    fn chart_bars_for_limit(
        &self,
        symbol: &str,
        timeframe: &str,
        limit: Option<usize>,
    ) -> Result<ChartBars, GatewayError> {
        let _ = limit;
        validate_symbol_timeframe(symbol, timeframe)?;
        self.chart_bars_for(symbol, timeframe)
    }

    /// Open an interactive replay session over the core's history.
    ///
    /// Additive and default-refused: only a gateway with a session registry
    /// behind it overrides this. `params.session_id` names the registry
    /// slot; opening replaces any session already there.
    fn replay_start(&self, params: ReplayStartParams) -> Result<ReplayStateView, GatewayError> {
        let _ = params;
        Err(GatewayError::Unavailable(
            "interactive replay is not served by this gateway".to_owned(),
        ))
    }

    /// Decide the next `bars` bars of a live session through the shared
    /// decision stack. Additive and default-refused beside
    /// [`replay_start`](Self::replay_start).
    fn replay_step(&self, session_id: &str, bars: u64) -> Result<ReplayStateView, GatewayError> {
        let _ = (session_id, bars);
        Err(GatewayError::Unavailable(
            "interactive replay is not served by this gateway".to_owned(),
        ))
    }

    /// Read a live session's cursor, verdicts, fills, and journal without
    /// deciding anything. Additive and default-refused beside
    /// [`replay_start`](Self::replay_start).
    fn replay_state(&self, session_id: &str) -> Result<ReplayStateView, GatewayError> {
        let _ = session_id;
        Err(GatewayError::Unavailable(
            "interactive replay is not served by this gateway".to_owned(),
        ))
    }

    /// Close a live session and release its registry slot. Additive and
    /// default-refused beside [`replay_start`](Self::replay_start); stopping
    /// an unknown session refuses rather than succeeding emptily.
    fn replay_stop(&self, session_id: &str) -> Result<(), GatewayError> {
        let _ = session_id;
        Err(GatewayError::Unavailable(
            "interactive replay is not served by this gateway".to_owned(),
        ))
    }

    /// The backtest composition schema: the nested spec shape plus the
    /// registry enumerations a form picker must offer. Additive and
    /// default-refused beside the other served reads.
    fn backtest_schema(&self) -> Result<BacktestSchemaView, GatewayError> {
        Err(GatewayError::Unavailable(
            "backtest schema is not served by this gateway".to_owned(),
        ))
    }

    /// Run one backtest and store it under a deterministic run id. Returns
    /// the handle; the full projection reads back through
    /// [`backtest_state`](Self::backtest_state).
    fn backtest_run(&self, params: BacktestRunParams) -> Result<BacktestRunRef, GatewayError> {
        let _ = params;
        Err(GatewayError::Unavailable(
            "backtest runs are not served by this gateway".to_owned(),
        ))
    }

    /// Read one stored backtest run, fully projected.
    fn backtest_state(&self, run_id: &str) -> Result<BacktestRunView, GatewayError> {
        let _ = run_id;
        Err(GatewayError::Unavailable(
            "stored backtests are not served by this gateway".to_owned(),
        ))
    }

    /// Drop one stored backtest run and release its registry slot. Dropping
    /// an unknown run refuses rather than succeeding emptily.
    fn backtest_stop(&self, run_id: &str) -> Result<(), GatewayError> {
        let _ = run_id;
        Err(GatewayError::Unavailable(
            "stored backtests are not served by this gateway".to_owned(),
        ))
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
            // The mock observes no store: no reading on record.
            store_open_gaps: None,
            store_reachable: None,
            store_watermark_age_secs: None,
            // The mock serves no tape: no read on record.
            tape_denied: None,
            tape_last_miss: None,
            tape_no_tape: None,
            tape_served: None,
            tape_store_error: None,
            tape_unknown_symbol: None,
            tape_unsupported_timeframe: None,
            last_command: None,
            ingest_open_gaps: None,
            ingest_watermark_age_secs: None,
            ingest_last_repair: None,
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
            source: crate::views::PORTFOLIO_SOURCE_BOOKED.to_owned(),
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

    /// Bounded mock read: `limit` narrows the canned slot to its trailing
    /// bars, so load-more growth is observable in tests; `None` and oversize
    /// serve the canned slot untouched, exactly like the default.
    fn chart_bars_for_limit(
        &self,
        symbol: &str,
        timeframe: &str,
        limit: Option<usize>,
    ) -> Result<ChartBars, GatewayError> {
        validate_symbol_timeframe(symbol, timeframe)?;
        let bars = self.chart_bars_for(symbol, timeframe)?;
        Ok(match limit {
            Some(count) => {
                let skip = bars.rows.len().saturating_sub(count);
                ChartBars {
                    rows: bars.rows[skip..].to_vec(),
                    dropped: bars.dropped,
                }
            }
            None => bars,
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
        Ok(crate::symbol_catalog())
    }

    fn replay(&self) -> Result<ReplayView, GatewayError> {
        // Happy-path-only mock over a three-bar window at the head: one
        // checkpoint on bar 0, matching what the core publishes for a short
        // history at the default cadence. Refusal and cadence shapes are not
        // mocked — tests needing them drive the server gateway instead.
        let steps: Vec<ReplayStepView> = [
            "2023-11-14T22:13:20+00:00",
            "2023-11-14T22:28:20+00:00",
            "2023-11-14T22:43:20+00:00",
        ]
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
        Ok(ReplayView {
            cursor: total_bars,
            total_bars,
            steps,
        })
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
            basis: "BudgetEcho".to_owned(),
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
            .ok_or_else(|| {
                GatewayError::InvalidGrant("grant document names no grant_id".to_owned())
            })?;
        Ok(ArmResponse {
            grant_id: grant_id.to_owned(),
            worst_case: WorstCase {
                max_trades: 1,
                max_contracts: 1,
                max_loss: Money::from(100),
                max_notional: Money::from(1_000),
                basis: "BudgetEcho".to_owned(),
            },
            armed: true,
        })
    }

    fn tickets_waiting(&self) -> Result<Vec<TicketView>, GatewayError> {
        // One canned wait, consistent with the canned worst case above: the
        // form's confirm path has a row to act on.
        Ok(vec![TicketView {
            ticket_id: "mock-ticket-1".to_owned(),
            rendered: "buy 1 MOCK @ 100 day [mock-ticket-1]".to_owned(),
            underlying: "MOCK".to_owned(),
            side: "Buy".to_owned(),
            quantity: 1,
            limit_price: Money::from(100),
        }])
    }

    fn ticket_drifts(&self) -> Result<Vec<TicketDriftView>, GatewayError> {
        // The canned book agrees with itself: no drift on record.
        Ok(Vec::new())
    }
}

#[cfg(test)]
#[path = "../tests/gateway.rs"]
mod tests;
