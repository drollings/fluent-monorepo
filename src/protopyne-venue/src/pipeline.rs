//! NOT-A-PIPELINE (M16): full-record gate, not `fluent_wvr::Pipeline`
//! (fail-fast). First refusal sets `allowed`/`axis` while later stages still
//! run; mapping onto `Pipeline::run` would drop the split.
//!
//! One gate order for every enabled order path.
//!
//! [`DecisionPipeline`] composes the existing gates in the documented order —
//! correctness, then producer-confidence, then task-value — and returns a
//! single [`PipelineVerdict`] that names the axis that refused. It reuses
//! every gate type as it stands ([`GateReport`]/`HealthVerdict`,
//! [`ReconcileHandle`], [`BackfillHandle`], `TradingState`, `RiskEngine`,
//! `GrantLedger`) and implements no gate of its own: the composition is the
//! deliverable, so two callers cannot enforce two different orders.
//!
//! The live session's submission route calls [`DecisionPipeline::route`] (the
//! producer-confidence stages) so its halt-entries/route-exits shape lives in
//! one place; the full [`DecisionPipeline::evaluate`] is the order a per-bar
//! decision runs, correctness through task-value.

use chrono::{DateTime, Utc};

use protopyne_strategy::{
    DrawdownStatus, GrantDecision, GrantLedger, GrantMarket, OrderShape, Proposal, Reservation,
    RiskApproved, RiskEngine, RiskReason, RiskState,
};

use crate::backfill::BackfillHandle;
use crate::command::StateHandle;
use crate::error::VenueError;
use crate::health::{check_route, HealthLimits, HealthState, RouteIntent};
use crate::reconcile::ReconcileHandle;

/// Which of the three gate axes refused. A refusal names exactly the axis
/// that fired first; an allowed verdict names none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateAxis {
    /// The action is structurally ill-defined: no whole quantity, or legs
    /// that fail their own defined-risk validation.
    Correctness,
    /// The inputs may be wrong right now: a stale or absent reading, an
    /// operator halt, a mismatched book, an unspliced hole, or a tripped
    /// breaker.
    ProducerConfidence,
    /// The trade is valid but unwanted: risk caps or grant budgets and
    /// thresholds.
    TaskValue,
}

/// One decision's verdict. `axis` names the refusing axis (`None` when
/// allowed); `reasons` carries every refusal string on that axis. The split
/// below records every stage even when an earlier one refused, so a caller
/// can populate its decision record without a second evaluation: a
/// producer-confidence trip never rewrites the trade's merit, and a
/// correctness refusal never defaults the task-value recording.
pub struct PipelineVerdict<'a> {
    /// Whether the order may proceed.
    pub allowed: bool,
    /// The axis that refused first, or `None` when allowed.
    pub axis: Option<GateAxis>,
    /// Every refusal reason on the failing axis, in evaluation order.
    pub reasons: Vec<String>,
    /// Whether the risk engine approved.
    pub risk_pass: bool,
    /// Risk refusal reasons (empty on approval).
    pub risk_reasons: Vec<String>,
    /// The risk engine's approval token, when the task-value stage ran.
    pub approval: Option<RiskApproved>,
    /// Whether the grant covers the proposal on pure merit (breaker input
    /// excluded): the pre-authorization evaluation.
    pub grant_merit: Option<GrantDecision>,
    /// Grant merit-denial reasons (empty when merit allows).
    pub grant_merit_reasons: Vec<String>,
    /// Whether the grant authorized submission: merit, the ledger's live
    /// breaker state, and a successful budget reservation.
    pub grant_authorized: bool,
    /// Authorization-denial reasons (empty when authorized).
    pub grant_authorization_reasons: Vec<String>,
    /// The grant's budget reservation, when authorization succeeded.
    pub reservation: Option<Reservation<'a>>,
}

impl<'a> PipelineVerdict<'a> {
    /// Assemble the verdict from the first refusal (if any) and the fully
    /// recorded split. `allowed` follows the first refusal alone; the split
    /// is recorded regardless.
    fn assemble(
        refusal: Option<(GateAxis, Vec<String>)>,
        risk_pass: bool,
        risk_reasons: Vec<String>,
        approval: Option<RiskApproved>,
        merit: GrantDecision,
        merit_reasons: Vec<String>,
        authorized: bool,
        authorization_reasons: Vec<String>,
        reservation: Option<Reservation<'a>>,
    ) -> Self {
        let (allowed, axis, reasons) = match refusal {
            None => (true, None, Vec::new()),
            Some((axis, reasons)) => (false, Some(axis), reasons),
        };
        Self {
            allowed,
            axis,
            reasons,
            risk_pass,
            risk_reasons,
            approval,
            grant_merit: Some(merit),
            grant_merit_reasons: merit_reasons,
            grant_authorized: authorized,
            grant_authorization_reasons: authorization_reasons,
            reservation,
        }
    }
}

impl std::fmt::Debug for PipelineVerdict<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `Reservation` holds a ledger borrow and has no `Debug`; report only
        // whether one was taken.
        f.debug_struct("PipelineVerdict")
            .field("allowed", &self.allowed)
            .field("axis", &self.axis)
            .field("reasons", &self.reasons)
            .field("risk_pass", &self.risk_pass)
            .field("approval", &self.approval)
            .field("reserved", &self.reservation.is_some())
            .field("grant_merit", &self.grant_merit)
            .field("grant_authorized", &self.grant_authorized)
            .finish()
    }
}

/// One decision's inputs for the full gate order: the proposal and the order
/// shape it will submit, the observed facts the risk and grant gates read, and
/// the engine/ledger that mint the token and the reservation. The verdict
/// borrows only the ledger (through the reservation); every other input is
/// read during evaluation, so the caller keeps mutating its proposal.
pub struct PipelineRequest<'p, 'l> {
    /// The proposal under evaluation.
    pub proposal: &'p Proposal,
    /// The shape of the order that will actually be submitted.
    pub shape: OrderShape,
    /// Whether this is an exit (exits route past producer-confidence gates).
    pub is_exit: bool,
    /// Observed producer health, when the caller has a reading. `None` skips
    /// the health gate; the caller owns that declaration.
    pub health: Option<(&'p HealthState, &'p HealthLimits)>,
    /// Observed venue facts for the risk gate.
    pub risk_state: &'p RiskState,
    /// Market facts for the grant gate.
    pub market: &'p GrantMarket,
    /// Decision time.
    pub now: DateTime<Utc>,
    /// The mandatory, non-bypassable risk engine.
    pub risk: &'l RiskEngine,
    /// The grant ledger: merit, live breaker state, and budget reservation.
    pub ledger: &'l GrantLedger,
}

/// The composed gate order. Holds the session-scoped producer-confidence
/// gates; the per-decision inputs arrive on each [`evaluate`](Self::evaluate)
/// call.
#[derive(Debug, Clone)]
pub struct DecisionPipeline {
    trading: StateHandle,
    reconcile: Option<ReconcileHandle>,
    backfill: Option<BackfillHandle>,
}

impl DecisionPipeline {
    /// A pipeline over `trading`, with no reconcile or backfill gate attached
    /// (the default-off wiring a session boots with).
    #[must_use]
    pub fn new(trading: StateHandle) -> Self {
        Self {
            trading,
            reconcile: None,
            backfill: None,
        }
    }

    /// Attach the reconciliation entry gate.
    #[must_use]
    pub fn with_reconcile(mut self, gate: ReconcileHandle) -> Self {
        self.reconcile = Some(gate);
        self
    }

    /// Attach the backfill entry gate.
    #[must_use]
    pub fn with_backfill(mut self, gate: BackfillHandle) -> Self {
        self.backfill = Some(gate);
        self
    }

    /// The producer-confidence route the session enforces on submission:
    /// trading state, then the health reading (when supplied), then the
    /// reconciliation and backfill gates — every one skipped for an exit.
    /// Each gate's own refusal is preserved, so the variant a caller saw
    /// before the pipeline existed is the variant it still sees.
    pub fn route(&self, is_exit: bool) -> Result<(), VenueError> {
        self.confidence(is_exit, None).map_err(|(_, error)| error)
    }

    /// The full gate order: correctness → producer-confidence → task-value.
    /// Every stage is recorded even when an earlier one refused, so the
    /// verdict carries the complete split: the first refusal decides
    /// `allowed` and `axis`, while risk, merit, and authorization are
    /// evaluated unconditionally (a dropped reservation releases its hold,
    /// so recording never moves budget).
    pub fn evaluate<'l>(&self, request: &PipelineRequest<'_, 'l>) -> PipelineVerdict<'l> {
        // (1) Correctness: the action must be structurally well-defined.
        let mut refusal = if request.proposal.contracts().is_none() {
            Some((
                GateAxis::Correctness,
                vec!["proposal quantity is not a whole number of contracts".to_owned()],
            ))
        } else if let Some(legs) = &request.proposal.legs {
            match legs.validate() {
                Ok(()) => None,
                Err(error) => Some((GateAxis::Correctness, vec![error.to_string()])),
            }
        } else {
            None
        };
        // (2) Producer-confidence: the inputs may be wrong right now. Exits
        // route past every gate here.
        if let Err((axis, error)) = self.confidence(request.is_exit, request.health) {
            if refusal.is_none() {
                refusal = Some((axis, vec![error.to_string()]));
            }
        }
        // (3) Task-value: risk, then the grant's merit, then breaker
        // authorization, then the budget reservation. All recorded even
        // when an earlier gate refused.
        let (risk_pass, risk_reasons, approval) =
            match request
                .risk
                .check(request.proposal, request.shape, request.risk_state)
            {
                Ok(approved) => (true, Vec::new(), Some(approved)),
                Err(refused) => (
                    false,
                    refused
                        .reasons
                        .iter()
                        .map(|reason| format!("{reason:?}"))
                        .collect(),
                    None,
                ),
            };
        if !risk_pass && refusal.is_none() {
            refusal = Some((GateAxis::TaskValue, risk_reasons.clone()));
        }
        // Merit is the pure evaluation with breaker input excluded: a
        // producer trip never rewrites the trade's recorded merit.
        let merit = request
            .ledger
            .evaluate(request.proposal, request.market, request.now);
        let merit_reasons = grant_reasons(&merit);
        let merit_allow = merit.is_allow();
        if !merit_allow && refusal.is_none() {
            refusal = Some((GateAxis::TaskValue, merit_reasons.clone()));
        }
        // A producer trip disarms entries but is not a merit judgement, and
        // authorization additionally needs a successful budget reservation.
        // Only an authorized bar takes budget: the reservation is attempted
        // exactly when merit allows on a non-disarmed entry.
        let breakers = request.ledger.breakers();
        let max = request.ledger.grant().breakers.max_consecutive_errors;
        // The drawdown arm judges the live book on every evaluation. A breach
        // trips the ledger (sticky, reason banked once) and disarms; an
        // unobserved book disarms without latching, so the feed may recover.
        // Either way merit above stays recorded: the breaker doubts the
        // producer, never the trade.
        let drawdown = request.ledger.observe_drawdown(&request.risk_state.facts);
        let mut disarmed = breakers.tripped || breakers.consecutive_errors > max;
        let mut drawdown_trip: Option<String> = None;
        match drawdown {
            DrawdownStatus::Breached(fraction) => {
                disarmed = true;
                let cap = request
                    .ledger
                    .grant()
                    .breakers
                    .max_portfolio_drawdown_fraction
                    .expect("a breach implies a configured cap");
                drawdown_trip = Some(format!(
                    "breaker tripped: portfolio drawdown {fraction} past max {cap}"
                ));
            }
            DrawdownStatus::Unobserved => {
                disarmed = true;
                drawdown_trip = Some(format!(
                    "breaker tripped: {:?} — drawdown cap configured but equity unobserved",
                    RiskReason::PortfolioFactsUnavailable
                ));
            }
            DrawdownStatus::Inert | DrawdownStatus::Within(_) => {}
        }
        let mut authorization_reasons = merit_reasons.clone();
        let mut reservation = None;
        if merit_allow && (request.is_exit || !disarmed) {
            match request
                .ledger
                .reserve(request.proposal, request.market, request.now)
            {
                Ok(held) => reservation = Some(held),
                Err(decision) => {
                    let denied = grant_reasons(&decision);
                    authorization_reasons.extend(denied.clone());
                    if refusal.is_none() {
                        refusal = Some((GateAxis::TaskValue, denied));
                    }
                }
            }
        }
        if disarmed && !request.is_exit {
            // A fresh drawdown verdict names the trip; otherwise the error
            // streak does. Either is the breaker's own reason, never merit's.
            let trip = drawdown_trip.unwrap_or_else(|| {
                format!(
                    "breaker tripped: entries disarmed after {} consecutive venue errors (max {max})",
                    breakers.consecutive_errors,
                )
            });
            authorization_reasons.push(trip);
            if refusal.is_none() {
                refusal = Some((
                    GateAxis::ProducerConfidence,
                    vec![authorization_reasons
                        .last()
                        .expect("trip reason pushed")
                        .clone()],
                ));
            }
        }
        PipelineVerdict::assemble(
            refusal,
            risk_pass,
            risk_reasons,
            approval,
            merit,
            merit_reasons,
            reservation.is_some(),
            authorization_reasons,
            reservation,
        )
    }

    /// The producer-confidence stages, in the documented order: health, then
    /// trading state, then the reconciliation gate, then the backfill gate.
    /// Exits route past every one. On refusal, returns the failing axis and
    /// the gate's own error.
    fn confidence(
        &self,
        is_exit: bool,
        health: Option<(&HealthState, &HealthLimits)>,
    ) -> Result<(), (GateAxis, VenueError)> {
        if is_exit {
            return Ok(());
        }
        if let Some((state, limits)) = health {
            check_route(state, limits, RouteIntent::Entry)
                .map_err(|error| (GateAxis::ProducerConfidence, error))?;
        }
        self.trading
            .get()
            .check(false)
            .map_err(|error| (GateAxis::ProducerConfidence, error))?;
        if let Some(gate) = &self.reconcile {
            gate.check_entries()
                .map_err(|error| (GateAxis::ProducerConfidence, error))?;
        }
        if let Some(gate) = &self.backfill {
            gate.check_entries()
                .map_err(|error| (GateAxis::ProducerConfidence, error))?;
        }
        Ok(())
    }
}

/// Denial reasons from a grant decision, empty for an allow.
fn grant_reasons(decision: &GrantDecision) -> Vec<String> {
    match decision {
        GrantDecision::Deny { reasons } => reasons.clone(),
        GrantDecision::Allow => Vec::new(),
    }
}

#[cfg(test)]
#[path = "../tests/pipeline.rs"]
mod tests;
