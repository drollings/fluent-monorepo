//! Portfolio registry, command plane, and read-only replica.
//!
//! Three pieces with three owners: [`PortfolioRegistry`] names the groups
//! (tastytrade/paper/backtest/manual-ticket) over the shared keyed registry —
//! the only grouping authority, never a second instrument index.
//! [`CommandUnit`] carries [`CommandKind`] (`CloseAll`/`CancelAll`/`Disarm`)
//! as a supervised [`WorkUnit`](fluent_wvr::WorkUnit): commands run under
//! [`SupervisedBatch`](fluent_concurrency::batch::SupervisedBatch) so a
//! failing command cancels its dependents through the dependency graph, and
//! submission bounding stays on the session's existing credit pair (no new
//! pool here). [`StateReplica`] folds journaled lifecycle events into a
//! monitor view — read-only by construction, holding no order authority.
//!
//! Entry gating is deliberately narrower than the risk engine, never wider:
//! [`TradingState`] halts entries while exits always route (a halted book
//! must stay manageable). The risk `kill_switch` is untouched.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use common_core::registry::ConcurrentRegistry;
use fluent_wvr::prelude::*;
use serde::{Deserialize, Serialize};

use protopyne_strategy::{transition, JournaledTransition, Proposal, ProposalEvent, ProposalState};
use protopyne_types::{LegSide, Money, SpreadLeg, SpreadLegs, SymKey, SymbolIndex};

use crate::error::VenueError;
use crate::journal::{Journal, JournalEvent, VenueStorage, LEG_FILL_KIND};
use crate::order::{OrderIntent, OrderRecord, OrderSide};
use crate::paper::PaperVenue;
use crate::position::{PositionSide, SpreadPosition};
use crate::venue::{LegExecution, Venue};

/// Named portfolios over the shared keyed registry: the single home that
/// maps a portfolio name to its strategies and accounts. Names intern
/// through a registry-owned [`SymbolIndex`] into [`SymKey`] keys — the one
/// index in this system besides the venue books, and likewise never global.
/// The public surface still speaks names, so keys never leak into
/// serialization.
pub struct PortfolioRegistry {
    inner: ConcurrentRegistry<SymKey, protopyne_types::Portfolio>,
    symbols: Mutex<SymbolIndex>,
}

impl PortfolioRegistry {
    /// Open an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: ConcurrentRegistry::new(),
            symbols: Mutex::new(SymbolIndex::new()),
        }
    }

    /// Register a portfolio under its own name, replacing any prior entry.
    /// A full symbol table refuses the insert and reports no prior entry;
    /// the rehearsal universe never approaches capacity.
    pub fn register(
        &self,
        portfolio: protopyne_types::Portfolio,
    ) -> Option<Arc<protopyne_types::Portfolio>> {
        let Ok(key) = common_core::sync::lock(&self.symbols).intern(&portfolio.name) else {
            return None;
        };
        self.inner.insert(key, portfolio)
    }

    /// Look up a portfolio by name. Unknown names are absence, never error:
    /// commands refuse unknown portfolios fail-closed at execution. Reads
    /// resolve without inserting, so lookups never grow the table.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<protopyne_types::Portfolio>> {
        let key = common_core::sync::lock(&self.symbols).lookup(name)?;
        self.inner.get(&key)
    }

    /// Registered portfolio names, in no guaranteed order.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        let symbols = common_core::sync::lock(&self.symbols);
        self.inner
            .keys()
            .iter()
            .filter_map(|key| symbols.resolve(*key).map(str::to_owned))
            .collect()
    }

    /// Count of registered portfolios.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether any portfolio is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

impl Default for PortfolioRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether the platform may route orders right now. Narrower than the risk
/// engine, never wider: entries need [`TradingState::Enabled`], exits always
/// route so a halted book stays manageable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TradingState {
    /// New entries and exits both route.
    Enabled,
    /// Nothing routes; the fail-closed rest state.
    #[default]
    Disabled,
    /// Only exits route.
    ExitOnly,
}

impl TradingState {
    /// Whether an order may route: exits always route, entries need
    /// [`TradingState::Enabled`].
    #[must_use]
    pub fn allows(&self, is_exit: bool) -> bool {
        is_exit || matches!(self, Self::Enabled)
    }

    /// Fail-closed enforcement twin of `health :: check_trade`: refusals
    /// carry the halting state as the reason. Operator intent is an
    /// authority signal **(correctness)**-shaped — refuse with reason, never
    /// degrade — and never a merit judgment on the trade.
    pub fn check(&self, is_exit: bool) -> Result<(), VenueError> {
        if self.allows(is_exit) {
            Ok(())
        } else {
            Err(VenueError::Refused(format!(
                "trading halted ({self:?}): entries refused, exits still route"
            )))
        }
    }
}

/// Shared handle to the trading state: commands transition it, gates read
/// it. Locking never crosses an await (all transitions are synchronous), so
/// a recovering mutex is the honest primitive, never a wedged gate.
#[derive(Debug, Clone, Default)]
pub struct StateHandle {
    inner: Arc<Mutex<TradingState>>,
}

impl StateHandle {
    /// Share ownership of `state`.
    #[must_use]
    pub fn new(state: TradingState) -> Self {
        Self {
            inner: Arc::new(Mutex::new(state)),
        }
    }

    /// Read the current state.
    #[must_use]
    pub fn get(&self) -> TradingState {
        *common_core::sync::lock(&self.inner)
    }

    /// Transition to `state`. Only ever narrows under command; no path here
    /// widens authority on its own.
    pub fn set(&self, state: TradingState) {
        *common_core::sync::lock(&self.inner) = state;
    }
}

/// Client ID every journaled operator command carries: commands are
/// authority rows, not orders, so they share one synthetic id while their
/// kinds (`command.*`) keep them distinct from lifecycle rows.
pub const OPERATOR_CLIENT_ID: &str = "operator";

/// Journaled kind for a halt-entries command.
pub const COMMAND_DISARM_KIND: &str = "command.disarm";
/// Journaled kind for a resume-entries command.
pub const COMMAND_RESUME_KIND: &str = "command.resume";
/// Journaled kind for a close-all audit row.
pub const COMMAND_CLOSE_ALL_KIND: &str = "command.close_all";
/// Journaled kind for a cancel-all audit row.
pub const COMMAND_CANCEL_ALL_KIND: &str = "command.cancel_all";

/// Rebuild the trading state from journaled command rows: last command
/// wins; disarm halts, resume restores; close/cancel rows are audit-only.
/// No rows means no operator ever acted, so the fold starts
/// [`TradingState::Enabled`] — wiring is a no-op until the first command is
/// journaled, and a restart rebuilds exactly the state the operator left.
#[must_use]
pub fn replay_trading_state(events: &[JournalEvent]) -> TradingState {
    let mut state = TradingState::Enabled;
    for event in events {
        match event.kind.as_str() {
            COMMAND_DISARM_KIND => state = TradingState::Disabled,
            COMMAND_RESUME_KIND => state = TradingState::Enabled,
            _ => {}
        }
    }
    state
}

/// Journaled kind for a proposal lifecycle row. The single home: the run
/// path journals the same kind with the same [`JournaledTransition`] detail
/// shape, so approval rows and run rows fold through one projection.
pub const PROPOSAL_TRANSITION_KIND: &str = "proposal_transition";

/// Record a human decision on a proposal: validate the move against the
/// lifecycle first, journal the transition, then advance the proposal. A
/// refused move journals nothing and mutates nothing; only `Approve` and
/// `Reject` travel this path, so the journal never confuses who authorized
/// what — machine events stay on the run path.
pub fn decide_proposal(
    journal: &Journal,
    permit: &VenueStorage,
    proposal: &mut Proposal,
    event: ProposalEvent,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<JournaledTransition, VenueError> {
    if !matches!(event, ProposalEvent::Approve | ProposalEvent::Reject) {
        return Err(VenueError::Invalid(format!(
            "decision plane carries Approve/Reject, not {event:?}"
        )));
    }
    let to = transition(proposal.state, event)
        .map_err(|e| VenueError::Invalid(format!("proposal lifecycle refused: {e}")))?;
    let entry = JournaledTransition {
        proposal_id: proposal.id.clone(),
        from: proposal.state,
        to,
        reason: reason.to_owned(),
        at: now,
    };
    let detail = serde_json::to_string(&entry).map_err(|e| VenueError::Journal(e.to_string()))?;
    journal.record_transition(permit, &proposal.id, PROPOSAL_TRANSITION_KIND, &detail, now)?;
    proposal
        .apply(event, reason, now)
        .map_err(|e| VenueError::Invalid(format!("proposal lifecycle refused: {e}")))?;
    Ok(entry)
}

/// Project journaled lifecycle rows into per-proposal states: last row per
/// proposal wins; unparseable rows and every other kind never project. The
/// inbox reads this fold, so the served states are the journaled ones.
#[must_use]
pub fn replay_proposal_states(
    events: &[JournalEvent],
) -> std::collections::BTreeMap<String, ProposalState> {
    let mut states = std::collections::BTreeMap::new();
    for event in events {
        if event.kind != PROPOSAL_TRANSITION_KIND {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<JournaledTransition>(&event.detail) else {
            continue;
        };
        states.insert(entry.proposal_id, entry.to);
    }
    states
}

/// Asset the trading-state sensor provides: bar units carrying entries
/// declare it in `depends`; exit and monitor units stay independent so a
/// halted book stays manageable.
pub const TRADING_ENABLED_ASSET: &str = "trading.enabled";

/// One command-plane directive. `CloseAll` flattens every open group in the
/// venue book (one atomic intent per leg-group, never leg-by-leg);
/// `CancelAll` lists active orders for cancellation; `Disarm` transitions
/// the shared state to [`TradingState::Disabled`]; `Resume` transitions it
/// back to [`TradingState::Enabled`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandKind {
    /// Flatten every open group, scoped to a registered portfolio name.
    CloseAll {
        /// Portfolio the flattened book belongs to.
        portfolio: String,
    },
    /// List active orders for cancellation, scoped to a portfolio name.
    CancelAll {
        /// Portfolio the orders belong to.
        portfolio: String,
    },
    /// Halt entries now; exits keep routing.
    Disarm,
    /// Route entries again after a halt.
    Resume,
}

/// One closable unit: an exposure side, an absolute quantity, and the
/// structure that must travel with it (`None` for single-leg books).
#[derive(Debug, Clone)]
pub struct LegGroup {
    /// Underlying the group tracks.
    pub underlying: String,
    /// Side of the open exposure (long closes by selling).
    pub side: PositionSide,
    /// Absolute contracts to close.
    pub quantity: u32,
    /// Structure traveling on the closing intent, if any.
    pub legs: Option<SpreadLegs>,
}

/// Emit one atomic closing intent per leg-group: the intent side flips the
/// exposure, and spread legs travel with flipped sides on the same intent —
/// never as one intent per leg. Deterministic client IDs (`close-{index}-…
/// …-{underlying}`) keep retries lookups, never second orders.
///
/// Refuses (fail-closed) when a group would close into undefined risk — a
/// lone long leg flips into a naked short — or carries zero quantity.
pub fn close_all_intents(
    groups: &[LegGroup],
    limit: Money,
    proposal_id: &str,
    strategy: &str,
) -> Result<Vec<OrderIntent>, VenueError> {
    let mut intents = Vec::with_capacity(groups.len());
    for (index, group) in groups.iter().enumerate() {
        if group.quantity == 0 {
            return Err(VenueError::Invalid(format!(
                "close group {index} on {} carries zero quantity",
                group.underlying
            )));
        }
        let legs = group
            .legs
            .as_ref()
            .map(flip_legs)
            .transpose()
            .map_err(|reason| {
                VenueError::Invalid(format!(
                    "close group {index} on {}: {reason}",
                    group.underlying
                ))
            })?;
        let side = match group.side {
            PositionSide::Long => OrderSide::Sell,
            PositionSide::Short => OrderSide::Buy,
        };
        intents.push(
            OrderIntent::new()
                .client_id(format!("close-{index}-{}", group.underlying))
                .proposal_id(proposal_id.to_owned())
                .strategy(strategy.to_owned())
                .underlying(group.underlying.clone())
                .side(side)
                .quantity(group.quantity)
                .limit_price(limit)
                .maybe_legs(legs)
                .build(),
        );
    }
    Ok(intents)
}

/// Flip every leg side for closing: longs sell, shorts buy. The flipped
/// structure re-validates because closing must never invent undefined risk.
fn flip_legs(legs: &SpreadLegs) -> Result<SpreadLegs, String> {
    let flipped = legs
        .legs()
        .iter()
        .map(|leg| {
            SpreadLeg::new()
                .contract(leg.contract.clone())
                .side(match leg.side {
                    LegSide::Long => LegSide::Short,
                    LegSide::Short => LegSide::Long,
                })
                .ratio(leg.ratio)
                .build()
        })
        .collect();
    SpreadLegs::try_new(flipped).map_err(|e| e.to_string())
}

/// Client IDs of orders still working: active only, so cancel sweeps never
/// chase terminal orders.
#[must_use]
pub fn cancel_all_ids(orders: &[OrderRecord]) -> Vec<String> {
    orders
        .iter()
        .filter(|order| order.status.is_active())
        .map(|order| order.intent.client_id.clone())
        .collect()
}

/// A command-plane directive as a supervised unit: reads venue books and the
/// portfolio registry synchronously (no hidden I/O, no awaits), transitions
/// shared state or emits intent data, and reports unknown portfolios as
/// execution failures so supervision cancels dependents fail-closed.
#[derive(bon::Builder)]
#[builder(start_fn = new)]
pub struct CommandUnit {
    /// Command name (also the supervision identity).
    pub name: String,
    /// Dependency assets: a failing producer cancels this unit through the
    /// batch graph.
    #[builder(default)]
    pub depends: Vec<ArcIntern<str>>,
    /// Assets this unit provides to downstream dependents.
    #[builder(default)]
    pub provides: Vec<ArcIntern<str>>,
    /// Directive this unit carries.
    pub command: CommandKind,
    /// Venue books the unit reads.
    pub venue: Arc<PaperVenue>,
    /// Shared trading state the unit transitions.
    pub state: StateHandle,
    /// Portfolio registry the unit validates scope against.
    pub registry: Arc<PortfolioRegistry>,
    /// Limit marking emitted close intents (default zero: callers price
    /// from quotes; an unpriced close refuses downstream, never misprices).
    #[builder(default = Money::zero())]
    pub limit: Money,
    /// Proposal ID stamped on emitted close intents.
    #[builder(default = "close".to_owned())]
    pub proposal_id: String,
    /// Strategy stamped on emitted close intents.
    #[builder(default = "command".to_owned())]
    pub strategy: String,
    /// Journal the command's audit row through before applying anything.
    /// `None` keeps the legacy direct apply (existing callers, unit probes);
    /// `Some` makes journal-first structural — no row, no state change.
    /// (Optional fields default to `None`; no attribute needed.)
    pub journal: Option<Arc<Journal>>,
}

impl CommandUnit {
    fn require_portfolio(&self, portfolio: &str) -> Result<(), WorkError> {
        if self.registry.get(portfolio).is_none() {
            return Err(WorkError::Execution(format!(
                "unknown portfolio {portfolio}: refusing to act on an ungrouped book"
            )));
        }
        Ok(())
    }

    /// Read the venue books as closable groups: one single-leg group per
    /// nonzero net position, one spread group per execution prefix (legs of
    /// one fill share a client-ID prefix and therefore close as one
    /// package). Prototype grouping, documented as such: mixed-ratio books
    /// close the minimum whole-unit count, and exact ties on dominant side
    /// break toward the first leg in OCC order.
    fn open_groups(&self) -> Vec<LegGroup> {
        let mut groups = Vec::new();
        let mut singles: Vec<_> = self.venue.positions();
        singles.sort_by(|left, right| left.underlying.cmp(&right.underlying));
        for position in singles {
            // Prototype positions are `u32`-born (fills carry `u32`
            // quantities); anything wider cannot be expressed downstream, so
            // it is skipped rather than clamped into a wrong close.
            let Ok(magnitude) = u32::try_from(position.quantity.unsigned_abs()) else {
                continue;
            };
            if magnitude == 0 {
                continue;
            }
            groups.push(LegGroup {
                underlying: position.underlying,
                side: if position.quantity > 0 {
                    PositionSide::Long
                } else {
                    PositionSide::Short
                },
                quantity: magnitude,
                legs: None,
            });
        }
        let spread = self.venue.spread_positions();
        let mut executions: HashMap<String, Vec<(String, SpreadPosition)>> = HashMap::new();
        let mut keys: Vec<&String> = spread.keys().collect();
        keys.sort();
        for key in keys {
            let position = spread.get(key).expect("sorted keys come from the map");
            let prefix = position
                .trade_ids
                .first()
                .and_then(|id| id.split('#').next())
                .unwrap_or(key)
                .to_owned();
            executions
                .entry(prefix)
                .or_default()
                .push((key.clone(), position.clone()));
        }
        let mut prefixes: Vec<String> = executions.keys().cloned().collect();
        prefixes.sort();
        for prefix in prefixes {
            let legs = executions
                .remove(&prefix)
                .expect("sorted prefixes come from the map");
            let structure = legs.iter().find_map(|(_, position)| position.legs.clone());
            let dominant = legs
                .iter()
                .max_by_key(|(_, position)| position.quantity_abs)
                .expect("execution groups are nonempty");
            let units = legs
                .iter()
                .map(|(_, position)| position.quantity_abs)
                .min()
                .expect("execution groups are nonempty");
            if units == 0 {
                continue;
            }
            groups.push(LegGroup {
                underlying: dominant.1.underlying.clone(),
                side: dominant.1.side,
                quantity: units,
                legs: structure,
            });
        }
        groups
    }
}

impl CommandUnit {
    /// Journal-first gate: when bound to a journal, record the command's
    /// audit row through the [`VenueStorage`] permit from the batch
    /// capability set before anything mutates. A missing permit or a failed
    /// write refuses the command — state changes only behind a durable row.
    fn journal_command(
        &self,
        ctx: &WorkContext,
        kind: &str,
        detail: &str,
    ) -> Result<(), WorkError> {
        let Some(journal) = &self.journal else {
            return Ok(());
        };
        let Some(permit) = ctx.caps.get::<VenueStorage>().cloned() else {
            return Err(WorkError::Execution(
                "command journal bound but venue-storage capability missing: refusing to mutate unjournaled"
                    .to_owned(),
            ));
        };
        journal
            .record_transition(&permit, OPERATOR_CLIENT_ID, kind, detail, Utc::now())
            .map_err(|e| WorkError::Execution(e.to_string()))
    }
}

impl WorkUnit for CommandUnit {
    fn name(&self) -> &str {
        &self.name
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &self.depends
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &self.provides
    }

    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        match &self.command {
            CommandKind::Disarm => {
                self.journal_command(ctx, COMMAND_DISARM_KIND, "{}")?;
                self.state.set(TradingState::Disabled);
                Ok(WorkOutput::ok(
                    "disarmed: entries halted, exits still route",
                ))
            }
            CommandKind::Resume => {
                self.journal_command(ctx, COMMAND_RESUME_KIND, "{}")?;
                self.state.set(TradingState::Enabled);
                Ok(WorkOutput::ok("resumed: entries route again"))
            }
            CommandKind::CloseAll { portfolio } => {
                self.require_portfolio(portfolio)?;
                self.journal_command(
                    ctx,
                    COMMAND_CLOSE_ALL_KIND,
                    &serde_json::json!({ "portfolio": portfolio }).to_string(),
                )?;
                let groups = self.open_groups();
                let intents =
                    close_all_intents(&groups, self.limit, &self.proposal_id, &self.strategy)
                        .map_err(|e| WorkError::Execution(e.to_string()))?;
                WorkOutput::typed("close-all intents emitted", &intents)
            }
            CommandKind::CancelAll { portfolio } => {
                self.require_portfolio(portfolio)?;
                self.journal_command(
                    ctx,
                    COMMAND_CANCEL_ALL_KIND,
                    &serde_json::json!({ "portfolio": portfolio }).to_string(),
                )?;
                let ids = cancel_all_ids(&self.venue.orders());
                WorkOutput::typed("cancel-all order IDs listed", &ids)
            }
        }
    }
}

impl_fieldless!(CommandUnit);

impl Describable for CommandUnit {
    fn describe(&self) -> serde_json::Value {
        fluent_wvr::describe_work_unit(
            &self.name,
            &self.depends,
            &self.provides,
            Some("effect: reads venue books; disarms shared state or emits intent data"),
        )
    }
}

impl_component!(CommandUnit);

/// Supervision sensor for bar batches: reports the shared trading state
/// into a [`SupervisedBatch`] as pass/fail. Failing cancels entry-carrying
/// bar units through the graph (they declare [`TRADING_ENABLED_ASSET`] in
/// `depends`); exit and monitor units stay independent and still complete.
/// The unit is synchronous and side-effect free (one lock read), honoring
/// the `execute` purity contract.
///
/// [`SupervisedBatch`]: fluent_concurrency::batch::SupervisedBatch
pub struct TradingStateUnit {
    state: StateHandle,
    provides: Vec<ArcIntern<str>>,
}

impl TradingStateUnit {
    /// Build the session's sensor over `state`. The unit name is fixed
    /// (`trading.state.sensor`) so bars cannot register it twice by
    /// accident; entry units key on the [`TRADING_ENABLED_ASSET`] asset.
    #[must_use]
    pub fn new(state: StateHandle) -> Self {
        Self {
            state,
            provides: vec![ArcIntern::from(TRADING_ENABLED_ASSET)],
        }
    }
}

impl WorkUnit for TradingStateUnit {
    fn name(&self) -> &str {
        "trading.state.sensor"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &self.provides
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        match self.state.get().check(false) {
            Ok(()) => Ok(WorkOutput::ok("trading enabled")),
            Err(reason) => Err(WorkError::Execution(reason.to_string())),
        }
    }
}

impl_fieldless!(TradingStateUnit);

impl Describable for TradingStateUnit {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "description": "Trading-state supervision sensor: fails while entries are halted",
        })
    }
}

impl_component!(TradingStateUnit);

/// One journaled order as the replica sees it: client ID plus latest
/// lifecycle kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicaOrder {
    /// Client ID the lifecycle belongs to.
    pub client_id: String,
    /// Latest journaled kind for this client (`intent`, `filled`, …).
    pub status: String,
}

/// Read-only monitor view over journaled lifecycle state. This is the
/// terminal data contract for rendering — it carries no order authority,
/// and it is distinct from the server preview aggregate that shares the
/// shape's name in another crate.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortfolioView {
    /// Latest lifecycle kind per client, in first-seen order.
    pub orders: Vec<ReplicaOrder>,
    /// Replayed spread-leg fills, in journal order.
    pub leg_fills: Vec<LegExecution>,
    /// Sequence numbers of leg rows that would not parse: surfaced, never
    /// silently skipped.
    pub unparsed_leg_seqs: Vec<i64>,
    /// Highest journal sequence folded in.
    pub last_seq: i64,
}

/// Pure fold from journal events to the monitor view. Same events, same
/// view — replay and the live fold cannot disagree.
pub struct StateReplica;

impl StateReplica {
    /// Fold a whole event slice into a view.
    #[must_use]
    pub fn apply(events: &[JournalEvent]) -> PortfolioView {
        let mut view = PortfolioView::default();
        for event in events {
            Self::apply_one(&mut view, event);
        }
        view
    }

    /// Fold one event into a view. Single-event and whole-slice folds agree
    /// by construction. Operator command rows (`command.*`) carry authority
    /// transitions, not orders; proposal lifecycle rows carry human or run
    /// decisions on proposals; manual-ticket rows carry the human-executed
    /// ticket workflow: all three advance the tip but never read as
    /// phantom orders.
    pub fn apply_one(view: &mut PortfolioView, event: &JournalEvent) {
        view.last_seq = view.last_seq.max(event.seq);
        if event.kind.starts_with("command.")
            || event.kind == PROPOSAL_TRANSITION_KIND
            || event.kind.starts_with("manual_ticket.")
        {
            return;
        }
        if event.kind == LEG_FILL_KIND {
            match serde_json::from_str::<LegExecution>(&event.detail) {
                Ok(leg) => view.leg_fills.push(leg),
                Err(_) => view.unparsed_leg_seqs.push(event.seq),
            }
            return;
        }
        if let Some(order) = view
            .orders
            .iter_mut()
            .find(|order| order.client_id == event.client_id)
        {
            order.status.clone_from(&event.kind);
        } else {
            view.orders.push(ReplicaOrder {
                client_id: event.client_id.clone(),
                status: event.kind.clone(),
            });
        }
    }
}

#[cfg(test)]
#[path = "../tests/command.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/command_plane.rs"]
mod command_plane_tests;
