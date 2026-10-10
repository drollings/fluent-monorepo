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

use common_core::registry::ConcurrentRegistry;
use fluent_wvr::prelude::*;
use serde::{Deserialize, Serialize};

use protopyne_types::{LegSide, Money, SpreadLeg, SpreadLegs};

use crate::error::VenueError;
use crate::journal::{JournalEvent, LEG_FILL_KIND};
use crate::order::{OrderIntent, OrderRecord, OrderSide};
use crate::paper::PaperVenue;
use crate::position::{PositionSide, SpreadPosition};
use crate::venue::{LegExecution, Venue};

/// Named portfolios over the shared keyed registry: the single home that
/// maps a portfolio name to its strategies and accounts.
pub struct PortfolioRegistry {
    inner: ConcurrentRegistry<String, protopyne_types::Portfolio>,
}

impl PortfolioRegistry {
    /// Open an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: ConcurrentRegistry::new(),
        }
    }

    /// Register a portfolio under its own name, replacing any prior entry.
    pub fn register(
        &self,
        portfolio: protopyne_types::Portfolio,
    ) -> Option<Arc<protopyne_types::Portfolio>> {
        self.inner.insert(portfolio.name.clone(), portfolio)
    }

    /// Look up a portfolio by name. Unknown names are absence, never error:
    /// commands refuse unknown portfolios fail-closed at execution.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<protopyne_types::Portfolio>> {
        self.inner.get(&name.to_owned())
    }

    /// Registered portfolio names.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.inner.keys()
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

/// One command-plane directive. `CloseAll` flattens every open group in the
/// venue book (one atomic intent per leg-group, never leg-by-leg);
/// `CancelAll` lists active orders for cancellation; `Disarm` transitions
/// the shared state to [`TradingState::Disabled`].
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
                VenueError::Invalid(format!("close group {index} on {}: {reason}", group.underlying))
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
            let legs = executions.remove(&prefix).expect("sorted prefixes come from the map");
            let structure = legs
                .iter()
                .find_map(|(_, position)| position.legs.clone());
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

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        match &self.command {
            CommandKind::Disarm => {
                self.state.set(TradingState::Disabled);
                Ok(WorkOutput::ok("disarmed: entries halted, exits still route"))
            }
            CommandKind::CloseAll { portfolio } => {
                self.require_portfolio(portfolio)?;
                let groups = self.open_groups();
                let intents = close_all_intents(&groups, self.limit, &self.proposal_id, &self.strategy)
                    .map_err(|e| WorkError::Execution(e.to_string()))?;
                WorkOutput::typed("close-all intents emitted", &intents)
            }
            CommandKind::CancelAll { portfolio } => {
                self.require_portfolio(portfolio)?;
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
    /// by construction.
    pub fn apply_one(view: &mut PortfolioView, event: &JournalEvent) {
        view.last_seq = view.last_seq.max(event.seq);
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
