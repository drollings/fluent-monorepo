//! Arming grants and their pure evaluator.
//!
//! A [`Grant`] is an immutable, journaled authorization envelope: scope,
//! budgets, three tiers of thresholds, and circuit breakers. Arming is a
//! deliberate act; a live grant can be tightened or revoked, never loosened
//! (loosening is a new grant). [`evaluate_grant`] is a pure function over its
//! inputs so backtests preview exactly what live trading would decide.

use bon::Builder;
use chrono::{DateTime, Utc};
use fluent_wvr::{Describable, FieldAccess};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use protopyne_types::Money;

use crate::error::StrategyError;
use crate::proposal::{Direction, Proposal, ProposalSide, StructureKind};

/// Boundary-string encoding shared by every composite patch leaf below: a
/// tier edits as one JSON document through `FieldAccess::set_field` (Tier 3
/// reflection over a Tier 2 body), so the struct definition stays the single
/// source of truth and no parallel leaf parser exists anywhere. Display
/// renders the same document; serialization of these plain structs cannot
/// fail, and the fallback keeps the trading path panic-free if it ever did.
macro_rules! json_boundary {
    ($ty:ty, $what:literal) => {
        impl FromStr for $ty {
            type Err = String;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                serde_json::from_str(s).map_err(|e| format!("invalid {} document: {e}", $what))
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&serde_json::to_string(self).unwrap_or_default())
            }
        }
    };
}

/// Entry/exit/roll permissions inside a grant's scope. Exit-only operation
/// is the low-risk default: entries need an explicit opt-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, FieldAccess, Describable)]
pub struct GrantPermission {
    /// Whether new exposure may be opened.
    #[field(desc = "Whether new exposure may be opened (tightening clears it)")]
    pub entries: bool,
    /// Whether existing exposure may be closed.
    #[field(desc = "Whether existing exposure may be closed (tightening clears it)")]
    pub exits: bool,
    /// Whether positions may be rolled as single multi-leg orders.
    #[field(desc = "Whether positions may roll as single multi-leg orders (tightening clears it)")]
    pub rolls: bool,
}

json_boundary!(GrantPermission, "grant permissions");

/// What a grant covers: which strategies, underlyings, structures, and
/// directions may trade, and with which lifecycle permissions. Empty lists
/// match nothing: a grant must name its scope explicitly to authorize.
///
/// The list fields stay out of the field descriptor (`#[field(skip)]`): a
/// `Vec` has no boundary-string reading and the orphan rule forbids lending
/// it one, so no list encoding is declared until a second consumer needs it.
/// Scope narrowing stays enforced core-side by [`Grant::tighten`].
#[derive(
    Debug, Clone, Builder, PartialEq, Eq, Serialize, Deserialize, FieldAccess, Describable,
)]
#[builder(start_fn = new)]
pub struct GrantScope {
    /// Strategy names covered by this grant.
    #[field(skip)]
    pub strategies: Vec<String>,
    /// Underlying symbols covered by this grant.
    #[field(skip)]
    pub underlyings: Vec<String>,
    /// Structures covered by this grant.
    #[field(skip)]
    pub structures: Vec<StructureKind>,
    /// Directions covered by this grant.
    #[field(skip)]
    pub directions: Vec<Direction>,
    /// Lifecycle permissions.
    #[field(
        desc = "Lifecycle permissions as a JSON document (tightening clears entries, exits, rolls)"
    )]
    pub permissions: GrantPermission,
}

impl GrantScope {
    /// Whether `proposal` falls inside this scope, including its side
    /// permission (entry, exit, and roll are separate permissions).
    #[must_use]
    pub fn allows(&self, proposal: &Proposal) -> bool {
        if !self.strategies.contains(&proposal.strategy) {
            return false;
        }
        if !self.underlyings.contains(&proposal.underlying) {
            return false;
        }
        if !self.structures.contains(&proposal.structure) {
            return false;
        }
        if !self.directions.contains(&proposal.direction) {
            return false;
        }
        match proposal.side {
            ProposalSide::Entry => self.permissions.entries,
            ProposalSide::Exit => self.permissions.exits,
            ProposalSide::Roll => self.permissions.rolls,
        }
    }

    /// Whether `self` covers a subset of `other` (used to prove a
    /// tightening never widens scope).
    #[must_use]
    pub fn is_subset_of(&self, other: &Self) -> bool {
        self.strategies.iter().all(|s| other.strategies.contains(s))
            && self
                .underlyings
                .iter()
                .all(|u| other.underlyings.contains(u))
            && self.structures.iter().all(|s| other.structures.contains(s))
            && self.directions.iter().all(|d| other.directions.contains(d))
            && (!self.permissions.entries || other.permissions.entries)
            && (!self.permissions.exits || other.permissions.exits)
            && (!self.permissions.rolls || other.permissions.rolls)
    }
}

/// Signal-tier thresholds: is the idea worth acting on?
/// Axis: **(task-value)** — merit of the trade, never producer health.
#[derive(
    Debug, Clone, Copy, Builder, PartialEq, Serialize, Deserialize, FieldAccess, Describable,
)]
#[builder(start_fn = new)]
pub struct SignalThresholds {
    /// Minimum signal conviction in `[0, 1]` **(task-value)**.
    #[field(desc = "Minimum signal conviction in [0, 1] (tightening raises it)")]
    pub min_conviction: f64,
    /// Minimum options-activity score in `[0, 1]` **(task-value)**.
    #[field(desc = "Minimum options-activity score in [0, 1] (tightening raises it)")]
    pub min_activity_score: f64,
}

json_boundary!(SignalThresholds, "signal thresholds");

/// Trade-tier thresholds: is this concrete order acceptable?
/// Axis: **(task-value)**, except the quote gate which is **(correctness)**.
#[derive(
    Debug, Clone, Copy, Builder, PartialEq, Serialize, Deserialize, FieldAccess, Describable,
)]
#[builder(start_fn = new)]
pub struct TradeThresholds {
    /// Maximum loss any single trade may commit **(task-value)**.
    #[field(
        desc = "Maximum loss one trade may commit, decimal major units (tightening lowers it)",
        format = "decimal"
    )]
    pub max_loss_per_trade: Money,
    /// Maximum bid/ask spread width the fill model tolerates **(task-value)**.
    #[field(
        desc = "Maximum bid/ask spread width, decimal major units (tightening lowers it)",
        format = "decimal"
    )]
    pub max_spread_width: Money,
    /// Minimum days to expiry **(task-value)**.
    #[field(desc = "Minimum days to expiry (tightening raises it)")]
    pub min_dte_days: u32,
    /// Maximum days to expiry **(task-value)**.
    #[field(desc = "Maximum days to expiry (tightening lowers it)")]
    pub max_dte_days: u32,
}

json_boundary!(TradeThresholds, "trade thresholds");

/// Portfolio/market-tier thresholds: is the book and the market fit to trade?
/// Open-risk caps are **(task-value)**; the quote-age gate is
/// **(correctness)**.
#[derive(
    Debug, Clone, Copy, Builder, PartialEq, Serialize, Deserialize, FieldAccess, Describable,
)]
#[builder(start_fn = new)]
pub struct PortfolioThresholds {
    /// Maximum aggregate committed max loss across open risk
    /// **(task-value)**.
    #[field(
        desc = "Maximum aggregate committed max loss, decimal major units (tightening lowers it)",
        format = "decimal"
    )]
    pub max_open_risk: Money,
    /// Maximum quote age in seconds before data counts as stale
    /// **(correctness)**.
    #[field(desc = "Maximum quote age in seconds (tightening lowers it)")]
    pub quote_max_age_secs: u64,
}

json_boundary!(PortfolioThresholds, "portfolio thresholds");

/// Hard budgets reserved at submission and consumed on fill.
/// Axis: **(task-value)** — every budget bounds worst-case exposure, which
/// the arming screen shows before confirmation.
#[derive(
    Debug, Clone, Copy, Builder, PartialEq, Serialize, Deserialize, FieldAccess, Describable,
)]
#[builder(start_fn = new)]
pub struct GrantBudgets {
    /// Maximum number of trades under this grant **(task-value)**.
    #[field(desc = "Maximum trades under this grant (tightening lowers it)")]
    pub max_trades: u32,
    /// Maximum contracts (or shares) under this grant **(task-value)**.
    #[field(desc = "Maximum contracts under this grant (tightening lowers it)")]
    pub max_contracts: u32,
    /// Maximum committed max loss under this grant **(task-value)**.
    #[field(
        desc = "Maximum committed max loss, decimal major units (tightening lowers it)",
        format = "decimal"
    )]
    pub max_loss: Money,
    /// Maximum committed notional under this grant **(task-value)**.
    #[field(
        desc = "Maximum committed notional, decimal major units (tightening lowers it)",
        format = "decimal"
    )]
    pub max_notional: Money,
}

json_boundary!(GrantBudgets, "grant budgets");

/// Budget already consumed. Kept separate from the immutable [`Grant`] so
/// the evaluator stays a pure function of `(proposal, grant, usage, ...)`.
#[derive(Debug, Clone, Copy, Builder, PartialEq, Eq, Serialize, Deserialize)]
#[builder(start_fn = new)]
pub struct GrantUsage {
    /// Trades already submitted.
    #[builder(default)]
    pub used_trades: u32,
    /// Contracts already submitted.
    #[builder(default)]
    pub used_contracts: u32,
    /// Max loss already committed.
    #[builder(default = Money::zero())]
    pub used_loss: Money,
    /// Notional already committed.
    #[builder(default = Money::zero())]
    pub used_notional: Money,
}

/// Circuit-breaker trip limits. Axis: **(producer-confidence)** — these
/// measure doubt in the producer's health, never the trade's merit, and must
/// never approve or veto a trade on merit.
#[derive(
    Debug, Clone, Copy, Builder, PartialEq, Eq, Serialize, Deserialize, FieldAccess, Describable,
)]
#[builder(start_fn = new)]
pub struct BreakerLimits {
    /// Consecutive venue errors that disarm entries **(producer-confidence)**.
    #[field(desc = "Consecutive venue errors that disarm entries (tightening lowers it)")]
    pub max_consecutive_errors: u32,
}

json_boundary!(BreakerLimits, "breaker limits");

/// Live breaker state, tracked outside the immutable grant.
#[derive(Debug, Clone, Builder, PartialEq, Eq, Serialize, Deserialize)]
#[builder(start_fn = new)]
pub struct BreakerState {
    /// Consecutive venue errors observed so far.
    #[builder(default)]
    pub consecutive_errors: u32,
    /// Whether a breaker has tripped and entries are disarmed.
    #[builder(default)]
    pub tripped: bool,
    /// Why the breaker tripped, if it did.
    pub trip_reason: Option<String>,
}

/// Fresh market snapshot the evaluator checks the proposal against. The
/// evaluator runs at decision time and again immediately before submission
/// against a fresh quote: callers pass the newest snapshot they have.
#[derive(Debug, Clone, Copy, Builder, PartialEq, Eq, Serialize, Deserialize)]
#[builder(start_fn = new)]
pub struct GrantMarket {
    /// Age of the quote in seconds **(correctness)**.
    pub quote_age_secs: u64,
    /// Current bid/ask spread width **(task-value)**.
    pub spread_width: Money,
}

/// An immutable arming grant: scope, budgets, three threshold tiers, and
/// breaker limits over a bounded time window.
#[derive(Debug, Clone, Builder, PartialEq, Serialize, Deserialize)]
#[builder(start_fn = new)]
pub struct Grant {
    /// Stable grant identifier.
    pub id: String,
    /// What the grant covers.
    pub scope: GrantScope,
    /// Hard budgets **(task-value)**.
    pub budgets: GrantBudgets,
    /// Signal-tier thresholds **(task-value)**.
    pub signal: SignalThresholds,
    /// Trade-tier thresholds **(task-value**, quote handling as marked).
    pub trade: TradeThresholds,
    /// Portfolio/market-tier thresholds (axis marked per field).
    pub portfolio: PortfolioThresholds,
    /// Breaker limits **(producer-confidence)**.
    pub breakers: BreakerLimits,
    /// Window start (inclusive).
    pub valid_from: DateTime<Utc>,
    /// Window end (exclusive).
    pub valid_until: DateTime<Utc>,
    /// Revoked grants authorize nothing, whenever they expire.
    #[builder(default)]
    pub revoked: bool,
}

impl protopyne_types::Validate for Grant {
    type Err = String;
    fn validate(&self) -> Result<(), Self::Err> {
        Grant::validate(self)
    }
}

impl Grant {
    /// Fail-closed structural check: live window, nonzero budgets, and a
    /// fully named scope. Shared by arming paths; the HTTP route delegates
    /// here so the rule lives in exactly one place.
    pub fn validate(&self) -> Result<(), String> {
        if self.revoked {
            return Err("grant is revoked".to_owned());
        }
        if self.valid_from >= self.valid_until {
            return Err("grant window is empty".to_owned());
        }
        if self.budgets.max_trades < 1 || self.budgets.max_contracts < 1 {
            return Err("grant budgets must authorize at least one trade".to_owned());
        }
        if self.scope.strategies.is_empty()
            || self.scope.underlyings.is_empty()
            || self.scope.structures.is_empty()
            || self.scope.directions.is_empty()
        {
            return Err(
                "grant scope must name strategies, underlyings, structures, and directions"
                    .to_owned(),
            );
        }
        Ok(())
    }

    /// Whether the grant authorizes anything at `now`.
    #[must_use]
    pub fn is_live(&self, now: DateTime<Utc>) -> bool {
        !self.revoked && self.valid_from <= now && now < self.valid_until
    }

    /// Whether `self` authorizes no more than `current` in every tier: the
    /// whole-grant counterpart of [`Grant::tighten`], enforcing the same rule
    /// set (budgets, signal, trade, portfolio, breaker tiers, scope subset,
    /// and window) so arming paths delegate instead of reimplementing it.
    /// Identity and revocation are not tiers and are not compared; a later
    /// start is a shorter window, hence narrower.
    #[must_use]
    pub fn tightens(&self, current: &Self) -> bool {
        if self.budgets.max_trades > current.budgets.max_trades
            || self.budgets.max_contracts > current.budgets.max_contracts
            || self.budgets.max_loss.as_decimal() > current.budgets.max_loss.as_decimal()
            || self.budgets.max_notional.as_decimal() > current.budgets.max_notional.as_decimal()
        {
            return false;
        }
        if self.signal.min_conviction < current.signal.min_conviction
            || self.signal.min_activity_score < current.signal.min_activity_score
        {
            return false;
        }
        if self.trade.max_loss_per_trade.as_decimal()
            > current.trade.max_loss_per_trade.as_decimal()
            || self.trade.max_spread_width.as_decimal()
                > current.trade.max_spread_width.as_decimal()
            || self.trade.min_dte_days < current.trade.min_dte_days
            || self.trade.max_dte_days > current.trade.max_dte_days
        {
            return false;
        }
        if self.portfolio.max_open_risk.as_decimal() > current.portfolio.max_open_risk.as_decimal()
            || self.portfolio.quote_max_age_secs > current.portfolio.quote_max_age_secs
        {
            return false;
        }
        if self.breakers.max_consecutive_errors > current.breakers.max_consecutive_errors {
            return false;
        }
        if !self.scope.is_subset_of(&current.scope) {
            return false;
        }
        if self.valid_until > current.valid_until || self.valid_from < current.valid_from {
            return false;
        }
        true
    }

    /// Revoke immediately. Returns the revoked copy; the original is
    /// unchanged and revocation is instant for every check after the swap.
    #[must_use]
    pub fn revoked_copy(&self) -> Self {
        let mut next = self.clone();
        next.revoked = true;
        next
    }

    /// Tighten this grant. Every supplied patch field must move toward more
    /// restrictive (budgets down, minimums up, windows shorter, scope
    /// narrower); anything looser is refused and loosening needs a new grant.
    pub fn tighten(&self, patch: GrantPatch) -> Result<Self, StrategyError> {
        let mut next = self.clone();
        if let Some(budgets) = patch.budgets {
            if budgets.max_trades > self.budgets.max_trades
                || budgets.max_contracts > self.budgets.max_contracts
                || budgets.max_loss.as_decimal() > self.budgets.max_loss.as_decimal()
                || budgets.max_notional.as_decimal() > self.budgets.max_notional.as_decimal()
            {
                return Err(StrategyError::InvalidGrant(
                    "tightening must not raise any budget".to_owned(),
                ));
            }
            next.budgets = budgets;
        }
        if let Some(signal) = patch.signal {
            if signal.min_conviction < self.signal.min_conviction
                || signal.min_activity_score < self.signal.min_activity_score
            {
                return Err(StrategyError::InvalidGrant(
                    "tightening must not lower any signal threshold".to_owned(),
                ));
            }
            next.signal = signal;
        }
        if let Some(trade) = patch.trade {
            if trade.max_loss_per_trade.as_decimal() > self.trade.max_loss_per_trade.as_decimal()
                || trade.max_spread_width.as_decimal() > self.trade.max_spread_width.as_decimal()
                || trade.min_dte_days < self.trade.min_dte_days
                || trade.max_dte_days > self.trade.max_dte_days
            {
                return Err(StrategyError::InvalidGrant(
                    "tightening must not widen any trade threshold".to_owned(),
                ));
            }
            next.trade = trade;
        }
        if let Some(portfolio) = patch.portfolio {
            if portfolio.max_open_risk.as_decimal() > self.portfolio.max_open_risk.as_decimal()
                || portfolio.quote_max_age_secs > self.portfolio.quote_max_age_secs
            {
                return Err(StrategyError::InvalidGrant(
                    "tightening must not widen any portfolio threshold".to_owned(),
                ));
            }
            next.portfolio = portfolio;
        }
        if let Some(breakers) = patch.breakers {
            if breakers.max_consecutive_errors > self.breakers.max_consecutive_errors {
                return Err(StrategyError::InvalidGrant(
                    "tightening must not raise any breaker limit".to_owned(),
                ));
            }
            next.breakers = breakers;
        }
        if let Some(scope) = patch.scope {
            if !scope.is_subset_of(&self.scope) {
                return Err(StrategyError::InvalidGrant(
                    "tightening must not widen grant scope".to_owned(),
                ));
            }
            next.scope = scope;
        }
        if let Some(valid_until) = patch.valid_until {
            if valid_until > self.valid_until {
                return Err(StrategyError::InvalidGrant(
                    "tightening must not extend the grant window".to_owned(),
                ));
            }
            next.valid_until = valid_until;
        }
        Ok(next)
    }
}

/// Optional per-tier replacements for [`Grant::tighten`]. `None` keeps the
/// current value; `Some` must move toward more restrictive.
///
/// The field descriptor drives the arming form: every settable tier edits
/// leaf by leaf (see [`Describable`] below), so the form renders from the
/// schema with no hand-written field list. `scope` stays out of the
/// descriptor: the generated `get_field` moves an `Option` out of the
/// borrow, which only compiles for `Copy` tiers, and the scope lists are
/// skipped anyway (see [`GrantScope`]) — scope narrowing stays enforced
/// core-side by [`Grant::tighten`], never through the form.
///
/// Whole-tier boundary strings stay the server path: [`FieldAccess`]
/// `set_field` parses one tier document through its `FromStr` codec, so a
/// patch arriving as tier texts needs no second parser.
#[derive(Debug, Clone, Default, Builder, PartialEq, Serialize, Deserialize, FieldAccess)]
#[builder(start_fn = new)]
pub struct GrantPatch {
    /// Replacement budgets (all caps must not rise).
    #[field(desc = "Replacement budgets, one value per leaf (caps must not rise)")]
    pub budgets: Option<GrantBudgets>,
    /// Replacement signal thresholds (minimums must not fall).
    #[field(desc = "Replacement signal thresholds, one value per leaf (minimums must not fall)")]
    pub signal: Option<SignalThresholds>,
    /// Replacement trade thresholds (caps must not widen).
    #[field(desc = "Replacement trade thresholds, one value per leaf (caps must not widen)")]
    pub trade: Option<TradeThresholds>,
    /// Replacement portfolio thresholds (caps must not widen).
    #[field(desc = "Replacement portfolio thresholds, one value per leaf (caps must not widen)")]
    pub portfolio: Option<PortfolioThresholds>,
    /// Replacement breaker limits (caps must not rise).
    #[field(desc = "Replacement breaker limits, one value per leaf (caps must not rise)")]
    pub breakers: Option<BreakerLimits>,
    /// Replacement scope (must be a subset).
    #[field(skip)]
    pub scope: Option<GrantScope>,
    /// Replacement window end (must not move later).
    #[field(
        desc = "Replacement window end, RFC 3339 (must not move later)",
        format = "date-time"
    )]
    pub valid_until: Option<DateTime<Utc>>,
}

/// Nested descriptor for the arming form: every settable tier carries its
/// own leaf schema, so each leaf edits through a typed editor (integer,
/// decimal text, date-time text) instead of one freeform document box per
/// tier. Hand-written by tier rather than derived flat, because the derive
/// spells every tier `type: string` (whole-document boundary strings) and
/// the form needs the leaf types; the agreement test pins the top-level
/// keys and every nested leaf set against the derived `field_names`, so a
/// new tier or leaf fails loudly here instead of vanishing from the form.
/// Leaf schemas come from each tier type's own `describe` (value-free —
/// the zero instances below only satisfy the `&self` receiver), so a leaf
/// description or format stays owned by its tier struct. `scope` stays out,
/// like in `field_names`.
impl Describable for GrantPatch {
    fn describe(&self) -> serde_json::Value {
        let zero = Money::zero();
        let leaf_schemas = [
            (
                "budgets",
                "Replacement budgets, one value per leaf (caps must not rise)",
                GrantBudgets::new()
                    .max_trades(0)
                    .max_contracts(0)
                    .max_loss(zero)
                    .max_notional(zero)
                    .build()
                    .describe(),
            ),
            (
                "signal",
                "Replacement signal thresholds, one value per leaf (minimums must not fall)",
                SignalThresholds::new()
                    .min_conviction(0.0)
                    .min_activity_score(0.0)
                    .build()
                    .describe(),
            ),
            (
                "trade",
                "Replacement trade thresholds, one value per leaf (caps must not widen)",
                TradeThresholds::new()
                    .max_loss_per_trade(zero)
                    .max_spread_width(zero)
                    .min_dte_days(0)
                    .max_dte_days(0)
                    .build()
                    .describe(),
            ),
            (
                "portfolio",
                "Replacement portfolio thresholds, one value per leaf (caps must not widen)",
                PortfolioThresholds::new()
                    .max_open_risk(zero)
                    .quote_max_age_secs(0)
                    .build()
                    .describe(),
            ),
            (
                "breakers",
                "Replacement breaker limits, one value per leaf (caps must not rise)",
                BreakerLimits::new()
                    .max_consecutive_errors(0)
                    .build()
                    .describe(),
            ),
        ];
        let mut properties = serde_json::Map::new();
        for (tier, description, schema) in leaf_schemas {
            let leaves = schema
                .get("properties")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            properties.insert(
                tier.to_owned(),
                serde_json::json!({
                    "type": "object",
                    "description": description,
                    "properties": leaves,
                }),
            );
        }
        properties.insert(
            "valid_until".to_owned(),
            serde_json::json!({
                "type": "string",
                "format": "date-time",
                "description": "Replacement window end, RFC 3339 (must not move later)",
            }),
        );
        serde_json::json!({ "type": "object", "properties": properties, "required": [] })
    }
}

/// Outcome of [`evaluate_grant`]: unconditional allow, or deny with every
/// reason collected (never just the first) for audit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantDecision {
    /// The grant covers this proposal.
    Allow,
    /// The grant refuses this proposal, with all reasons.
    Deny {
        /// Every failed check, in evaluation order.
        reasons: Vec<String>,
    },
}

impl GrantDecision {
    /// Whether the proposal is covered.
    #[must_use]
    pub fn is_allow(&self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// Compare one committed total against its cap, denying on arithmetic
/// overflow: an unrepresentable sum must refuse, never wrap or saturate into
/// an authorization.
fn budget_exceeded(
    used: Decimal,
    commit: Decimal,
    cap: Decimal,
    reasons: &mut Vec<String>,
    exhausted: &str,
) {
    match used.checked_add(commit) {
        Some(total) => {
            if total > cap {
                reasons.push(exhausted.to_owned());
            }
        }
        None => reasons.push(format!("{exhausted} (budget arithmetic overflow)")),
    }
}

/// Decide whether `grant` covers `proposal`.
///
/// Pure function: same inputs always produce the identical decision, with no
/// I/O, clock reads, or hidden state. Backtest preview, paper trading, and
/// (later) live submission all call this one function — never a second
/// preview-only copy.
///
/// Check order: liveness, scope, budgets, signal tier, trade tier (including
/// the defined-risk rule), portfolio/market tier, breakers. All failures are
/// collected so a denial explains itself fully.
///
/// Scope appears twice by design, with distinct meanings: the `scope.allows`
/// check is the whole-scope gate (one reason covering strategy, underlying,
/// structure, direction, and permission together), while the trade-tier
/// structure checks name the structure specifically — the defined-risk
/// exclusion bans naked shorts regardless of scope, and the
/// structure-in-scope check attributes threshold denials to the structure
/// for audit.
#[must_use]
pub fn evaluate_grant(
    proposal: &Proposal,
    grant: &Grant,
    usage: &GrantUsage,
    market: &GrantMarket,
    breakers: &BreakerState,
    now: DateTime<Utc>,
) -> GrantDecision {
    let mut reasons: Vec<String> = Vec::new();

    if grant.revoked {
        reasons.push("grant revoked".to_owned());
    }
    if now < grant.valid_from || now >= grant.valid_until {
        reasons.push("grant window not live".to_owned());
    }
    if !grant.scope.allows(proposal) {
        reasons.push("proposal outside grant scope".to_owned());
    }

    if usage.used_trades.saturating_add(1) > grant.budgets.max_trades {
        reasons.push("trade budget exhausted".to_owned());
    }
    match proposal.contracts() {
        Some(contracts) => {
            if usage.used_contracts.saturating_add(contracts) > grant.budgets.max_contracts {
                reasons.push("contract budget exhausted".to_owned());
            }
        }
        None => reasons.push("unrepresentable contract quantity".to_owned()),
    }
    budget_exceeded(
        usage.used_loss.as_decimal(),
        proposal.max_loss.as_decimal(),
        grant.budgets.max_loss.as_decimal(),
        &mut reasons,
        "max-loss budget exhausted",
    );
    budget_exceeded(
        usage.used_notional.as_decimal(),
        proposal.notional.as_decimal(),
        grant.budgets.max_notional.as_decimal(),
        &mut reasons,
        "notional budget exhausted",
    );

    // NaN scores compare as unordered and deny: a missing score is never
    // treated as meeting a threshold (fail closed).
    let conviction_ok = matches!(
        proposal
            .conviction
            .partial_cmp(&grant.signal.min_conviction),
        Some(Ordering::Greater | Ordering::Equal)
    );
    if !conviction_ok {
        reasons.push("conviction below signal threshold".to_owned());
    }
    let activity_ok = matches!(
        proposal
            .activity_score
            .partial_cmp(&grant.signal.min_activity_score),
        Some(Ordering::Greater | Ordering::Equal)
    );
    if !activity_ok {
        reasons.push("activity score below signal threshold".to_owned());
    }

    if proposal.max_loss.as_decimal() > grant.trade.max_loss_per_trade.as_decimal() {
        reasons.push("per-trade max loss exceeded".to_owned());
    }
    if market.spread_width.as_decimal() > grant.trade.max_spread_width.as_decimal() {
        reasons.push("spread width above trade threshold".to_owned());
    }
    if proposal.dte_days < grant.trade.min_dte_days || proposal.dte_days > grant.trade.max_dte_days
    {
        reasons.push("DTE outside trade window".to_owned());
    }
    if !proposal.structure.is_defined_risk() {
        reasons.push("naked short structures excluded".to_owned());
    }
    if !grant.scope.structures.contains(&proposal.structure) {
        reasons.push("structure outside grant scope".to_owned());
    }

    budget_exceeded(
        usage.used_loss.as_decimal(),
        proposal.max_loss.as_decimal(),
        grant.portfolio.max_open_risk.as_decimal(),
        &mut reasons,
        "portfolio open-risk cap exceeded",
    );
    if market.quote_age_secs > grant.portfolio.quote_max_age_secs {
        reasons.push("quote stale for grant evaluation".to_owned());
    }

    if breakers.tripped {
        reasons.push("breaker tripped: entries disarmed".to_owned());
    }
    if breakers.consecutive_errors > grant.breakers.max_consecutive_errors {
        reasons.push("consecutive-error breaker tripped".to_owned());
    }

    if reasons.is_empty() {
        GrantDecision::Allow
    } else {
        GrantDecision::Deny { reasons }
    }
}

#[cfg(test)]
#[path = "../tests/grant.rs"]
mod tests;
