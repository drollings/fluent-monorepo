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
use std::time::Duration;

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
    #[field(
        desc = "Minimum signal conviction in [0, 1] (tightening raises it)",
        axis = "task-value"
    )]
    pub min_conviction: f64,
    /// Minimum options-activity score in `[0, 1]` **(task-value)**.
    #[field(
        desc = "Minimum options-activity score in [0, 1] (tightening raises it)",
        axis = "task-value"
    )]
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
        format = "decimal",
        axis = "task-value"
    )]
    pub max_loss_per_trade: Money,
    /// Maximum bid/ask spread width the fill model tolerates **(task-value)**.
    #[field(
        desc = "Maximum bid/ask spread width, decimal major units (tightening lowers it)",
        format = "decimal",
        axis = "task-value"
    )]
    pub max_spread_width: Money,
    /// Minimum days to expiry **(task-value)**, judged against the proposal's
    /// own instrument.
    #[field(
        desc = "Minimum days to expiry (tightening raises it)",
        axis = "task-value"
    )]
    pub min_dte_days: u32,
    /// Maximum days to expiry **(task-value)**, judged against the
    /// proposal's own instrument.
    #[field(
        desc = "Maximum days to expiry (tightening lowers it)",
        axis = "task-value"
    )]
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
        format = "decimal",
        axis = "task-value"
    )]
    pub max_open_risk: Money,
    /// Maximum quote age in seconds before data counts as stale
    /// **(correctness)**.
    #[field(
        desc = "Maximum quote age in seconds (tightening lowers it)",
        axis = "correctness"
    )]
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
    #[field(
        desc = "Maximum trades under this grant (tightening lowers it)",
        axis = "task-value"
    )]
    pub max_trades: u32,
    /// Maximum contracts (or shares) under this grant **(task-value)**.
    #[field(
        desc = "Maximum contracts under this grant (tightening lowers it)",
        axis = "task-value"
    )]
    pub max_contracts: u32,
    /// Maximum committed max loss under this grant **(task-value)**.
    #[field(
        desc = "Maximum committed max loss, decimal major units (tightening lowers it)",
        format = "decimal",
        axis = "task-value"
    )]
    pub max_loss: Money,
    /// Maximum committed notional under this grant **(task-value)**.
    #[field(
        desc = "Maximum committed notional, decimal major units (tightening lowers it)",
        format = "decimal",
        axis = "task-value"
    )]
    pub max_notional: Money,
}

json_boundary!(GrantBudgets, "grant budgets");

/// Where a [`ComputedWorstCase`] number came from. Producer-confidence about
/// the number's provenance, never a verdict on exposure: the screen shows
/// [`BudgetEcho`](Self::BudgetEcho) as an upper bound, never as a computed
/// exposure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorstCaseBasis {
    /// Every permitted structure was priced from a chain: the numbers are
    /// computed exposures.
    ChainDerived,
    /// At least one permitted structure could not be priced: the numbers are
    /// the grant's own budget fields, an upper bound on exposure.
    BudgetEcho,
}

impl WorstCaseBasis {
    /// The stable wire tag the view layer carries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChainDerived => "ChainDerived",
            Self::BudgetEcho => "BudgetEcho",
        }
    }
}

/// Worst case for one package unit of a structure, priced from a chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorstCaseUnit {
    /// Worst-case loss for one package unit.
    pub max_loss: Money,
    /// Committed notional for one package unit.
    pub notional: Money,
}

/// Bounded worst-case exposure of an armed grant: at most the grant's budgets,
/// tightened by per-structure unit economics when a chain answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputedWorstCase {
    /// Package units (trades) the binding structure allows.
    pub max_trades: u32,
    /// Leg contracts in those units.
    pub max_contracts: u32,
    /// Worst-case committed loss.
    pub max_loss: Money,
    /// Worst-case committed notional.
    pub max_notional: Money,
    /// Provenance of the numbers.
    pub basis: WorstCaseBasis,
}

/// Bound an armed grant's exposure from per-structure unit economics.
///
/// For every permitted structure the caller prices, the implied package
/// units are `min(max_trades, max_contracts / legs_per_unit)`, and the worst
/// case is the largest `units × per-unit` over those structures. Both numbers
/// are capped by the grant's own loss and notional budgets — a budget is a
/// hard limit, so [`WorstCaseBasis::BudgetEcho`] is always an upper bound on
/// what [`WorstCaseBasis::ChainDerived`] can compute. When any permitted
/// structure cannot be priced (`per_unit` answers `None`) or the structure
/// list is empty, the numbers fall back to the budget fields.
#[must_use]
pub fn worst_case_for(
    grant: &Grant,
    structures: &[StructureKind],
    per_unit: impl Fn(StructureKind) -> Option<WorstCaseUnit>,
    legs_per_unit: impl Fn(StructureKind) -> u32,
) -> ComputedWorstCase {
    let budgets = grant.budgets;
    let mut worst: Option<(u32, u32, Money, Money)> = None;
    let mut every_priced = !structures.is_empty();
    for &structure in structures {
        let Some(unit) = per_unit(structure) else {
            every_priced = false;
            continue;
        };
        let legs = legs_per_unit(structure).max(1);
        let units = budgets.max_trades.min(budgets.max_contracts / legs);
        let candidate = (
            units,
            units * legs,
            Money::new(unit.max_loss.as_decimal() * Decimal::from(units)),
            Money::new(unit.notional.as_decimal() * Decimal::from(units)),
        );
        if worst.is_none_or(|best| candidate.2.as_decimal() > best.2.as_decimal()) {
            worst = Some(candidate);
        }
    }
    let Some((max_trades, max_contracts, loss, notional)) = worst.filter(|_| every_priced) else {
        return exercise_budget_echo(budgets);
    };
    ComputedWorstCase {
        max_trades,
        max_contracts,
        max_loss: cheaper(loss, budgets.max_loss),
        max_notional: cheaper(notional, budgets.max_notional),
        basis: WorstCaseBasis::ChainDerived,
    }
}

/// The budget fields as the worst case: an upper bound on exposure, never a
/// computed one.
fn exercise_budget_echo(budgets: GrantBudgets) -> ComputedWorstCase {
    ComputedWorstCase {
        max_trades: budgets.max_trades,
        max_contracts: budgets.max_contracts,
        max_loss: budgets.max_loss,
        max_notional: budgets.max_notional,
        basis: WorstCaseBasis::BudgetEcho,
    }
}

/// The smaller of two amounts: a budget is a hard ceiling.
/// (M3.2: delegates to [`Money::min`], the one home for this comparison.)
fn cheaper(left: Money, right: Money) -> Money {
    left.min(right)
}

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
    /// Last operator attestation, when the grant carries a re-attestation
    /// window. Initialised to the grant's `valid_from` on arming; journaled
    /// `Attest` rows move it forward (last wins, never backwards). Attesting
    /// consumes nothing — budgets do not move.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attested: Option<DateTime<Utc>>,
}

/// Re-attestation window: the operator's authority lapses on a cadence, so
/// a grant left unconfirmed stops opening new risk while positions stay
/// manageable. Additive and `None` by default: grants armed without one
/// behave exactly as before, forever.
#[derive(Debug, Clone, Copy, Builder, PartialEq, Serialize, Deserialize)]
#[builder(start_fn = new)]
pub struct ReAttestationWindow {
    /// How long one attestation stays authoritative.
    pub cadence: Duration,
    /// Extra time past cadence before entries disarm.
    pub grace: Duration,
}

/// Re-attestation window for a grant lifetime: longer windows get tighter
/// (or equal) windows — never the reverse. A policy table, not a fit: the
/// calibration suite asserts every window length maps exactly once and the
/// mapping never loosens with length.
///
/// | window length | re-attestation |
/// |---|---|
/// | ≤ 1 h | none — the window ends before any cadence could lapse |
/// | ≤ 24 h | none — a single session needs no re-attestation |
/// | ≤ 72 h | daily cadence, one hour of grace |
/// | > 72 h | daily cadence, no grace (re-confirm before each entry session) |
#[must_use]
pub fn re_attestation_for_window(window: Duration) -> Option<ReAttestationWindow> {
    if window <= Duration::from_hours(24) {
        None
    } else if window <= Duration::from_hours(72) {
        Some(ReAttestationWindow {
            cadence: Duration::from_hours(24),
            grace: Duration::from_hours(1),
        })
    } else {
        Some(ReAttestationWindow {
            cadence: Duration::from_hours(24),
            grace: Duration::ZERO,
        })
    }
}
/// Circuit-breaker trip limits. Axis: **(producer-confidence)** — these
/// measure doubt in the producer's health, never the trade's merit, and must
/// never approve or veto a trade on merit.
#[derive(
    Debug, Clone, Copy, Builder, PartialEq, Serialize, Deserialize, FieldAccess, Describable,
)]
#[builder(start_fn = new)]
pub struct BreakerLimits {
    /// Consecutive venue errors that disarm entries **(producer-confidence)**.
    #[field(
        desc = "Consecutive venue errors that disarm entries (tightening lowers it)",
        axis = "producer-confidence"
    )]
    pub max_consecutive_errors: u32,
    /// Peak-to-trough equity drawdown fraction that disarms entries
    /// **(producer-confidence)**. `None` leaves the arm inert, so existing
    /// runs are unaffected; `Some` must sit in `(0, 1]` (validated at
    /// arming). A breach trips exactly like the error streak — entries
    /// disarm, exits route, the reason banks once — while merit stays
    /// recorded. A configured cap with no equity reading refuses as
    /// unobserved, never passes.
    #[field(
        desc = "Peak-to-trough drawdown fraction that disarms entries (tightening lowers it)",
        axis = "producer-confidence"
    )]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_portfolio_drawdown_fraction: Option<f64>,
}

/// Whether a replacement drawdown cap is looser than the one it replaces: a
/// higher cap trips later, and dropping the cap removes the arm entirely.
/// `None`-to-`None` is equal, never looser.
fn drawdown_looser(next: Option<f64>, current: Option<f64>) -> bool {
    match (next, current) {
        (None, Some(_)) => true,
        (Some(next), Some(current)) => next > current,
        _ => false,
    }
}

/// Whether a replacement re-attestation window is looser than the one it
/// replaces: dropping the window removes the arm entirely, and a later trip
/// is looser. `None`-to-`None` is equal, never looser.
fn re_attestation_looser(
    next: Option<ReAttestationWindow>,
    current: Option<ReAttestationWindow>,
) -> bool {
    match (next, current) {
        (None, Some(_)) => true,
        (Some(next), Some(current)) => {
            next.cadence.saturating_add(next.grace) > current.cadence.saturating_add(current.grace)
        }
        _ => false,
    }
}

/// Documented default drawdown cap for operators arming the breaker.
///
/// | cap | posture |
/// |---|---|
/// | 0.05 | tight: a twentieth off the peak disarms entries |
/// | **0.10** | **default**: a tenth off the peak disarms entries; exits still route |
/// | 0.20 | loose: only a fifth-off-peak stops new risk |
///
/// The default is a starting point, not a fit: the calibration suite proves
/// a thousand rising curves stay within it and the boundary pins hold at any
/// configured value.
pub const DEFAULT_MAX_PORTFOLIO_DRAWDOWN_FRACTION: f64 = 0.10;

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
    /// Bid/ask spread width the trade would cross **(task-value)**: the
    /// package's own width on a leg-attached proposal, the underlying's bar
    /// spread where no package is priced.
    pub spread_width: Money,
    /// Observed open committed risk over the book, when the caller owns a
    /// reading **(task-value)**. `None` skips the open-risk cap: the caller
    /// declared the producer gates irrelevant at construction (rehearsal).
    /// A live caller supplies the session producer's reading — observed or
    /// not — and an unobserved book refuses through the risk gate's
    /// aggregate caps, never by inventing a zero here.
    #[serde(default)]
    pub open_risk: Option<Money>,
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
    /// Re-attestation window **(producer-confidence)**. `None` (the default)
    /// leaves attestation unenforced, so existing grants behave exactly as
    /// before; `Some` disarms entries past cadence + grace until re-attested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub re_attestation: Option<ReAttestationWindow>,
    /// Window start (inclusive).
    pub valid_from: DateTime<Utc>,
    /// Window end (exclusive).
    pub valid_until: DateTime<Utc>,
    /// Revoked grants authorize nothing, whenever they expire.
    #[builder(default)]
    pub revoked: bool,
}

impl fluent_wvr::validate::Validate for Grant {
    type Err = String;
    /// Fail-closed structural check: live window, nonzero budgets, and a
    /// fully named scope. A *revoked* grant is structurally sound and stays
    /// representable (serde, storage, folds); whether it may be armed is a
    /// separate arm-time rule in [`Grant::validate_armable`].
    fn validate(&self) -> Result<(), Self::Err> {
        if self.valid_from >= self.valid_until {
            return Err("grant window is empty".to_owned());
        }
        if let Some(cap) = self.breakers.max_portfolio_drawdown_fraction {
            if !(cap > 0.0 && cap <= 1.0 && cap.is_finite()) {
                return Err(format!(
                    "drawdown cap {cap} outside (0, 1]: it can never be a threshold"
                ));
            }
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
}

/// A named grant construction: the one place a preset grant is built.
/// Presets name their tier values once; every caller that arms the same
/// rehearsal surface shares this body instead of a second literal.
#[derive(Debug, Clone, PartialEq)]
pub enum GrantPreset {
    /// The permissive rehearsal surface: fixed strategies over one
    /// underlying, both directions, wide budgets, open thresholds, a long
    /// validity window. The scope's structure list always follows the run
    /// that requested it, so a backtest over one structure is never denied
    /// for naming another.
    Rehearsal {
        /// Stable grant identifier.
        id: String,
        /// Structures the run may name.
        structures: Vec<StructureKind>,
    },
}

/// Per-tier tightening predicates shared by [`Grant::tightens`] (whole-grant
/// compare) and [`Grant::tighten`] (patch compare): `next` authorizes no more
/// than `current` in that tier. One rule set, two call shapes — the patch
/// path wraps these in per-tier errors, the predicate path negates them
/// (M6.1). `re_attestation` and `valid_from` have no patch fields, so they
/// stay inline in `tightens` only.
fn budgets_tightens(next: &GrantBudgets, current: &GrantBudgets) -> bool {
    next.max_trades <= current.max_trades
        && next.max_contracts <= current.max_contracts
        && next.max_loss.as_decimal() <= current.max_loss.as_decimal()
        && next.max_notional.as_decimal() <= current.max_notional.as_decimal()
}

fn signal_tightens(next: &SignalThresholds, current: &SignalThresholds) -> bool {
    next.min_conviction >= current.min_conviction
        && next.min_activity_score >= current.min_activity_score
}

fn trade_tightens(next: &TradeThresholds, current: &TradeThresholds) -> bool {
    next.max_loss_per_trade.as_decimal() <= current.max_loss_per_trade.as_decimal()
        && next.max_spread_width.as_decimal() <= current.max_spread_width.as_decimal()
        && next.min_dte_days >= current.min_dte_days
        && next.max_dte_days <= current.max_dte_days
}

fn portfolio_tightens(next: &PortfolioThresholds, current: &PortfolioThresholds) -> bool {
    next.max_open_risk.as_decimal() <= current.max_open_risk.as_decimal()
        && next.quote_max_age_secs <= current.quote_max_age_secs
}

fn breakers_tightens(next: &BreakerLimits, current: &BreakerLimits) -> bool {
    next.max_consecutive_errors <= current.max_consecutive_errors
        && !drawdown_looser(
            next.max_portfolio_drawdown_fraction,
            current.max_portfolio_drawdown_fraction,
        )
}

fn scope_tightens(next: &GrantScope, current: &GrantScope) -> bool {
    next.is_subset_of(current)
}

fn window_end_tightens(
    next_valid_until: DateTime<Utc>,
    current_valid_until: DateTime<Utc>,
) -> bool {
    next_valid_until <= current_valid_until
}

impl Grant {
    /// Build a preset grant. The rehearsal preset is the only permissive
    /// construction; every other grant is armed explicitly.
    #[must_use]
    pub fn preset(preset: GrantPreset) -> Self {
        match preset {
            GrantPreset::Rehearsal { id, structures } => Grant::new()
                .id(id)
                .scope(
                    GrantScope::new()
                        .strategies(vec!["ribbon".to_owned()])
                        .underlyings(vec!["SPY".to_owned()])
                        .structures(structures)
                        .directions(vec![Direction::Long, Direction::Short])
                        .permissions(GrantPermission {
                            entries: true,
                            exits: true,
                            rolls: false,
                        })
                        .build(),
                )
                .budgets(
                    GrantBudgets::new()
                        .max_trades(1_000)
                        .max_contracts(10_000)
                        .max_loss(Money::from(1_000_000))
                        .max_notional(Money::from(100_000_000))
                        .build(),
                )
                .signal(
                    SignalThresholds::new()
                        .min_conviction(0.0)
                        .min_activity_score(0.0)
                        .build(),
                )
                .trade(
                    TradeThresholds::new()
                        .max_loss_per_trade(Money::from(1_000))
                        .max_spread_width(Money::from(50))
                        .min_dte_days(1)
                        .max_dte_days(90)
                        .build(),
                )
                .portfolio(
                    PortfolioThresholds::new()
                        .max_open_risk(Money::from(10_000_000))
                        .quote_max_age_secs(60)
                        .build(),
                )
                .breakers(BreakerLimits::new().max_consecutive_errors(3).build())
                .valid_from(
                    DateTime::from_timestamp(1_700_000_000, 0).expect("fixed stamp is valid"),
                )
                .valid_until(
                    DateTime::from_timestamp(1_900_000_000, 0).expect("fixed stamp is valid"),
                )
                .build(),
        }
    }

    /// Structural check plus the arm-time rule: a revoked grant authorizes
    /// nothing, so arming it is refused. Every arming path (the HTTP route and
    /// the journaled command plane) calls this instead of `validate`, keeping
    /// the refusal in exactly one place.
    pub fn validate_armable(&self) -> Result<(), String> {
        fluent_wvr::validate::Validate::validate(self)?;
        if self.revoked {
            return Err("grant is revoked".to_owned());
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
        if !budgets_tightens(&self.budgets, &current.budgets) {
            return false;
        }
        if !signal_tightens(&self.signal, &current.signal) {
            return false;
        }
        if !trade_tightens(&self.trade, &current.trade) {
            return false;
        }
        if !portfolio_tightens(&self.portfolio, &current.portfolio) {
            return false;
        }
        if !breakers_tightens(&self.breakers, &current.breakers) {
            return false;
        }
        // Dropping attestation is looser, tightening the window is tighter;
        // the cadence table only ever tightens with window length.
        // (`re_attestation` has no patch field, so it stays here, not in a
        // shared predicate.)
        if re_attestation_looser(self.re_attestation, current.re_attestation) {
            return false;
        }
        if !scope_tightens(&self.scope, &current.scope) {
            return false;
        }
        if !window_end_tightens(self.valid_until, current.valid_until)
            || self.valid_from < current.valid_from
        {
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
            if !budgets_tightens(&budgets, &self.budgets) {
                return Err(StrategyError::InvalidGrant(
                    "tightening must not raise any budget".to_owned(),
                ));
            }
            next.budgets = budgets;
        }
        if let Some(signal) = patch.signal {
            if !signal_tightens(&signal, &self.signal) {
                return Err(StrategyError::InvalidGrant(
                    "tightening must not lower any signal threshold".to_owned(),
                ));
            }
            next.signal = signal;
        }
        if let Some(trade) = patch.trade {
            if !trade_tightens(&trade, &self.trade) {
                return Err(StrategyError::InvalidGrant(
                    "tightening must not widen any trade threshold".to_owned(),
                ));
            }
            next.trade = trade;
        }
        if let Some(portfolio) = patch.portfolio {
            if !portfolio_tightens(&portfolio, &self.portfolio) {
                return Err(StrategyError::InvalidGrant(
                    "tightening must not widen any portfolio threshold".to_owned(),
                ));
            }
            next.portfolio = portfolio;
        }
        if let Some(breakers) = patch.breakers {
            if !breakers_tightens(&breakers, &self.breakers) {
                return Err(StrategyError::InvalidGrant(
                    "tightening must not raise any breaker limit".to_owned(),
                ));
            }
            next.breakers = breakers;
        }
        if let Some(scope) = patch.scope {
            if !scope_tightens(&scope, &self.scope) {
                return Err(StrategyError::InvalidGrant(
                    "tightening must not widen grant scope".to_owned(),
                ));
            }
            next.scope = scope;
        }
        if let Some(valid_until) = patch.valid_until {
            if !window_end_tightens(valid_until, self.valid_until) {
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

/// Single-sourced denial vocabulary (M6.1c): the fixed tier order
/// [`evaluate_grant`] walks, as constants. The evaluator and
/// [`deny_order`](crate::fixtures::deny_order) both read these, so a
/// renamed stage or a moved tier fails to compile instead of silently
/// forking the string pin. A full typed-refusal enum would change
/// [`GrantDecision`]'s wire shape (`reasons: Vec<String>` is read by the
/// server, ledger, journal, and UI) — deliberately not done here.
pub mod refusal {
    /// Grant is revoked.
    pub const REVOKED: &str = "grant revoked";
    /// Outside `[valid_from, valid_until)`.
    pub const WINDOW_NOT_LIVE: &str = "grant window not live";
    /// Entry past cadence + grace without re-attestation.
    pub const ATTESTATION_LAPSED: &str = "grant attestation lapsed";
    /// Whole-scope gate (strategy/underlying/structure/direction/permission).
    pub const OUTSIDE_SCOPE: &str = "proposal outside grant scope";
    /// Trade-count budget consumed.
    pub const TRADE_BUDGET_EXHAUSTED: &str = "trade budget exhausted";
    /// Conviction below the signal floor.
    pub const CONVICTION_BELOW_THRESHOLD: &str = "conviction below signal threshold";
    /// Per-trade max loss exceeded.
    pub const MAX_LOSS_EXCEEDED: &str = "per-trade max loss exceeded";
    /// Portfolio open-risk cap exceeded.
    pub const OPEN_RISK_EXCEEDED: &str = "portfolio open-risk cap exceeded";
    /// Breaker disarmed entries.
    pub const BREAKER_TRIPPED: &str = "breaker tripped: entries disarmed";
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
/// an authorization. (M6.2) This is the grant-budget comparator; the
/// mandatory-risk caps (`RiskEngine::check`, typed `RiskReason`) and the
/// sizing clamp (`SizingCaps::contain`, which clamps rather than refuses)
/// are different authorities over different refusal representations — keep
/// all three, do not abstract over them.
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
    evaluate_grant_inner(proposal, grant, usage, market, breakers, now, true)
}

/// Merit-only evaluation: the attestation arm is excluded, exactly as the
/// breaker input is excluded by passing clean state. The pipeline records
/// merit through this, so a lapsed cadence never rewrites the trade's merit
/// — authorization (reserve) always runs the full fixed order.
pub(crate) fn evaluate_grant_merit(
    proposal: &Proposal,
    grant: &Grant,
    usage: &GrantUsage,
    market: &GrantMarket,
    breakers: &BreakerState,
    now: DateTime<Utc>,
) -> GrantDecision {
    evaluate_grant_inner(proposal, grant, usage, market, breakers, now, false)
}

fn evaluate_grant_inner(
    proposal: &Proposal,
    grant: &Grant,
    usage: &GrantUsage,
    market: &GrantMarket,
    breakers: &BreakerState,
    now: DateTime<Utc>,
    enforce_attestation: bool,
) -> GrantDecision {
    use refusal as R;
    let mut reasons: Vec<String> = Vec::new();

    if grant.revoked {
        reasons.push(R::REVOKED.to_owned());
    }
    if now < grant.valid_from || now >= grant.valid_until {
        reasons.push(R::WINDOW_NOT_LIVE.to_owned());
    }
    // Operator authority lapses on its own cadence: entries disarm past
    // cadence + grace until re-attested, while exits and rolls route exactly
    // like the breaker's disarmed-entries shape. Merit excludes this arm, so
    // a lapsed cadence is never a verdict on the trade.
    if enforce_attestation && proposal.side == ProposalSide::Entry {
        if let Some(window) = grant.re_attestation {
            let since = usage.last_attested.unwrap_or(grant.valid_from);
            let elapsed = (now - since).to_std().unwrap_or(Duration::ZERO);
            if elapsed > window.cadence.saturating_add(window.grace) {
                reasons.push(R::ATTESTATION_LAPSED.to_owned());
            }
        }
    }
    if !grant.scope.allows(proposal) {
        reasons.push(R::OUTSIDE_SCOPE.to_owned());
    }

    if usage.used_trades.saturating_add(1) > grant.budgets.max_trades {
        reasons.push(R::TRADE_BUDGET_EXHAUSTED.to_owned());
    }
    match proposal.leg_contracts() {
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
        reasons.push(R::CONVICTION_BELOW_THRESHOLD.to_owned());
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
        reasons.push(R::MAX_LOSS_EXCEEDED.to_owned());
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

    // Open committed risk is a reading from the venue's own book, not the
    // ledger's lifetime tally: consumed budgets stay monotonic (a closed
    // book never unspends them), while this cap binds at-rest exposure.
    // No reading means the caller declared the producer gates irrelevant
    // (rehearsal) — the cap is inert, never failed open or closed.
    if let Some(reading) = market.open_risk {
        budget_exceeded(
            reading.as_decimal(),
            proposal.max_loss.as_decimal(),
            grant.portfolio.max_open_risk.as_decimal(),
            &mut reasons,
            R::OPEN_RISK_EXCEEDED,
        );
    }
    if market.quote_age_secs > grant.portfolio.quote_max_age_secs {
        reasons.push("quote stale for grant evaluation".to_owned());
    }

    if breakers.tripped {
        reasons.push(R::BREAKER_TRIPPED.to_owned());
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
