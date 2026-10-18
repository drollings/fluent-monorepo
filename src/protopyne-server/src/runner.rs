//! Backtest runner: the same code path as paper, over pinned history.
//!
//! One bar at a time, oldest first, the runner executes the real decision
//! stack — [`Strategy::propose`](protopyne_strategy::Strategy::propose),
//! [`RiskEngine::check`](protopyne_strategy::RiskEngine), pure
//! [`evaluate_grant`](protopyne_strategy::evaluate_grant), then
//! [`Venue::submit`](protopyne_venue::Venue) — and records every verdict in a
//! [`BarDecision`]. Backtest and paper share [`decide_and_submit`], so a
//! divergence between the two is a bug, not a feature.
//!
//! Parallel parameter sweeps fan out over the shared [`ResultPool`] primitive
//! (never raw spawns). Replay and rewind compose the shared
//! [`CheckpointedStepGraph`] primitive for research only: rewind never touches
//! live order state, which this module does not own.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use crate::replay_session::needs_chain_data;
use fluent_dag::checkpointed::CheckpointedStepGraph;
use protopyne_store::{CandleStore, SpanPlan};
use protopyne_strategy::{
    evaluate_grant, BreakerState, Direction, Grant, GrantDecision, GrantMarket, GrantUsage,
    JournaledTransition, LegSelector, Proposal, ProposalEvent, ProposalSide, ProposeInput,
    RiskEngine, RiskState, Strategy, StructureKind,
};
use protopyne_types::{Bar, Money};
use protopyne_venue::{
    submit_guarded, CorporateEvent, LegQuoteSource, OrderIntent, OrderSide, OrderStatus,
    PaperConfig, PaperVenue, Position, Quote, SubmissionRecorder, Venue, VenueError,
    PROPOSAL_TRANSITION_KIND,
};

use crate::backtest::{load_history, quote_for_bar, BacktestConfig, BacktestVenue, HistorySource};
use crate::error::ServerError;
use crate::portfolio::{aggregate, BookedFill, PAPER_PORTFOLIO};
use crate::report::{
    build_report, count_unquotable_bars, history_source_limit, uncertified_coverage_limit,
    BacktestReport, HistoryKind, RunManifest, ABSENT_CAPABILITIES,
};

/// Per-bar economics and identity for one backtest run.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RunnerConfig {
    /// Symbol traded in this run.
    pub underlying: String,
    /// Strategy name for attribution.
    pub strategy_name: String,
    /// Worst-case loss booked per proposal.
    pub max_loss: Money,
    /// Committed notional booked per proposal.
    pub notional: Money,
    /// Days to expiry booked per proposal.
    pub dte_days: u32,
    /// Bars skipped before the first proposal (warmup honesty).
    pub warmup_bars: usize,
}

impl protopyne_types::Validate for RunnerConfig {
    type Err = ServerError;
    fn validate(&self) -> Result<(), Self::Err> {
        RunnerConfig::validate(self)
    }
}

impl RunnerConfig {
    /// Builder entry point.
    #[must_use]
    pub fn new(
        underlying: String,
        strategy_name: String,
        max_loss: Money,
        notional: Money,
        dte_days: u32,
        warmup_bars: usize,
    ) -> Self {
        Self {
            underlying,
            strategy_name,
            max_loss,
            notional,
            dte_days,
            warmup_bars,
        }
    }

    /// Fail-closed self-consistency check.
    pub fn validate(&self) -> Result<(), ServerError> {
        if self.underlying.trim().is_empty() {
            return Err(ServerError::Backtest("underlying must be named".to_owned()));
        }
        if self.strategy_name.trim().is_empty() {
            return Err(ServerError::Backtest(
                "strategy_name must be named".to_owned(),
            ));
        }
        Ok(())
    }
}

/// One bar's recorded verdict: proposal, gates, and fill outcome.
///
/// `skipped` carries the reason whenever no proposal reached the venue
/// (warmup, unquotable bar, refused quote, denied gate, venue refusal).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BarDecision {
    /// Index into the replayed history.
    pub bar_index: usize,
    /// Bar timestamp (RFC 3339).
    pub ts: String,
    /// Deterministic proposal id (`run-{seed}-{index}`).
    pub proposal_id: String,
    /// Signal direction (`None` when no proposal was emitted).
    pub direction: Option<Direction>,
    /// Proposal structure (`None` when no proposal was emitted, or for runs
    /// recorded before structure was captured — the portfolio fold labels
    /// those fills `unrecorded` rather than guessing).
    #[serde(default)]
    pub structure: Option<StructureKind>,
    /// Strategy that produced this bar's proposal, carried from the
    /// proposal itself (`""` when no proposal was emitted).
    #[serde(default)]
    pub strategy: String,
    /// Underlying the proposal traded, carried from the proposal itself
    /// (`""` when no proposal was emitted).
    #[serde(default)]
    pub underlying: String,
    /// Signal conviction of the proposal, carried from the proposal itself
    /// (`0.0` when no proposal was emitted).
    #[serde(default)]
    pub conviction: f64,
    /// Whether the risk engine approved.
    pub risk_pass: bool,
    /// Risk refusal reasons (empty on approval).
    pub risk_reasons: Vec<String>,
    /// Whether the grant covered the proposal.
    pub grant_allow: bool,
    /// Grant denial reasons (empty on allow).
    pub grant_reasons: Vec<String>,
    /// Whether an order reached the venue.
    pub submitted: bool,
    /// Proposal limit (bar close); zero when no proposal existed.
    #[serde(default = "Money::zero")]
    pub limit_price: Money,
    /// Executable limit derived post-proposal (touch for the direction);
    /// zero when no proposal existed.
    #[serde(default = "Money::zero")]
    pub checked_limit: Money,
    /// Contracts submitted to the venue.
    #[serde(default)]
    pub submitted_quantity: u32,
    /// Contracts the venue filled (the single booked-quantity source).
    #[serde(default)]
    pub filled_quantity: u32,
    /// Average fill price, when filled.
    pub fill_price: Option<Money>,
    /// Commission paid, when filled.
    pub fee: Option<Money>,
    /// Journaled lifecycle transitions for this bar's proposal, in emission
    /// order — the real state machine driven by the risk, grant, and venue
    /// verdicts, never a post-hoc reconstruction. Empty when no proposal was
    /// emitted. The last entry's `to` state is the served lifecycle state.
    #[serde(default)]
    pub transitions: Vec<JournaledTransition>,
    /// Lifecycle journal failure after the venue already reported: the fill
    /// above still books (venue truth), but the journaled chain is short.
    /// `None` on the clean path. Surfaced instead of swallowed so a run
    /// whose recorder failed mid-verdict says so in its own bytes.
    #[serde(default)]
    pub lifecycle_error: Option<String>,
    /// Skip reason, when nothing reached the venue for a non-gate reason.
    pub skipped: Option<String>,
}

/// A verdict for a bar that never reached the gates: warmup, unquotable
/// data, or a refused signal. One constructor keeps every early return
/// identical in shape. Crate-visible so the replay session builds the same
/// verdicts as the batch loops.
pub(crate) fn skipped_decision(
    bar_index: usize,
    ts: String,
    proposal_id: String,
    reason: &str,
) -> BarDecision {
    BarDecision {
        bar_index,
        ts,
        proposal_id,
        direction: None,
        structure: None,
        strategy: String::new(),
        underlying: String::new(),
        conviction: 0.0,
        risk_pass: false,
        risk_reasons: Vec::new(),
        grant_allow: false,
        grant_reasons: Vec::new(),
        submitted: false,
        limit_price: Money::zero(),
        checked_limit: Money::zero(),
        submitted_quantity: 0,
        filled_quantity: 0,
        fill_price: None,
        fee: None,
        transitions: Vec::new(),
        lifecycle_error: None,
        skipped: Some(reason.to_owned()),
    }
}

/// The executable order derived from a proposal: venue side plus the limit
/// the venue will actually receive.
///
/// Proposals carry an indicative bar-close limit, but the touch side (buy at
/// ask, sell at bid) depends on direction, which only proposing reveals — so
/// the executable limit is derived post-proposal, after the gates. Any future
/// price-sensitive risk rule must take this checked limit, never the proposal
/// limit: the checked values are the submitted values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckedOrder {
    /// Venue side for the direction.
    pub side: OrderSide,
    /// Touch limit the venue receives.
    pub limit: Money,
}

/// Venue side for a signal direction: longs buy, shorts sell. The single
/// home for the mapping — `checked_order_for` and the portfolio fill fold
/// both read it, so submission and attribution can never disagree on which
/// side a direction traded.
#[must_use]
pub fn side_for_direction(direction: Direction) -> OrderSide {
    match direction {
        Direction::Long => OrderSide::Buy,
        Direction::Short => OrderSide::Sell,
    }
}

/// Derive the executable order for `direction` from the current touch.
#[must_use]
pub fn checked_order_for(direction: Direction, quote: &Quote) -> CheckedOrder {
    match direction {
        Direction::Long => CheckedOrder {
            side: side_for_direction(direction),
            limit: quote.ask,
        },
        Direction::Short => CheckedOrder {
            side: side_for_direction(direction),
            limit: quote.bid,
        },
    }
}

/// Observed producer-health facts for one bar, fed into the merit gates.
///
/// Axis: **(producer-confidence)** for staleness/skew/reconciliation doubt,
/// **(correctness)** for what the gates do with it (refuse with reasons).
/// These facts describe the producer (feed, clock, venue book) — never the
/// trade's merit — so they disarm entries only and never alter a merit
/// verdict, and merit outcomes never write back here.
#[derive(Debug, Clone, Copy)]
pub struct BarHealth {
    /// Age of the quote traded on this bar, in seconds.
    pub quote_age_secs: u64,
    /// Observed clock skew in seconds.
    pub clock_skew_secs: u64,
    /// Whether the venue book agrees with journaled state.
    pub reconciliation_healthy: bool,
}

impl BarHealth {
    /// What the rehearsal loops observe: quotes are synthesized at the bar
    /// close and evaluated at the same instant over one clock, with no
    /// separate venue truth to disagree with. A live feed fills these three
    /// fields from the feed, the clock, and the reconciliation loop instead —
    /// the per-bar construction site in each loop is that plug point.
    #[must_use]
    pub fn rehearsed() -> Self {
        Self {
            quote_age_secs: 0,
            clock_skew_secs: 0,
            reconciliation_healthy: true,
        }
    }
}

/// The journal kind under which a proposal's lifecycle transitions are
/// recorded: the venue command plane's single home, re-exported here so the
/// run path journals the same kind approval rows carry. Distinct from the
/// venue's status/verdict kinds, so the status fold ignores it; a consumer
/// that wants the chain reads the detail payload.
pub const LIFECYCLE_KIND: &str = PROPOSAL_TRANSITION_KIND;

/// Advance `proposal` by one lifecycle event, journaling the transition
/// through `recorder` before the decision acts on the new state, and push the
/// recorded entry onto `emitted` so the run carries its chain. An illegal
/// move (a bug, by construction) refuses rather than silently no-op'ing, so
/// the journal can never record a transition that did not happen.
fn advance<R: SubmissionRecorder>(
    proposal: &mut Proposal,
    recorder: &R,
    client_id: &str,
    event: ProposalEvent,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<JournaledTransition, VenueError> {
    let entry = proposal
        .apply(event, reason, now)
        .map_err(|e| VenueError::Invalid(format!("proposal lifecycle refused: {e}")))?;
    let detail = serde_json::to_string(&entry)
        .map_err(|e| VenueError::Journal(format!("proposal transition not encodable: {e}")))?;
    recorder.record_transition(client_id, LIFECYCLE_KIND, &detail, now)?;
    Ok(entry)
}

/// Drive the risk and grant portion of the lifecycle from `Proposed`: a risk
/// refusal ends it; a grant-covered proposal auto-approves; one the grant
/// does not cover asks a human. Records the moves in order on `emitted`.
fn advance_gates<R: SubmissionRecorder>(
    proposal: &mut Proposal,
    recorder: &R,
    client_id: &str,
    risk_pass: bool,
    grant_allow: bool,
    now: DateTime<Utc>,
    emitted: &mut Vec<JournaledTransition>,
) -> Result<(), VenueError> {
    if !risk_pass {
        emitted.push(advance(
            proposal,
            recorder,
            client_id,
            ProposalEvent::FailRisk,
            "risk refused",
            now,
        )?);
        return Ok(());
    }
    emitted.push(advance(
        proposal,
        recorder,
        client_id,
        ProposalEvent::PassRisk,
        "risk approved",
        now,
    )?);
    let (event, reason) = if grant_allow {
        (ProposalEvent::AutoApprove, "grant covers entry")
    } else {
        (ProposalEvent::RequireHuman, "no grant covers entry")
    };
    emitted.push(advance(proposal, recorder, client_id, event, reason, now)?);
    Ok(())
}

/// The lifecycle events a venue verdict implies, in order. A filled or
/// partially filled order acknowledges work before it can report the fill;
/// a refusal terminalizes; an in-flight verdict advances to working only
/// once the venue acknowledged.
fn verdict_events(status: OrderStatus) -> &'static [ProposalEvent] {
    match status {
        OrderStatus::Filled => &[ProposalEvent::StartWork, ProposalEvent::Fill],
        OrderStatus::Partial => &[ProposalEvent::StartWork, ProposalEvent::PartialFill],
        OrderStatus::Acknowledged => &[ProposalEvent::StartWork],
        OrderStatus::Rejected => &[ProposalEvent::Reject],
        OrderStatus::Cancelled | OrderStatus::Expired | OrderStatus::OpenFailed => {
            &[ProposalEvent::Cancel]
        }
        OrderStatus::Pending | OrderStatus::Submitted => &[],
    }
}

/// Drive the post-submission lifecycle from the venue verdict, recording the
/// moves in order on `emitted`.
fn advance_verdict<R: SubmissionRecorder>(
    proposal: &mut Proposal,
    recorder: &R,
    client_id: &str,
    status: OrderStatus,
    now: DateTime<Utc>,
    emitted: &mut Vec<JournaledTransition>,
) -> Result<(), VenueError> {
    let reason = format!("venue verdict {status:?}");
    for event in verdict_events(status) {
        emitted.push(advance(
            proposal, recorder, client_id, *event, &reason, now,
        )?);
    }
    Ok(())
}

/// The decision skeleton for a bar that produced a proposal: attribution and
/// the checked limit come from the proposal itself, never fabricated. The
/// caller fills the gate verdicts, lifecycle, and outcome.
#[allow(clippy::too_many_arguments)]
fn proposed_decision(
    proposal: &Proposal,
    bar_index: usize,
    ts: String,
    checked: CheckedOrder,
    risk_pass: bool,
    risk_reasons: Vec<String>,
    grant_allow: bool,
    grant_reasons: Vec<String>,
) -> BarDecision {
    BarDecision {
        bar_index,
        ts,
        proposal_id: proposal.id.clone(),
        direction: Some(proposal.direction),
        structure: Some(proposal.structure),
        strategy: proposal.strategy.clone(),
        underlying: proposal.underlying.clone(),
        conviction: proposal.conviction,
        risk_pass,
        risk_reasons,
        grant_allow,
        grant_reasons,
        submitted: false,
        limit_price: proposal.limit_price,
        checked_limit: checked.limit,
        submitted_quantity: 0,
        filled_quantity: 0,
        fill_price: None,
        fee: None,
        transitions: Vec::new(),
        lifecycle_error: None,
        skipped: None,
    }
}

/// Shared decision stack for one bar, used identically by backtest replay
/// and the paper feed. Pure gates plus one venue submission; the caller owns
/// `usage` accounting so both paths accumulate budgets the same way.
///
/// Quote synthesis parameters arrive explicitly in `quote_config` so the fill
/// prices state the caller's assumptions: backtest runs pass their pinned
/// venue config, the paper feed passes its own labeled config. There is
/// deliberately no default here — a hidden spread would price fills under an
/// assumption no report names.
///
/// `breakers` is carried across bars by the caller (never rebuilt per bar):
/// retryable venue failures increment its consecutive-error count and trip it
/// past the grant's bound, which disarms further *entries* in that run while
/// exits stay open and merit verdicts stay exactly as computed. Merit denials
/// and skips leave it untouched — a refused trade is task-value information,
/// not producer doubt.
///
/// The trip rule upholds the same contract as the breaker tier of
/// [`evaluate_grant`]: with `max_consecutive_errors = N`, the Nth consecutive
/// error still arms, and the N+1th trips (`consecutive_errors > max`).
///
/// `chain` carries per-leg market data for selection, when installed: `None`
/// preserves the pinned batch legacy byte-identically (spread labels book as
/// single legs); `Some` with a multi-leg proposal selects legs as of this
/// bar through the frozen selector and attaches them to the intent, so the
/// existing spread fill path prices them.
#[allow(clippy::too_many_arguments)]
pub async fn decide_and_submit<R: SubmissionRecorder, V: Venue>(
    strategy: &Strategy,
    risk: &RiskEngine,
    grant: &Grant,
    usage: &mut GrantUsage,
    venue: &V,
    recorder: &R,
    quote_config: &BacktestConfig,
    breakers: &mut BreakerState,
    health: BarHealth,
    bar_index: usize,
    bar: &Bar,
    closes: &[f64],
    config: &RunnerConfig,
    seed: u64,
    chain: Option<&dyn LegQuoteSource>,
) -> BarDecision {
    let proposal_id = format!("run-{seed}-{bar_index}");
    let ts = bar.ts.to_rfc3339();
    let Some(quote) = quote_for_bar(bar, quote_config) else {
        return skipped_decision(
            bar_index,
            ts,
            proposal_id,
            "unquotable bar: non-positive close",
        );
    };
    let Some(spread_width) = quote.spread_width() else {
        return skipped_decision(bar_index, ts, proposal_id, "crossed quote");
    };
    let limit_price = Money::new(bar.c);
    let input = ProposeInput {
        strategy: &config.strategy_name,
        underlying: &config.underlying,
        side: ProposalSide::Entry,
        closes,
        limit_price,
        max_loss: config.max_loss,
        notional: config.notional,
        dte_days: config.dte_days,
        created_at: bar.ts,
    };
    let mut proposal = match strategy.propose(&proposal_id, input) {
        Ok(proposal) => proposal,
        Err(e) => {
            return skipped_decision(bar_index, ts, proposal_id, &format!("signal refused: {e}"));
        }
    };
    // Leg selection runs before any gate journals: a multi-leg proposal with
    // an installed chain source resolves to concrete legs as of this bar
    // (the bar-derived touch is the quote mid by construction), and any
    // refusal skips with the lifecycle untouched. Without a source the
    // pinned legacy flows unchanged — that divergence is preserved here,
    // never fixed.
    let selected_legs = if needs_chain_data(proposal.structure) {
        match chain {
            None => None,
            Some(source) => {
                let snapshot = source.chain_snapshot(&config.underlying, bar.ts);
                if snapshot.is_empty() {
                    return skipped_decision(
                        bar_index,
                        ts,
                        proposal_id,
                        "no chain quotes at-or-before this bar",
                    );
                }
                match LegSelector::new().build().select_legs(
                    proposal.structure,
                    config.dte_days,
                    &config.underlying,
                    Money::new(bar.c),
                    bar.ts.date_naive(),
                    &snapshot,
                ) {
                    Ok(legs) => Some(legs),
                    Err(refusal) => {
                        return skipped_decision(bar_index, ts, proposal_id, &refusal.to_string());
                    }
                }
            }
        }
    } else {
        None
    };
    // Orders rest at the touch so backtest flow is marketable under the same
    // liquidity filter paper enforces; the proposal limit marks the bar close
    // and the checked limit is the submitted price.
    let checked = checked_order_for(proposal.direction, &quote);
    // The order's client key: the proposal id plus its single attempt. The
    // same key names the intent and every lifecycle transition, so replay
    // rebuilds one proposal history.
    let client_id = protopyne_venue::client_id_for(&proposal.id, 0);
    let state = RiskState {
        quote_age_secs: health.quote_age_secs,
        clock_skew_secs: health.clock_skew_secs,
        reconciliation_healthy: health.reconciliation_healthy,
        is_limit_order: true,
        is_spread: true,
    };
    // Retain the token the engine mints on approval: the submission protocol
    // demands it, so the runner cannot reach the venue without it.
    let risk_check = risk.check(&proposal, &state);
    let (risk_pass, risk_reasons) = match &risk_check {
        Ok(_) => (true, Vec::new()),
        Err(refused) => (
            false,
            refused
                .reasons
                .iter()
                .map(|reason| format!("{reason:?}"))
                .collect(),
        ),
    };
    let market = GrantMarket::new()
        .quote_age_secs(health.quote_age_secs)
        .spread_width(spread_width)
        .build();
    // Merit verdicts are computed without breaker input: a tripped breaker
    // must never turn an Allow into a Deny. The disarm below enforces the
    // trip at submission instead, so the recorded verdict stays honest about
    // the trade while the run stops opening exposure.
    let clean_breakers = BreakerState::new().build();
    let (grant_allow, grant_reasons) =
        match evaluate_grant(&proposal, grant, usage, &market, &clean_breakers, bar.ts) {
            GrantDecision::Allow => (true, Vec::new()),
            GrantDecision::Deny { reasons } => (false, reasons),
        };
    // The decision skeleton carries the proposal's own attribution and the
    // checked limit; the gates and the lifecycle below fill it in.
    let mut decision = proposed_decision(
        &proposal,
        bar_index,
        ts,
        checked,
        risk_pass,
        risk_reasons,
        grant_allow,
        grant_reasons,
    );
    // Drive the real proposal lifecycle through the risk and grant verdicts,
    // journaling each move before the decision acts on the new state. A move
    // the state machine refuses (impossible by construction) fails the bar
    // closed rather than recording a transition that did not happen.
    if let Err(e) = advance_gates(
        &mut proposal,
        recorder,
        &client_id,
        risk_pass,
        grant_allow,
        bar.ts,
        &mut decision.transitions,
    ) {
        decision.skipped = Some(e.to_string());
        return decision;
    }
    if !(decision.risk_pass && decision.grant_allow) {
        return decision;
    }
    if (breakers.tripped || breakers.consecutive_errors > grant.breakers.max_consecutive_errors)
        && proposal.side == ProposalSide::Entry
    {
        // Entries are disarmed for this run: withdraw approval rather than
        // parking the proposal at `AutoApproved`, so the served state is
        // terminal and the journal shows why nothing was submitted.
        match advance(
            &mut proposal,
            recorder,
            &client_id,
            ProposalEvent::Cancel,
            "entries disarmed: breaker tripped",
            bar.ts,
        ) {
            Ok(entry) => decision.transitions.push(entry),
            Err(e) => {
                decision.skipped = Some(e.to_string());
                return decision;
            }
        }
        decision.skipped = Some(format!(
            "breaker tripped: entries disarmed after {} consecutive venue errors (max {})",
            breakers.consecutive_errors, grant.breakers.max_consecutive_errors,
        ));
        return decision;
    }
    let Some(contracts) = proposal.contracts() else {
        // A fractional or out-of-range quantity can never be submitted:
        // withdraw approval for the same reason as above.
        match advance(
            &mut proposal,
            recorder,
            &client_id,
            ProposalEvent::Cancel,
            "quantity unrepresentable",
            bar.ts,
        ) {
            Ok(entry) => decision.transitions.push(entry),
            Err(e) => {
                decision.skipped = Some(e.to_string());
                return decision;
            }
        }
        decision.skipped = Some("unrepresentable quantity".to_owned());
        return decision;
    };
    let intent = OrderIntent::new()
        .client_id(client_id.clone())
        .proposal_id(proposal.id.clone())
        .strategy(proposal.strategy.clone())
        .underlying(proposal.underlying.clone())
        .side(checked.side)
        .quantity(contracts)
        .limit_price(checked.limit)
        .maybe_legs(selected_legs)
        .build();
    debug_assert_eq!(
        intent.limit_price, checked.limit,
        "the submitted limit must be the checked limit"
    );
    decision.submitted_quantity = intent.quantity;
    // Risk passed (guarded above), so the engine's token is present; the
    // protocol demands it, and a missing token can never reach the venue.
    let Ok(approved) = &risk_check else {
        decision.skipped = Some("risk approval missing for an approved proposal".to_owned());
        return decision;
    };
    // Hand the approved proposal to the venue: the lifecycle advances to
    // `Submitted` before the call, so the intent to trade is journaled even
    // when the verdict never returns.
    match advance(
        &mut proposal,
        recorder,
        &client_id,
        ProposalEvent::Submit,
        "handed to venue",
        bar.ts,
    ) {
        Ok(entry) => decision.transitions.push(entry),
        Err(e) => {
            decision.skipped = Some(e.to_string());
            return decision;
        }
    }
    // Only producer-health failures feed the confidence-axis counter: a
    // refused order speaks to the trade's merit, never to the venue's health,
    // so refusals leave the breaker alone. Success clears a cooling streak;
    // once tripped, the flag and its banked reason stay put for the run.
    let record = match submit_guarded(recorder, venue, approved, intent, &quote, bar.ts).await {
        Ok(record) => {
            if !breakers.tripped {
                breakers.consecutive_errors = 0;
            }
            record
        }
        Err(e) => {
            if e.is_retryable() {
                breakers.consecutive_errors = breakers.consecutive_errors.saturating_add(1);
                if !breakers.tripped
                    && breakers.consecutive_errors > grant.breakers.max_consecutive_errors
                {
                    breakers.tripped = true;
                    breakers.trip_reason = Some(format!(
                        "{} consecutive venue errors exceed max {}",
                        breakers.consecutive_errors, grant.breakers.max_consecutive_errors,
                    ));
                }
            } else {
                // A permanent refusal terminalizes the proposal. A retryable
                // failure leaves it `Submitted`: the intent is in the outbox
                // and query-by-ID resolves the unknown verdict, so the served
                // state never claims a rejection the venue did not make.
                match advance(
                    &mut proposal,
                    recorder,
                    &client_id,
                    ProposalEvent::Reject,
                    "venue refused",
                    bar.ts,
                ) {
                    Ok(entry) => decision.transitions.push(entry),
                    Err(advance_error) => {
                        decision.skipped = Some(advance_error.to_string());
                        return decision;
                    }
                }
            }
            decision.skipped = Some(e.to_string());
            return decision;
        }
    };
    // Map the venue verdict onto the remaining lifecycle events. The
    // sequence is legal by construction and the runner's recorder is
    // in-memory/infallible; if the recorder itself fails mid-way, the fill
    // the venue already reported still books — the failure rides on the
    // decision as `lifecycle_error` instead of vanishing into a drop.
    if let Err(e) = advance_verdict(
        &mut proposal,
        recorder,
        &client_id,
        record.status,
        bar.ts,
        &mut decision.transitions,
    ) {
        decision.lifecycle_error = Some(e.to_string());
    }
    let filled = record.filled_quantity;
    let submitted = record.intent.quantity;
    usage.used_trades = usage.used_trades.saturating_add(1);
    usage.used_contracts = usage.used_contracts.saturating_add(filled);
    usage.used_loss = Money::new(usage.used_loss.as_decimal() + proposal.max_loss.as_decimal());
    usage.used_notional =
        Money::new(usage.used_notional.as_decimal() + proposal.notional.as_decimal());
    decision.submitted = true;
    decision.submitted_quantity = submitted;
    decision.filled_quantity = filled;
    decision.fill_price = record.avg_price;
    decision.fee = Some(record.fees_paid);
    decision
}

/// One completed backtest run: pin, per-bar verdicts, and honest report.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BacktestRun {
    /// Reproducibility pin (data, code, config).
    pub manifest: RunManifest,
    /// Per-bar verdicts in replay order.
    pub decisions: Vec<BarDecision>,
    /// Metrics with stated assumptions and limits.
    pub report: BacktestReport,
}

impl BacktestRun {
    /// Deterministic JSON over manifest plus decisions for rerun comparison.
    /// Decision serialization cannot fail in practice; a failure still
    /// reports loudly instead of emitting an empty body.
    #[must_use]
    pub fn to_canonical_json(&self) -> String {
        let decisions = serde_json::to_string(&self.decisions).unwrap_or_else(|e| {
            format!(
                "{{\"serialization_error\":{}}}",
                serde_json::to_string(&e.to_string())
                    .unwrap_or_else(|_| "\"unencodable\"".to_owned())
            )
        });
        format!("{}\n{decisions}", self.manifest.to_canonical_json())
    }
}

/// Mark-to-close equity curve for filled decisions.
///
/// Cash starts at zero; buys pay `price * filled + fee`, sells receive
/// `price * filled - fee`. The fold is keyed on `BarDecision.bar_index`:
/// each bar marks the running state after applying only that bar's fills,
/// so bars before any fill read flat zero and every later bar sees exactly
/// the fills so far — never future fills. Bars with no fills carry the
/// previous mark forward. Quantities come from the decision's
/// filled-quantity carrier — the same count the venue journaled — so sizing
/// flows through every booked number. Pure and deterministic.
#[must_use]
pub fn equity_curve_for(bars: &[Bar], decisions: &[BarDecision]) -> Vec<Money> {
    use std::collections::HashMap;
    fn book_fill(cash: &mut Decimal, net: &mut i64, decision: &BarDecision) {
        if !decision.submitted {
            return;
        }
        let (Some(price), Some(fee)) = (decision.fill_price, decision.fee) else {
            return;
        };
        let qty = decision.filled_quantity;
        let signed: i64 = match decision.direction {
            Some(Direction::Short) => -(i64::from(qty)),
            _ => i64::from(qty),
        };
        let flow = price.as_decimal() * Decimal::from(qty);
        if signed >= 0 {
            *cash -= flow + fee.as_decimal();
        } else {
            *cash += flow - fee.as_decimal();
        }
        *net += signed;
    }
    let mut by_bar: HashMap<usize, Vec<&BarDecision>> = HashMap::new();
    for decision in decisions {
        by_bar.entry(decision.bar_index).or_default().push(decision);
    }
    let mut cash = Decimal::ZERO;
    let mut net: i64 = 0;
    let mut curve = Vec::with_capacity(bars.len());
    for (index, bar) in bars.iter().enumerate() {
        if let Some(due) = by_bar.get(&index) {
            for decision in due {
                book_fill(&mut cash, &mut net, decision);
            }
        }
        curve.push(Money::new(cash + Decimal::from(net) * bar.c));
    }
    curve
}

/// Pin a run manifest, attaching planner provenance when the source had a
/// planner to name. A source with no provenance (files, dynamic) pins the
/// pre-tape manifest exactly as before; a store-backed source pins the
/// digest and spans the read produced. One pinning site, so a run's bytes
/// and its inputs cannot diverge between the single- and multi-symbol paths.
fn pinned_manifest(
    data_version: &str,
    config_json: String,
    bars: &[Bar],
    seed: u64,
    provenance: &[SpanPlan],
) -> RunManifest {
    let manifest = RunManifest::new(data_version, config_json, bars, seed);
    if provenance.is_empty() {
        manifest
    } else {
        manifest.with_tape_data(bars, provenance)
    }
}

/// The canonical JSON of the runner + venue + risk + grant configuration.
fn config_json_of(
    runner_config: &RunnerConfig,
    venue_config: &BacktestConfig,
    risk: &RiskEngine,
    grant: &Grant,
) -> Result<String, ServerError> {
    serde_json::to_string(&(runner_config, venue_config, risk.limits(), grant))
        .map_err(|e| ServerError::Backtest(format!("config not serializable: {e}")))
}

/// Run the full decision stack over `bars` through a versioned venue.
///
/// Determinism is a **(correctness)** invariant, not a tunable: same inputs
/// always produce byte-identical manifests and decisions.
#[allow(clippy::too_many_arguments)]
pub async fn run_backtest(
    strategy: &Strategy,
    risk: &RiskEngine,
    grant: &Grant,
    venue_config: &BacktestConfig,
    runner_config: &RunnerConfig,
    bars: &[Bar],
    seed: u64,
) -> Result<BacktestRun, ServerError> {
    runner_config.validate()?;
    venue_config.validate()?;
    if bars.is_empty() {
        return Err(ServerError::Backtest(
            "history must not be empty".to_owned(),
        ));
    }
    let paper = PaperConfig::new().build();
    let venue = BacktestVenue::new(venue_config.clone(), bars.to_vec(), paper)
        .map_err(|e| ServerError::Backtest(e.to_string()))?;
    let config_json = config_json_of(runner_config, venue_config, risk, grant)?;
    let manifest = pinned_manifest(
        &venue_config.data_version,
        config_json,
        venue.bars(),
        seed,
        &[],
    );
    run_backtest_on_venue(
        strategy,
        risk,
        grant,
        &venue,
        venue_config,
        runner_config,
        venue.bars(),
        seed,
        manifest,
        HistoryKind::Dynamic,
        false,
    )
    .await
}

/// Run the full decision stack over a [`HistorySource`].
///
/// The one difference from [`run_backtest`] is the input: the bars — and the
/// planner provenance the tape arm carries — come from [`load_history`],
/// then enter the identical [`run_backtest_on_venue`] decide path. A files or
/// dynamic source pins no tape data and serializes exactly as a values run;
/// a store-backed source pins the read's digest and spans. The `store` is
/// only consulted by the tape arm.
#[allow(clippy::too_many_arguments)]
pub async fn run_backtest_from_source(
    strategy: &Strategy,
    risk: &RiskEngine,
    grant: &Grant,
    venue_config: &BacktestConfig,
    runner_config: &RunnerConfig,
    source: &HistorySource,
    store: Option<&dyn CandleStore>,
    seed: u64,
) -> Result<BacktestRun, ServerError> {
    runner_config.validate()?;
    venue_config.validate()?;
    let loaded = load_history(source, store)?;
    let planner_certified = !loaded.provenance.is_empty();
    let config_json = config_json_of(runner_config, venue_config, risk, grant)?;
    let manifest = pinned_manifest(
        &venue_config.data_version,
        config_json,
        &loaded.bars,
        seed,
        &loaded.provenance,
    );
    // Single ownership per run: the loaded bars move into the venue and the
    // decide path borrows them back from it — no second `Vec` beside the
    // venue's own history.
    let venue = BacktestVenue::new(
        venue_config.clone(),
        loaded.bars,
        PaperConfig::new().build(),
    )
    .map_err(|e| ServerError::Backtest(e.to_string()))?;
    run_backtest_on_venue(
        strategy,
        risk,
        grant,
        &venue,
        venue_config,
        runner_config,
        venue.bars(),
        seed,
        manifest,
        HistoryKind::of(source),
        planner_certified,
    )
    .await
}

/// Run the shared decide path over `bars` through a caller-supplied venue.
///
/// The single-symbol `run_backtest` and the multi-symbol harness both enter
/// here, so one code path stays one code path: a divergence between rehearsal
/// shapes is a bug, never a modeling choice. Config and bar validation stay
/// with the callers (they own the input shape); the manifest arrives pinned.
#[allow(clippy::too_many_arguments)]
async fn run_backtest_on_venue<V: Venue>(
    strategy: &Strategy,
    risk: &RiskEngine,
    grant: &Grant,
    venue: &V,
    venue_config: &BacktestConfig,
    runner_config: &RunnerConfig,
    bars: &[Bar],
    seed: u64,
    manifest: RunManifest,
    kind: HistoryKind,
    planner_certified: bool,
) -> Result<BacktestRun, ServerError> {
    // One loop body for batch and interactive runs: the session owns the
    // usage, breaker, closes prefix, and ephemeral journal, and steps each
    // bar through the shared decision stack. Driving it to completion here
    // reproduces the historical inline loop decision-for-decision.
    let mut session = crate::replay_session::ReplaySession::start(
        strategy,
        risk,
        grant,
        venue,
        venue_config.clone(),
        runner_config.clone(),
        crate::replay_session::ChainPolicy::Unscreened,
        bars,
        seed,
    )?;
    while session.step().await.is_some() {}
    let decisions = session.decisions().to_vec();
    let skipped_warmup = session.skipped_warmup();
    let equity = equity_curve_for(bars, &decisions);
    let fills = decisions
        .iter()
        .filter(|decision| decision.submitted)
        .count() as u64;
    let proposals = decisions
        .iter()
        .filter(|decision| decision.direction.is_some())
        .count() as u64;
    let total_fees = decisions
        .iter()
        .filter_map(|decision| decision.fee)
        .fold(Decimal::ZERO, |sum, fee| sum + fee.as_decimal());
    let filled_notional = decisions
        .iter()
        .filter_map(|decision| {
            decision
                .fill_price
                .map(|price| price.as_decimal() * Decimal::from(decision.filled_quantity))
        })
        .fold(Decimal::ZERO, |sum, leg| sum + leg);
    let reserved_notional = runner_config.notional.as_decimal() * Decimal::from(proposals);
    let unquotable = count_unquotable_bars(bars);
    let assumptions = vec![
        format!(
            "quotes synthesized from closed-bar close with spread {} bps, size {}",
            venue_config.quote_spread_bps, venue_config.quote_size
        ),
        "fills share paper's spread-aware fill code (liquidity filter, slippage, fees)".to_owned(),
        "orders rest at the touch (buy ask, sell bid); proposal limits mark the bar close"
            .to_owned(),
        format!("max loss {} per proposal", runner_config.max_loss),
    ];
    // Limits are the source's facts, never asserted: the arm names itself, the
    // declared capability set names the prototype's absences, and an uncertified
    // source names the missing planner provenance.
    let mut limits = vec![
        history_source_limit(kind, &venue_config.data_version, bars.len()),
        format!("{unquotable} unquotable bars skipped; {skipped_warmup} warmup bars skipped"),
        ABSENT_CAPABILITIES.to_owned(),
    ];
    if let Some(omission) = uncertified_coverage_limit(planner_certified) {
        limits.push(omission);
    }
    let mut report = build_report(
        manifest.clone(),
        assumptions,
        limits,
        proposals,
        fills,
        Money::new(total_fees),
        Money::new(filled_notional),
        Money::new(reserved_notional),
        equity,
        report_rsrs(bars),
        // Batch installs no chain source: the run's chain facts are absent,
        // so the absence line stays verbatim and no chain line joins.
        None,
    );
    // Portfolio aggregation over the run's fills: decisions are the runner's
    // fill record, keyed back to bars by replay index (built in lockstep
    // above, so the lookup cannot miss — a miss fails the run, never drops
    // a fill). Only priced fills book: a fill without a price has no
    // economics to attribute. Marks are bar closes, the same tape the equity
    // curve marks.
    let mut booked: Vec<BookedFill> = Vec::new();
    for decision in &decisions {
        let (Some(direction), Some(price)) = (decision.direction, decision.fill_price) else {
            continue;
        };
        if !decision.submitted || decision.filled_quantity == 0 {
            continue;
        }
        let Some(bar) = bars.get(decision.bar_index) else {
            return Err(ServerError::Backtest(format!(
                "decision {} indexes bar {} of {}",
                decision.proposal_id,
                decision.bar_index,
                bars.len()
            )));
        };
        booked.push(
            BookedFill::new()
                .portfolio(PAPER_PORTFOLIO.to_owned())
                .underlying(runner_config.underlying.clone())
                .structure(decision.structure.map_or_else(
                    || "unrecorded".to_owned(),
                    |structure| format!("{structure:?}"),
                ))
                .strategy(runner_config.strategy_name.clone())
                .grant(grant.id.clone())
                .side(side_for_direction(direction))
                .quantity(decision.filled_quantity)
                .price(price)
                .fee(decision.fee.unwrap_or_else(Money::zero))
                .trade_id(protopyne_venue::client_id_for(&decision.proposal_id, 0))
                .at(bar.ts)
                .build(),
        );
    }
    let marks: Vec<(chrono::DateTime<chrono::Utc>, Money)> =
        bars.iter().map(|bar| (bar.ts, Money::new(bar.c))).collect();
    report.portfolio = Some(aggregate(&booked)?.summarize(&marks)?);
    Ok(BacktestRun {
        manifest,
        decisions,
        report,
    })
}

/// One symbol's leg of a multi-symbol run: its configs plus its bar source.
///
/// Histories arrive from files or dynamically — the harness loads either arm
/// through `load_history`, so a symbol with no file replays beside one with
/// a pinned file through the identical decide path.
#[derive(Debug, Clone, bon::Builder)]
#[builder(start_fn = new)]
pub struct MultiSymbolInput {
    /// Quote synthesis plus data-version pin for this symbol.
    pub venue_config: BacktestConfig,
    /// Attribution plus economics for this symbol.
    pub runner_config: RunnerConfig,
    /// Bar source for this symbol.
    pub history: HistorySource,
    /// The durable store a [`HistorySource::Tape`] leg reads through.
    /// `None` for the file and dynamic arms, which name no store; a tape leg
    /// with no store refuses through [`load_history`], never silently
    /// substituting another source.
    pub store: Option<Arc<dyn CandleStore>>,
}

/// Combined reproducibility pin for a multi-symbol run: one per-symbol pin
/// (data version, code version, config) in harness input order, under the
/// shared suite seed. Serialization order is struct order, so reruns are
/// byte-identical.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MultiRunManifest {
    /// Crate version at run time, shared across symbols.
    pub code_version: String,
    /// Caller-supplied seed shared across symbols (0 = none).
    pub seed: u64,
    /// Per-symbol pins, in harness input order.
    pub symbols: Vec<RunManifest>,
}

impl MultiRunManifest {
    /// Deterministic JSON encoding of the combined pin. Same inputs always
    /// produce the same bytes; a failure still reports loudly inside the
    /// payload instead of emitting an empty object.
    #[must_use]
    pub fn to_canonical_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|e| {
            format!(
                "{{\"serialization_error\":{}}}",
                serde_json::to_string(&e.to_string())
                    .unwrap_or_else(|_| "\"unencodable\"".to_owned())
            )
        })
    }
}

/// One completed multi-symbol run: the combined pin plus per-symbol runs in
/// harness input order.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MultiBacktestRun {
    /// Combined reproducibility pin.
    pub manifest: MultiRunManifest,
    /// Per-symbol runs, in harness input order.
    pub runs: Vec<BacktestRun>,
}

/// One multi-symbol job: a full leg run under its own history and venue.
/// Fully owned so pool workers stay `Send` with no borrowed state.
struct MultiJob {
    /// Index into the harness input list (output order is by index).
    index: usize,
    /// Shared decision stack, cloned per leg (element `Arc`s bump, nothing
    /// duplicates).
    strategy: Strategy,
    /// Fresh risk limits per leg (each leg tunes its own engine).
    risk_limits: protopyne_strategy::RiskLimits,
    /// Grant under evaluation, shared read-only across legs.
    grant: Grant,
    /// Shared paper tuning every leg's venue builds from.
    paper: PaperConfig,
    /// Quote synthesis plus data-version pin for this leg.
    venue_config: BacktestConfig,
    /// Attribution plus economics for this leg.
    runner_config: RunnerConfig,
    /// Loaded bars for this leg.
    bars: Vec<Bar>,
    /// Pinned manifest for this leg, built up front in input order.
    manifest: RunManifest,
    /// Which arm this leg's bars came from, for `data_limits`.
    kind: HistoryKind,
    /// Whether this leg's source carried planner provenance.
    planner_certified: bool,
    /// Suite seed shared across legs.
    seed: u64,
}

/// Run the shared decide path once per symbol over one shared venue set.
///
/// Every venue tunes from the same `paper` config (the set is built together,
/// never one hidden default per call), and every symbol enters the same
/// `run_backtest_on_venue` the single-symbol path uses — per-symbol decisions
/// equal standalone runs bit for bit. Legs fan out over the shared
/// [`ResultPool`](fluent_concurrency::pool::ResultPool) with outcomes
/// re-sorted by input index, so parallelism never perturbs determinism:
/// reruns are byte-identical with or without the pool. Workers clamp to
/// 1..=8 and the queue fits exactly one slot per leg, so oversubscription
/// waits its turn — no per-bar spawn, the pool lives for the run.
/// Underlyings must be unique: two legs on one symbol would attribute fills
/// ambiguously, so duplicates refuse fail-closed before anything spawns.
pub async fn run_multi_backtest(
    strategy: &Strategy,
    risk: &RiskEngine,
    grant: &Grant,
    paper: PaperConfig,
    inputs: Vec<MultiSymbolInput>,
    seed: u64,
) -> Result<MultiBacktestRun, ServerError> {
    if inputs.is_empty() {
        return Err(ServerError::Backtest(
            "multi-symbol run needs at least one symbol".to_owned(),
        ));
    }
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for input in &inputs {
        if !seen.insert(input.runner_config.underlying.as_str()) {
            return Err(ServerError::Backtest(format!(
                "duplicate symbol '{}': legs must be unique",
                input.runner_config.underlying
            )));
        }
    }
    // Histories and manifests load sequentially up front: a bad source or
    // config refuses before the pool spawns, and manifest order stays input
    // order regardless of completion order.
    let mut legs: Vec<MultiJob> = Vec::with_capacity(inputs.len());
    for (index, input) in inputs.into_iter().enumerate() {
        input.runner_config.validate()?;
        input.venue_config.validate()?;
        let loaded = load_history(&input.history, input.store.as_deref())?;
        let bars = loaded.bars;
        let config_json = config_json_of(&input.runner_config, &input.venue_config, risk, grant)?;
        let manifest = pinned_manifest(
            &input.venue_config.data_version,
            config_json,
            &bars,
            seed,
            &loaded.provenance,
        );
        legs.push(MultiJob {
            index,
            strategy: strategy.clone(),
            risk_limits: risk.limits().clone(),
            grant: grant.clone(),
            paper: paper.clone(),
            venue_config: input.venue_config,
            runner_config: input.runner_config,
            bars,
            manifest,
            kind: HistoryKind::of(&input.history),
            planner_certified: !loaded.provenance.is_empty(),
            seed,
        });
    }
    let workers = legs.len().clamp(1, 8);
    let pool = fluent_concurrency::pool::ResultPool::new(
        fluent_concurrency::tokio_runtime(),
        workers,
        legs.len().max(1),
        move |job: MultiJob| async move {
            // Single ownership per leg: the leg's bars move into its venue
            // and the decide path borrows them back — pool workers share
            // nothing mutable and copy nothing per bar.
            let venue = BacktestVenue::new(job.venue_config.clone(), job.bars, job.paper.clone())
                .map_err(|e| e.to_string())?;
            let risk = RiskEngine::new(job.risk_limits);
            let run = run_backtest_on_venue(
                &job.strategy,
                &risk,
                &job.grant,
                &venue,
                &job.venue_config,
                &job.runner_config,
                venue.bars(),
                job.seed,
                job.manifest,
                job.kind,
                job.planner_certified,
            )
            .await
            .map_err(|e| e.to_string())?;
            Ok::<(usize, BacktestRun), String>((job.index, run))
        },
    );
    let mut indexed = Vec::with_capacity(legs.len());
    for job in legs {
        let (index, run) = pool
            .submit(job)
            .await
            .map_err(|e| ServerError::Backtest(format!("multi submit failed: {e:?}")))?;
        indexed.push((index, run));
    }
    pool.shutdown().await;
    indexed.sort_by_key(|(index, _)| *index);
    let mut symbols = Vec::with_capacity(indexed.len());
    let mut runs = Vec::with_capacity(indexed.len());
    for (_, run) in indexed {
        symbols.push(run.manifest.clone());
        runs.push(run);
    }
    Ok(MultiBacktestRun {
        manifest: MultiRunManifest {
            code_version: env!("CARGO_PKG_VERSION").to_owned(),
            seed,
            symbols,
        },
        runs,
    })
}

/// Replay the same decision stack against a live-shape paper venue.
///
/// Used by the parity test: identical inputs must produce identical
/// decisions whether the venue is versioned history or paper.
pub async fn run_paper_decisions(
    strategy: &Strategy,
    risk: &RiskEngine,
    grant: &Grant,
    venue: &PaperVenue,
    runner_config: &RunnerConfig,
    bars: &[Bar],
    seed: u64,
) -> Vec<BarDecision> {
    // The paper feed has no pinned history, so it states its own synthesis
    // assumptions under an explicit paper label (default spread/size, same
    // values the shared path always used — now named instead of hidden).
    let quote_config = BacktestConfig::new()
        .underlying(runner_config.underlying.clone())
        .data_version("paper-synthetic".to_owned())
        .build();
    // Same session, paper venue: identical inputs produce identical decisions
    // whether the venue is versioned history or paper.
    if bars.is_empty() {
        return Vec::new();
    }
    let mut session = crate::replay_session::ReplaySession::start(
        strategy,
        risk,
        grant,
        venue,
        quote_config,
        runner_config.clone(),
        crate::replay_session::ChainPolicy::Unscreened,
        bars,
        seed,
    )
    .expect("paper replay validates: configs are built, bars are lent");
    while session.step().await.is_some() {}
    session.decisions().to_vec()
}

/// One sweep outcome: which variant ran and what it filled.
#[derive(Debug, Clone)]
pub struct SweepOutcome {
    /// Index into the submitted variant list (output order is by index).
    pub index: usize,
    /// Decisions made under this variant.
    pub decisions: Vec<BarDecision>,
    /// Orders filled under this variant.
    pub fills: u64,
    /// Manifest pin for this variant.
    pub manifest: RunManifest,
}

/// One sweep job: a full run under one grant variant.
#[derive(Debug, Clone)]
struct SweepJob {
    index: usize,
    grant: Grant,
}

/// Shared sweep inputs behind one `Arc` so pool workers stay `Send`.
struct SweepShared {
    strategy: Arc<Strategy>,
    risk_limits: protopyne_strategy::RiskLimits,
    venue_config: BacktestConfig,
    runner_config: RunnerConfig,
    bars: Vec<Bar>,
    seed: u64,
}

/// Run one grant variant per entry in `grants` concurrently.
///
/// Fans out over the shared [`ResultPool`](fluent_concurrency::pool::ResultPool);
/// outcomes return sorted by variant index so sweeps stay deterministic.
/// `Strategy` crosses worker boundaries behind an `Arc` (shared ownership,
/// no duplication).
pub async fn run_sweep(
    strategy: Arc<Strategy>,
    risk_limits: protopyne_strategy::RiskLimits,
    venue_config: &BacktestConfig,
    runner_config: &RunnerConfig,
    bars: &[Bar],
    grants: Vec<Grant>,
    seed: u64,
) -> Result<Vec<SweepOutcome>, ServerError> {
    if grants.is_empty() {
        return Err(ServerError::Backtest(
            "sweep needs at least one variant".to_owned(),
        ));
    }
    if bars.is_empty() {
        return Err(ServerError::Backtest(
            "history must not be empty".to_owned(),
        ));
    }
    let shared = Arc::new(SweepShared {
        strategy,
        risk_limits,
        venue_config: venue_config.clone(),
        runner_config: runner_config.clone(),
        bars: bars.to_vec(),
        seed,
    });
    // Bounded fan-out: workers clamp to 1..=8 and the queue fits exactly one
    // slot per variant, so oversubscription waits its turn — the queue never
    // observes `Full` and never grows past the variant count.
    let workers = grants.len().clamp(1, 8);
    let pool = fluent_concurrency::pool::ResultPool::new(
        fluent_concurrency::tokio_runtime(),
        workers,
        grants.len().max(1),
        move |job: SweepJob| {
            let shared = Arc::clone(&shared);
            async move {
                let risk = RiskEngine::new(shared.risk_limits.clone());
                let run = run_backtest(
                    &shared.strategy,
                    &risk,
                    &job.grant,
                    &shared.venue_config,
                    &shared.runner_config,
                    &shared.bars,
                    shared.seed,
                )
                .await
                .map_err(|e| e.to_string())?;
                let fills = run
                    .decisions
                    .iter()
                    .filter(|decision| decision.submitted)
                    .count() as u64;
                Ok::<SweepOutcome, String>(SweepOutcome {
                    index: job.index,
                    decisions: run.decisions,
                    fills,
                    manifest: run.manifest,
                })
            }
        },
    );
    let mut outcomes = Vec::with_capacity(grants.len());
    for (index, grant) in grants.into_iter().enumerate() {
        let outcome = pool
            .submit(SweepJob { index, grant })
            .await
            .map_err(|e| ServerError::Backtest(format!("sweep submit failed: {e:?}")))?;
        outcomes.push(outcome);
    }
    pool.shutdown().await;
    outcomes.sort_by_key(|outcome| outcome.index);
    Ok(outcomes)
}

/// Per-step research state for the replay graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayStep {
    /// Index into the replayed history.
    pub bar_index: usize,
    /// Bar timestamp (RFC 3339).
    pub ts: String,
    /// Whether this step has been marked complete.
    pub done: bool,
}

/// Build a research-only replay graph over `bars`.
///
/// Each bar is a step depending on its predecessor; a checkpoint lands every
/// `checkpoint_every` bars. Composes [`CheckpointedStepGraph`] rather than
/// reimplementing step/rewind bookkeeping. Research only: this graph never
/// owns live order state.
pub fn build_replay(
    bars: &[Bar],
    checkpoint_every: usize,
) -> Result<CheckpointedStepGraph<String, ReplayStep>, ServerError> {
    if bars.is_empty() {
        return Err(ServerError::Backtest(
            "history must not be empty".to_owned(),
        ));
    }
    if checkpoint_every == 0 {
        return Err(ServerError::Backtest(
            "checkpoint_every must be at least 1".to_owned(),
        ));
    }
    let mut graph: CheckpointedStepGraph<String, ReplayStep> = CheckpointedStepGraph::new();
    for (index, bar) in bars.iter().enumerate() {
        let key = format!("bar-{index}");
        let deps = if index == 0 {
            Vec::new()
        } else {
            vec![format!("bar-{}", index - 1)]
        };
        graph
            .add_step(
                key.clone(),
                &deps,
                ReplayStep {
                    bar_index: index,
                    ts: bar.ts.to_rfc3339(),
                    done: false,
                },
            )
            .map_err(|e| ServerError::Backtest(format!("replay step rejected: {e:?}")))?;
        if index % checkpoint_every == 0 {
            graph
                .checkpoint(key)
                .map_err(|e| ServerError::Backtest(format!("checkpoint rejected: {e:?}")))?;
        }
    }
    Ok(graph)
}

/// Mark every step of `graph` complete in insertion order.
pub fn complete_replay(graph: &mut CheckpointedStepGraph<String, ReplayStep>) {
    let keys: Vec<String> = graph.step_ids().to_vec();
    for key in &keys {
        graph.complete(key);
        if let Some(state) = graph.state_mut(key) {
            state.done = true;
        }
    }
}

/// Triple-barrier labeling over closed bars: for each entry bar `i`, scan
/// the next `window - 1` bars for the first touch of the profit barrier
/// (`entry * (1 + take_profit)` on highs) or the loss barrier
/// (`entry * (1 - stop_loss)` on lows).
///
/// Labels are `Some(1.0)` (profit first), `Some(-1.0)` (loss first), or the
/// horizon bar's signed drift when time expires (flat expiry reads
/// `Some(0.0)`). A bar touched on both sides resolves to whichever extreme
/// stands farther from its close. Entries with no window left, non-finite
/// entries, and degenerate parameters (`window < 2`, non-positive or
/// non-finite barriers) read `None`: labels are future data for `y`, so a
/// missing label refuses rather than invents. Hand-rolled loop, no numba.
#[must_use]
pub fn triple_barrier(
    closes: &[f64],
    highs: &[f64],
    lows: &[f64],
    window: usize,
    take_profit: f64,
    stop_loss: f64,
) -> Vec<Option<f64>> {
    let len = closes.len().min(highs.len()).min(lows.len());
    if window < 2
        || !take_profit.is_finite()
        || !stop_loss.is_finite()
        || take_profit <= 0.0
        || stop_loss <= 0.0
    {
        return vec![None; len];
    }
    (0..len)
        .map(|i| {
            if i + window > len {
                return None;
            }
            let entry = closes[i];
            if !entry.is_finite() {
                return None;
            }
            let upper = entry * (1.0 + take_profit);
            let lower = entry * (1.0 - stop_loss);
            for j in i + 1..i + window {
                let hit_upper = highs[j] >= upper;
                let hit_lower = lows[j] <= lower;
                if hit_upper && hit_lower {
                    // Both touched on one bar: the farther-from-close
                    // extreme wins (unobservable order inside the bar).
                    if highs[j] - closes[j] > closes[j] - lows[j] {
                        return Some(1.0);
                    }
                    return Some(-1.0);
                }
                if hit_upper {
                    return Some(1.0);
                }
                if hit_lower {
                    return Some(-1.0);
                }
            }
            // Time expired: sign of the horizon bar's drift. Ordered
            // comparisons (never `== 0.0`) keep the flat-expiry zero exact:
            // NaN is excluded first, so the final arm is only ±0.0 — and
            // Rust `signum` would misread +0.0 as +1.0.
            let drift = closes[i + window - 1] / entry - 1.0;
            if drift.is_nan() {
                None
            } else if drift > 0.0 {
                Some(1.0)
            } else if drift < 0.0 {
                Some(-1.0)
            } else {
                Some(0.0)
            }
        })
        .collect()
}

/// Latest RSRS timing score over replayed bars at the upstream lookbacks
/// (`n = 18`, `m = 600`): the last finite score, or `None` when the history
/// is shorter than the scoring window. Called once per backtest over the
/// whole series — never per bar.
#[must_use]
pub fn report_rsrs(bars: &[Bar]) -> Option<f64> {
    use rust_decimal::prelude::ToPrimitive as _;
    let highs: Vec<f64> = bars
        .iter()
        .map(|bar| bar.h.to_f64().unwrap_or(f64::NAN))
        .collect();
    let lows: Vec<f64> = bars
        .iter()
        .map(|bar| bar.l.to_f64().unwrap_or(f64::NAN))
        .collect();
    protopyne_kernels::research::rsrs(&highs, &lows, 18, 600)
        .into_iter()
        .flatten()
        .next_back()
}

/// Split history into a head (`train`) and tail (`test`) at `train_len`.
///
/// Walk-forward splits never overlap: the test segment starts exactly where
/// training ends, so no bar is evaluated twice.
pub fn walk_forward_split(bars: &[Bar], train_len: usize) -> Result<(&[Bar], &[Bar]), ServerError> {
    if train_len == 0 || train_len >= bars.len() {
        return Err(ServerError::Backtest(
            "train split must be within (0, len)".to_owned(),
        ));
    }
    Ok((&bars[..train_len], &bars[train_len..]))
}

/// Positions snapshot helper kept beside the runner for report attribution.
#[must_use]
pub fn snapshot_positions<V: Venue>(venue: &V) -> Vec<Position> {
    venue.positions()
}

/// Corporate-event helper kept beside the runner so expiry handling stays on
/// the venue trait, never on ad-hoc position edits.
pub fn expire_position<V: Venue>(
    venue: &V,
    underlying: &str,
    now: DateTime<Utc>,
) -> Result<String, protopyne_venue::VenueError> {
    venue.apply_corporate_event(underlying, CorporateEvent::Expiry, now)
}

#[cfg(test)]
#[path = "../tests/runner.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/multi_symbol_tape.rs"]
mod multi_symbol_tape_tests;
