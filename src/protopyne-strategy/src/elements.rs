//! Strategy elements: one required signal plus swappable optional pieces.
//!
//! A strategy is a small declarative composition. The [`Signal`] is the only
//! required element; [`StructureSelector`], [`Sizing`], [`LossMgmt`], and
//! [`Monitor`] each ship a default so a strategy author overrides only what
//! the idea needs. [`Strategy::propose`] runs signal, structure, and sizing
//! in that order and emits a [`Proposal`](crate::proposal::Proposal) the
//! risk engine and grant evaluator then gate.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use protopyne_types::{MarkBasis, Money, Qty, SpreadLegs, Timeframe};
use rust_decimal::Decimal;
use thiserror::Error;

use crate::activity::{with_source_score, OptionsActivity};
use crate::economics::{spread_economics, EconRefusal, SpreadEconomics};
use crate::error::StrategyError;
use crate::legs::{LegRequest, LegSelector, StrikePolicy};
use crate::proposal::{Direction, Proposal, ProposalSide, ProposalState, StructureKind};
use crate::risk::{PortfolioFacts, RiskLimits};
use crate::view::{MarketData, MarketView};

/// What a signal concluded on closed bars: a direction and two scores in
/// `[0, 1]`. Scores feed the grant's signal tier **(task-value)**.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SignalOutput {
    /// Direction of exposure.
    pub direction: Direction,
    /// Indicator conviction in `[0, 1]`.
    pub conviction: f64,
    /// Options-activity score in `[0, 1]`.
    pub activity_score: f64,
}

/// Per-run configuration a signal binds into one instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalConfig {
    /// Spacing of the bars the instance will be fed.
    pub timeframe: Timeframe,
}

impl SignalConfig {
    /// Bind into an instance that expects bars spaced `timeframe` apart.
    #[must_use]
    pub const fn new(timeframe: Timeframe) -> Self {
        Self { timeframe }
    }
}

/// One run's owned signal state.
///
/// A [`Strategy`] is shared (`Arc`) across pool workers and sweep variants;
/// per-run state must never live behind `&self` there. The runner binds one
/// instance per run and steps it bar by bar, so two runs — concurrent or
/// sequential — observe independent state.
pub trait SignalInstance: Send {
    /// Evaluate a point-in-time [`MarketView`] into a direction and scores.
    fn evaluate(&mut self, view: &MarketView<'_>) -> Result<SignalOutput, StrategyError>;
    /// Bars the instance needs before it can decide, when it can say.
    fn warmup(&self) -> usize;
}

/// The only required strategy element: indicator and options-activity logic
/// evaluated on closed bars. Implementations are pure over their input view:
/// no I/O, no clock reads. A signal that cannot decide from the view it is
/// given refuses with a reason — it never substitutes a default direction.
pub trait Signal: Send + Sync {
    /// Element name for attribution (which strategy produced a position).
    fn name(&self) -> &str;
    /// Mint one independent instance for a run.
    fn bind(&self, cfg: &SignalConfig) -> Result<Box<dyn SignalInstance>, StrategyError>;
    /// Stateless evaluation for a single-shot decision. Signals that keep
    /// state must bind and evaluate the instance instead.
    fn evaluate(&self, view: &MarketView<'_>) -> Result<SignalOutput, StrategyError>;
}

/// Placeholder signal returning fixed scores. Useful for rehearsal and for
/// tests that target risk/grant behavior rather than indicator logic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FixedSignal {
    direction: Direction,
    conviction: f64,
    activity_score: f64,
}

impl FixedSignal {
    /// Build a fixed signal. Scores must sit in `[0, 1]`; anything else is
    /// refused rather than clamped, so misconfiguration stays loud.
    pub fn new(
        direction: Direction,
        conviction: f64,
        activity_score: f64,
    ) -> Result<Self, StrategyError> {
        if !(0.0..=1.0).contains(&conviction) {
            return Err(StrategyError::InvalidElement(format!(
                "conviction {conviction} outside [0, 1]"
            )));
        }
        if !(0.0..=1.0).contains(&activity_score) {
            return Err(StrategyError::InvalidElement(format!(
                "activity score {activity_score} outside [0, 1]"
            )));
        }
        Ok(Self {
            direction,
            conviction,
            activity_score,
        })
    }

    fn output(&self) -> SignalOutput {
        SignalOutput {
            direction: self.direction,
            conviction: self.conviction,
            activity_score: self.activity_score,
        }
    }
}

impl Signal for FixedSignal {
    fn name(&self) -> &str {
        "fixed"
    }

    fn bind(&self, _cfg: &SignalConfig) -> Result<Box<dyn SignalInstance>, StrategyError> {
        Ok(Box::new(*self))
    }

    fn evaluate(&self, _view: &MarketView<'_>) -> Result<SignalOutput, StrategyError> {
        Ok(self.output())
    }
}

impl SignalInstance for FixedSignal {
    fn evaluate(&mut self, _view: &MarketView<'_>) -> Result<SignalOutput, StrategyError> {
        Ok(self.output())
    }

    fn warmup(&self) -> usize {
        0
    }
}

/// Where a proposal's price, loss, and committed notional come from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EconomicsPolicy {
    /// The caller's configured values stand (the default; pinned legacy).
    #[default]
    ConfigSupplied,
    /// Structure selection prices the package from the chain.
    ChainDerived,
}

impl EconomicsPolicy {
    /// Whether the config-supplied arm is active.
    #[must_use]
    pub fn is_config_supplied(&self) -> bool {
        matches!(self, Self::ConfigSupplied)
    }
}

/// Terms a caller supplies to structure selection.
#[derive(Debug, Clone, Copy, bon::Builder)]
#[builder(start_fn = new)]
pub struct StructureRequest {
    /// Requested structure shape.
    pub structure: StructureKind,
    /// Minimum days to expiry.
    pub dte_days: u32,
    /// Upper bound on days to expiry, when the caller knows one (the
    /// authority's window). Unset leaves selection unbounded.
    pub max_dte_days: Option<u32>,
    /// Contracts per spread unit.
    pub quantity: Qty,
    /// Commission per contract, used to price the package.
    pub fee_per_contract: Money,
    /// Credit intent: sell the near-the-money anchor.
    #[builder(default)]
    pub credit: bool,
    /// How the anchor strike is chosen.
    #[builder(default)]
    pub strike_policy: StrikePolicy,
}

/// What selection concluded: the kind, plus legs, economics, and the
/// instrument's own DTE when the selector is chain-aware. An explicit
/// selector returns the kind and the requested DTE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructureSelection {
    /// Structure to propose.
    pub kind: StructureKind,
    /// Concrete legs, when selected from a chain.
    pub legs: Option<SpreadLegs>,
    /// Package economics, when legs were selected.
    pub economics: Option<SpreadEconomics>,
    /// Days to the selected expiry — the instrument the proposal names, so a
    /// grant window judges the trade rather than the request.
    pub dte_days: u32,
}

impl StructureSelection {
    /// A kind-only selection: no legs, no economics, the requested DTE.
    #[must_use]
    pub const fn kind_only(kind: StructureKind, dte_days: u32) -> Self {
        Self {
            kind,
            legs: None,
            economics: None,
            dte_days,
        }
    }

    /// A chain-derived selection: the kind, its legs, the package's
    /// economics, and the chosen expiry's DTE.
    #[must_use]
    pub const fn priced(
        kind: StructureKind,
        legs: SpreadLegs,
        economics: SpreadEconomics,
        dte_days: u32,
    ) -> Self {
        Self {
            kind,
            legs: Some(legs),
            economics: Some(economics),
            dte_days,
        }
    }
}

/// Why structure selection refused.
#[derive(Debug, Error)]
pub enum StructureRefusal {
    /// Leg selection lacked adequate data for the request.
    #[error("{0}")]
    Legs(#[from] crate::legs::LegSelectRefusal),
    /// Selected legs could not be priced within the defined-risk envelope.
    #[error("{0}")]
    Economics(#[from] EconRefusal),
}

/// Chooses instrument structure, strikes, and DTE policy. Optional: defaults
/// to an explicit, caller-supplied structure.
pub trait StructureSelector: Send + Sync {
    /// Pick the structure for this signal over a point-in-time view.
    fn select(
        &self,
        view: &MarketView<'_>,
        signal: &SignalOutput,
        request: &StructureRequest,
    ) -> Result<StructureSelection, StructureRefusal>;
}

/// Structure selector that always returns one explicit structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExplicitStructure {
    /// The structure every proposal uses.
    pub structure: StructureKind,
}

impl StructureSelector for ExplicitStructure {
    fn select(
        &self,
        _view: &MarketView<'_>,
        _signal: &SignalOutput,
        request: &StructureRequest,
    ) -> Result<StructureSelection, StructureRefusal> {
        // No chain means no instrument to name, so the request's DTE stands.
        Ok(StructureSelection::kind_only(
            self.structure,
            request.dte_days,
        ))
    }
}

/// Chain-aware selection: the one leg chooser, priced through
/// [`spread_economics`](crate::economics::spread_economics).
impl StructureSelector for LegSelector {
    fn select(
        &self,
        view: &MarketView<'_>,
        signal: &SignalOutput,
        request: &StructureRequest,
    ) -> Result<StructureSelection, StructureRefusal> {
        let chain = view
            .data
            .chain
            .as_ref()
            .ok_or(crate::legs::LegSelectRefusal::EmptySnapshot)?;
        let touch = view
            .data
            .bars
            .last()
            .map(|bar| Money::new(bar.c))
            .or_else(|| {
                view.closes
                    .last()
                    .and_then(|close| Decimal::from_f64_retain(*close))
                    .map(Money::new)
            })
            .ok_or(crate::legs::LegSelectRefusal::NoTouch)?;
        let as_of = view.as_of.date_naive();
        let legs = self.select_directional(
            &LegRequest {
                structure: request.structure,
                direction: signal.direction,
                credit: request.credit,
                strike_policy: request.strike_policy,
                dte_days: request.dte_days,
                max_dte_days: request.max_dte_days,
                underlying: chain.underlying(),
                touch,
                as_of,
            },
            chain.quotes(),
        )?;
        let economics = spread_economics(
            &legs,
            chain.quotes(),
            request.quantity,
            request.fee_per_contract,
        )?;
        // The package lives until its front leg expires: that is the window a
        // grant's DTE tier judges, so the proposal names it rather than the
        // requested minimum.
        let front = legs
            .legs()
            .iter()
            .map(|leg| leg.contract.expiry)
            .min()
            .ok_or(crate::legs::LegSelectRefusal::NoSuitableStrikes(
                "selected package carries no legs".to_owned(),
            ))?;
        Ok(StructureSelection::priced(
            request.structure,
            legs,
            economics,
            crate::legs::dte_of(front, as_of),
        ))
    }
}

/// One open position as the loss and monitor elements see it: identity,
/// direction, size, and entry economics. A strategy-side view over the
/// venue's book — venue position types never cross into strategy code.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionView {
    /// Instrument the position holds.
    pub underlying: String,
    /// Which way the position leans.
    pub side: Direction,
    /// Contracts (or shares) held.
    pub quantity: u32,
    /// Average entry price per unit.
    pub entry_avg: Money,
    /// Committed max loss on the whole position.
    pub max_loss: Money,
    /// Days to the front expiry, when the book names one. The time rule
    /// reads this; an unknown expiry never triggers it.
    pub dte_days: Option<u32>,
}

/// What a position is marked at: the basis that produced the mark and the
/// per-unit mark itself. `mark` is `None` exactly when the basis is absent —
/// absence is never a zero, and a rule that needs a price must refuse
/// rather than act on one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MarkContext {
    /// Where the mark came from.
    pub basis: MarkBasis,
    /// Per-unit mark, when the basis produced one.
    pub mark: Option<Money>,
}

/// Quantity containment the composed strategy enforces after the sizing
/// policy returns: the policy asks, these bounds decide. Every bound present
/// only ever reduces; absent bounds leave the policy's size standing (the
/// pinned rehearsal legacy). The runner threads the risk engine's per-trade
/// contract cap and the observed facts; the per-unit price converts buying
/// power to contracts where the caller priced one (the config-supplied
/// limit) — a chain-derived package has no per-unit price until selection
/// prices it, so its buying-power bound lives in the sizing policy, which
/// reads the same facts.
#[derive(Debug, Clone, Copy, Default)]
pub struct SizingCaps<'a> {
    /// Risk engine's per-trade contract cap. Binds every arm.
    pub risk: Option<&'a RiskLimits>,
    /// Observed portfolio facts. Bounds buying power where priced.
    pub facts: Option<&'a PortfolioFacts>,
}

impl SizingCaps<'_> {
    /// Reduce a policy's answer to what the bounds permit. A negative answer
    /// floors at zero (downstream refuses a zero quantity with a reason —
    /// containment never invents a trade). Buying power divides by the
    /// per-unit price the caller priced; `None` (or a non-positive price)
    /// leaves that leg unenforced.
    #[must_use]
    pub fn contain(&self, desired: Qty, unit_price: Option<Money>) -> Qty {
        let mut qty = desired.0.max(Decimal::ZERO);
        if let Some(risk) = self.risk {
            qty = qty.min(Decimal::from(risk.max_contracts_per_trade));
        }
        if let Some(price) = unit_price.filter(|price| price.as_decimal().is_sign_positive()) {
            if let Some(power) = self.facts.and_then(|facts| facts.buying_power) {
                qty = qty.min((power.as_decimal() / price.as_decimal()).floor());
            }
        }
        Qty::new(qty)
    }
}

/// Determines quantity. Optional: defaults to a fixed contract count.
/// Sizing can only reduce what risk permits, never exceed it — the engine
/// re-checks every quantity, so an aggressive sizer is contained.
pub trait Sizing: Send + Sync {
    /// Policy name for attribution (which sizing priced a proposal).
    fn name(&self) -> &str;
    /// Contracts (or shares) for this signal given the observed portfolio
    /// facts. An unobserved bundle must size as if broke: a policy never
    /// invents buying power, and the composed strategy still contains the
    /// answer against its caps.
    fn size(&self, signal: &SignalOutput, facts: &PortfolioFacts) -> Qty;
}

/// Whether a fact bundle can do sizing arithmetic: every number present is
/// finite. A NaN anywhere poisons the bundle to zero — sizing never
/// computes on numbers that cannot be trusted.
fn bundle_is_arithmetical(facts: &PortfolioFacts) -> bool {
    facts.net_delta.is_none_or(f64::is_finite)
}

/// Fixed-count sizing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedContracts {
    /// Contracts on every proposal.
    pub contracts: u32,
}

impl Sizing for FixedContracts {
    fn name(&self) -> &str {
        "fixed"
    }

    fn size(&self, _signal: &SignalOutput, _facts: &PortfolioFacts) -> Qty {
        Qty::new(rust_decimal::Decimal::from(self.contracts))
    }
}

/// Fixed fraction of net liquidation risked at a configured per-unit max
/// loss: `floor(fraction * net_liq / per_unit_max_loss)` contracts. The
/// fraction and the per-unit loss are configured (the strategy's own
/// packaging); net liquidation is observed, never configured. An unobserved
/// net liq sizes zero — downstream refuses a zero quantity with a reason,
/// so no gate can mistake it for a trade.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PercentNetLiq {
    /// Fraction of net liquidation risked, in `(0, 1]`.
    pub fraction: f64,
    /// Max loss per unit the fraction funds.
    pub per_unit_max_loss: Money,
}

impl PercentNetLiq {
    /// Build a validated policy: the fraction must sit in `(0, 1]` and the
    /// per-unit loss must be positive — anything else refuses rather than
    /// sizing a nonsense quantity.
    pub fn new(fraction: f64, per_unit_max_loss: Money) -> Result<Self, StrategyError> {
        if !(fraction > 0.0 && fraction <= 1.0) {
            return Err(StrategyError::InvalidElement(format!(
                "net-liq fraction {fraction} outside (0, 1]"
            )));
        }
        if per_unit_max_loss.as_decimal() <= Decimal::ZERO {
            return Err(StrategyError::InvalidElement(format!(
                "per-unit max loss must be positive: {per_unit_max_loss}"
            )));
        }
        Ok(Self {
            fraction,
            per_unit_max_loss,
        })
    }
}

impl Sizing for PercentNetLiq {
    fn name(&self) -> &str {
        "percent-net-liq"
    }

    fn size(&self, _signal: &SignalOutput, facts: &PortfolioFacts) -> Qty {
        if !bundle_is_arithmetical(facts) {
            return Qty::zero();
        }
        let Some(net_liq) = facts.net_liq else {
            return Qty::zero();
        };
        // A literal-built policy past the validating constructor still
        // fails closed: NaN or out-of-band fractions size zero.
        let Some(fraction) = Decimal::from_f64_retain(self.fraction) else {
            return Qty::zero();
        };
        if fraction <= Decimal::ZERO || self.per_unit_max_loss.as_decimal() <= Decimal::ZERO {
            return Qty::zero();
        }
        Qty::new(
            ((fraction * net_liq.as_decimal()) / self.per_unit_max_loss.as_decimal())
                .floor()
                .max(Decimal::ZERO),
        )
    }
}

/// Buying-power funding: `floor(fraction * buying_power / per_unit_price)`
/// contracts. Same shape as [`PercentNetLiq`]: configured fraction and
/// per-unit price, observed buying power, zero when unobserved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BuyingPower {
    /// Fraction of buying power funded, in `(0, 1]`.
    pub fraction: f64,
    /// Price per unit the fraction funds.
    pub per_unit_price: Money,
}

impl BuyingPower {
    /// Build a validated policy: the fraction must sit in `(0, 1]` and the
    /// per-unit price must be positive.
    pub fn new(fraction: f64, per_unit_price: Money) -> Result<Self, StrategyError> {
        if !(fraction > 0.0 && fraction <= 1.0) {
            return Err(StrategyError::InvalidElement(format!(
                "buying-power fraction {fraction} outside (0, 1]"
            )));
        }
        if per_unit_price.as_decimal() <= Decimal::ZERO {
            return Err(StrategyError::InvalidElement(format!(
                "per-unit price must be positive: {per_unit_price}"
            )));
        }
        Ok(Self {
            fraction,
            per_unit_price,
        })
    }
}

impl Sizing for BuyingPower {
    fn name(&self) -> &str {
        "buying-power"
    }

    fn size(&self, _signal: &SignalOutput, facts: &PortfolioFacts) -> Qty {
        if !bundle_is_arithmetical(facts) {
            return Qty::zero();
        }
        let Some(power) = facts.buying_power else {
            return Qty::zero();
        };
        let Some(fraction) = Decimal::from_f64_retain(self.fraction) else {
            return Qty::zero();
        };
        if fraction <= Decimal::ZERO || self.per_unit_price.as_decimal() <= Decimal::ZERO {
            return Qty::zero();
        }
        Qty::new(
            ((fraction * power.as_decimal()) / self.per_unit_price.as_decimal())
                .floor()
                .max(Decimal::ZERO),
        )
    }
}

/// What loss management proposes for a live position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LossAction {
    /// Machine-readable kind (e.g. `"stop"`, `"profit-target"`, `"roll"`).
    pub kind: String,
    /// Why the action is proposed.
    pub reason: String,
}

/// Defines how a position is protected and unwound. Optional: defaults to
/// holding (no action). Protective exits rest at the broker wherever it
/// supports them, so they survive an outage of this platform.
pub trait LossMgmt: Send + Sync {
    /// Rule name for attribution (which rule proposed an exit). Defaults to
    /// `"custom"` so third-party rules compile unchanged; every shipped
    /// rule names itself.
    fn name(&self) -> &str {
        "custom"
    }

    /// Propose actions for a position at its mark, in firing order; empty
    /// to hold. An absent mark acts on nothing — absence is not a trigger,
    /// so a rule that needs a price refuses rather than inventing one.
    fn review(&self, position: &PositionView, mark: &MarkContext) -> Vec<LossAction>;
}

/// Loss management that never acts (hold every position).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HoldLoss;

impl LossMgmt for HoldLoss {
    fn name(&self) -> &str {
        "hold"
    }

    fn review(&self, _position: &PositionView, _mark: &MarkContext) -> Vec<LossAction> {
        Vec::new()
    }
}

/// Whole-book unrealised at the mark: the signed per-unit distance from
/// entry, scaled to the held quantity. Long gains when the mark prints
/// above entry; short gains when it prints below. Pure price distance —
/// the view carries no fee line, so no fee is estimated.
fn unrealised_at(position: &PositionView, mark: Money) -> Money {
    let per_unit = match position.side {
        Direction::Long => mark.as_decimal() - position.entry_avg.as_decimal(),
        Direction::Short => position.entry_avg.as_decimal() - mark.as_decimal(),
    };
    Money::new(per_unit * Decimal::from(position.quantity))
}

/// Marked price, when the chain answered. `None` (an absent mark, or a
/// basis that produced no price) means the rule holds — absence is not a
/// trigger, and no rule invents a P&L.
fn priced(mark: &MarkContext) -> Option<Money> {
    mark.mark
}

/// Protective stop: fires `"stop"` once the book's unrealised loss runs
/// strictly past the threshold. Exactly on the threshold holds — the
/// boundary is not a trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopLoss {
    /// Unrealised loss that trips the stop.
    pub max_loss: Money,
}

impl StopLoss {
    /// Build a validated stop: the threshold must be positive, since a
    /// non-positive stop can never be a trigger.
    pub fn new(max_loss: Money) -> Result<Self, StrategyError> {
        if max_loss.as_decimal() <= Decimal::ZERO {
            return Err(StrategyError::InvalidElement(format!(
                "stop threshold must be positive: {max_loss}"
            )));
        }
        Ok(Self { max_loss })
    }
}

impl LossMgmt for StopLoss {
    fn name(&self) -> &str {
        "stop"
    }

    fn review(&self, position: &PositionView, mark: &MarkContext) -> Vec<LossAction> {
        let Some(price) = priced(mark) else {
            return Vec::new();
        };
        if unrealised_at(position, price).as_decimal() < -self.max_loss.as_decimal() {
            vec![LossAction {
                kind: "stop".to_owned(),
                reason: format!(
                    "unrealised {pnl} past stop {limit}",
                    pnl = unrealised_at(position, price),
                    limit = self.max_loss,
                ),
            }]
        } else {
            Vec::new()
        }
    }
}

/// Profit target: fires `"profit-target"` once the book's unrealised gain
/// runs strictly past the threshold. Exactly on the threshold holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfitTarget {
    /// Unrealised gain that trips the target.
    pub target: Money,
}

impl ProfitTarget {
    /// Build a validated target: the threshold must be positive.
    pub fn new(target: Money) -> Result<Self, StrategyError> {
        if target.as_decimal() <= Decimal::ZERO {
            return Err(StrategyError::InvalidElement(format!(
                "profit-target threshold must be positive: {target}"
            )));
        }
        Ok(Self { target })
    }
}

impl LossMgmt for ProfitTarget {
    fn name(&self) -> &str {
        "profit-target"
    }

    fn review(&self, position: &PositionView, mark: &MarkContext) -> Vec<LossAction> {
        let Some(price) = priced(mark) else {
            return Vec::new();
        };
        if unrealised_at(position, price).as_decimal() > self.target.as_decimal() {
            vec![LossAction {
                kind: "profit-target".to_owned(),
                reason: format!(
                    "unrealised {pnl} past target {limit}",
                    pnl = unrealised_at(position, price),
                    limit = self.target,
                ),
            }]
        } else {
            Vec::new()
        }
    }
}

/// Time stop: fires `"time-stop"` once the book's remaining DTE drops
/// strictly below the floor. Exactly on the floor holds; an unknown expiry
/// holds; and — like every rule — an absent mark holds, so a dark chain
/// never triggers an exit the platform cannot price.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeStop {
    /// DTE floor the book must stay above.
    pub max_dte_days: u32,
}

impl TimeStop {
    /// Build a time stop over the DTE floor. Any floor stands, including
    /// zero (exit once expired).
    #[must_use]
    pub const fn new(max_dte_days: u32) -> Self {
        Self { max_dte_days }
    }
}

impl LossMgmt for TimeStop {
    fn name(&self) -> &str {
        "time-stop"
    }

    fn review(&self, position: &PositionView, mark: &MarkContext) -> Vec<LossAction> {
        if priced(mark).is_none() {
            return Vec::new();
        }
        let Some(dte) = position.dte_days else {
            return Vec::new();
        };
        if dte < self.max_dte_days {
            vec![LossAction {
                kind: "time-stop".to_owned(),
                reason: format!(
                    "{dte} days left below the {floor}-day floor",
                    floor = self.max_dte_days
                ),
            }]
        } else {
            Vec::new()
        }
    }
}

/// Ordered rule set: reviews every rule in installation order and
/// concatenates the firings, so protection reads before profit-taking
/// reads before time reads when installed that way.
#[derive(Default)]
pub struct LossSet {
    /// Rules in firing order.
    pub rules: Vec<Arc<dyn LossMgmt>>,
}

impl std::fmt::Debug for LossSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LossSet")
            .field("rules", &self.rules.len())
            .finish()
    }
}

impl LossSet {
    /// Compose rules in firing order.
    #[must_use]
    pub fn new(rules: Vec<Arc<dyn LossMgmt>>) -> Self {
        Self { rules }
    }
}

impl LossMgmt for LossSet {
    fn name(&self) -> &str {
        "set"
    }

    fn review(&self, position: &PositionView, mark: &MarkContext) -> Vec<LossAction> {
        self.rules
            .iter()
            .flat_map(|rule| rule.review(position, mark))
            .collect()
    }
}

/// A read-only monitoring observation. Monitors may propose actions, but only
/// an approval or an armed grant can turn one into an order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorAlert {
    /// Portfolio the observation belongs to.
    pub portfolio: String,
    /// What was observed.
    pub message: String,
}

/// Observes positions, portfolios, and health. Optional, read-only, and
/// independent of strategy logic: defaults to silence.
pub trait Monitor: Send + Sync {
    /// Observe the portfolio facts and open positions; emit alerts, if any.
    fn observe(&self, facts: &PortfolioFacts, positions: &[PositionView]) -> Vec<MonitorAlert>;
}

/// Monitor that never alerts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NullMonitor;

impl Monitor for NullMonitor {
    fn observe(&self, _facts: &PortfolioFacts, _positions: &[PositionView]) -> Vec<MonitorAlert> {
        Vec::new()
    }
}

/// Threshold breach monitor: alerts when open risk crosses its cap.
/// The alert is a proposal input, never an order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitMonitor {
    /// Portfolio under watch.
    pub portfolio: String,
    /// Open-risk cap the facts are judged against.
    pub cap: Money,
}

impl Monitor for LimitMonitor {
    fn observe(&self, facts: &PortfolioFacts, _positions: &[PositionView]) -> Vec<MonitorAlert> {
        if facts.open_risk.as_decimal() > self.cap.as_decimal() {
            vec![MonitorAlert {
                portfolio: self.portfolio.clone(),
                message: format!("open risk {} above cap {}", facts.open_risk, self.cap),
            }]
        } else {
            Vec::new()
        }
    }
}

/// Economics the caller supplies for a proposal. Signal, structure, and
/// sizing fill in direction, structure, and quantity; price, loss, notional,
/// and expiry come from the caller's quote and structure math.
#[derive(Debug, Clone, Copy)]
pub struct ProposeInput<'a> {
    /// Strategy name for attribution.
    pub strategy: &'a str,
    /// Underlying symbol.
    pub underlying: &'a str,
    /// Entry, exit, or roll.
    pub side: ProposalSide,
    /// Closed-bar closes, oldest first, for the signal.
    pub closes: &'a [f64],
    /// Limit price per contract/share.
    pub limit_price: Money,
    /// Worst-case loss if the position goes to zero value.
    pub max_loss: Money,
    /// Committed notional.
    pub notional: Money,
    /// Days to expiry of the front leg: the minimum the request asks for.
    /// A chain-aware selection replaces it with the chosen expiry's own DTE.
    pub dte_days: u32,
    /// Upper bound on days to expiry, when the caller knows one (the
    /// authority's window). `None` leaves selection bounded by the minimum
    /// alone.
    pub max_dte_days: Option<u32>,
    /// Where price, loss, and notional come from.
    pub economics: EconomicsPolicy,
    /// Structure shape the selector should build.
    pub structure: StructureKind,
    /// Credit intent for the selector.
    pub credit: bool,
    /// Commission per contract used to price the package.
    pub fee_per_contract: Money,
    /// Quantity containment applied after the sizing policy returns. The
    /// runner threads the risk engine's per-trade cap and the observed
    /// facts; absent bounds leave the policy's size standing.
    pub caps: SizingCaps<'a>,
    /// Proposal creation time.
    pub created_at: DateTime<Utc>,
}

/// A strategy: one required signal plus independently swappable optional
/// elements. Backtest, paper, and (later) live share this composition and
/// differ only at the venue boundary.
pub struct Strategy {
    signal: Arc<dyn Signal>,
    structure: Arc<dyn StructureSelector>,
    sizing: Arc<dyn Sizing>,
    loss: Arc<dyn LossMgmt>,
    monitor: Arc<dyn Monitor>,
    activity: Option<Arc<dyn OptionsActivity>>,
}

impl Clone for Strategy {
    /// Share the composed elements: every role is already reference-counted,
    /// so cloning bumps five `Arc`s and duplicates nothing. Pool workers
    /// each take a clone rather than sharing one handle across `await`
    /// points they do not co-own.
    fn clone(&self) -> Self {
        Self {
            signal: Arc::clone(&self.signal),
            structure: Arc::clone(&self.structure),
            sizing: Arc::clone(&self.sizing),
            loss: Arc::clone(&self.loss),
            monitor: Arc::clone(&self.monitor),
            activity: self.activity.as_ref().map(Arc::clone),
        }
    }
}

impl Strategy {
    /// Compose a strategy around a signal; optionals take their defaults
    /// (explicit vertical, one fixed contract, hold, silence).
    #[must_use]
    pub fn with_signal(signal: Arc<dyn Signal>) -> Self {
        Self {
            signal,
            structure: Arc::new(ExplicitStructure {
                structure: StructureKind::Vertical,
            }),
            sizing: Arc::new(FixedContracts { contracts: 1 }),
            loss: Arc::new(HoldLoss),
            monitor: Arc::new(NullMonitor),
            activity: None,
        }
    }

    /// The deployed momentum composition: a kernel trend signal (ten-bar
    /// momentum, sign rule at a five-point scale — the calibrated trend
    /// treatment), buying-power sizing at half the observed power against a
    /// $250 per-unit price, the ordered stop/target/time loss set, and the
    /// chain-aware leg selector. Refuses when any element's own validation
    /// refuses; the rehearsal fixed-signal composition stays the default,
    /// so nothing moves until an operator selects this one.
    pub fn momentum() -> Result<Self, StrategyError> {
        use crate::signals_kernel::{DirectionRule, KernelSignal, Polarity, SeriesRef};
        use protopyne_kernels::{ParamSet, PriceSource};

        let signal = KernelSignal::new(
            vec![SeriesRef::indicator(
                "mom",
                ParamSet::new().with_int("length", 10),
                PriceSource::Close,
            )],
            DirectionRule::Sign {
                line: 0,
                scale: 5.0,
                polarity: Polarity::Momentum,
            },
        )?;
        let sizing = BuyingPower::new(0.5, Money::new(Decimal::from(250)))?;
        let loss = LossSet::new(vec![
            Arc::new(StopLoss::new(Money::new(Decimal::from(200)))?),
            Arc::new(ProfitTarget::new(Money::new(Decimal::from(400)))?),
            Arc::new(TimeStop::new(7)),
        ]);
        Ok(Self::with_signal(Arc::new(signal))
            .with_structure(Arc::new(LegSelector::new().build()))
            .with_sizing(Arc::new(sizing))
            .with_loss(Arc::new(loss)))
    }

    /// Override the structure selector.
    #[must_use]
    pub fn with_structure(mut self, structure: Arc<dyn StructureSelector>) -> Self {
        self.structure = structure;
        self
    }

    /// Override sizing.
    #[must_use]
    pub fn with_sizing(mut self, sizing: Arc<dyn Sizing>) -> Self {
        self.sizing = sizing;
        self
    }

    /// Override loss management.
    #[must_use]
    pub fn with_loss(mut self, loss: Arc<dyn LossMgmt>) -> Self {
        self.loss = loss;
        self
    }

    /// Override monitoring.
    #[must_use]
    pub fn with_monitor(mut self, monitor: Arc<dyn Monitor>) -> Self {
        self.monitor = monitor;
        self
    }

    /// Install an options-activity source. Absent (the default), the signal's
    /// own `activity_score` stands; a source that cannot score the view also
    /// leaves it standing.
    #[must_use]
    pub fn with_options_activity(mut self, activity: Arc<dyn OptionsActivity>) -> Self {
        self.activity = Some(activity);
        self
    }

    /// Whether an options-activity source is installed. The default
    /// constructors install none, so a chain score is opt-in.
    #[must_use]
    pub fn has_options_activity(&self) -> bool {
        self.activity.is_some()
    }

    /// Signal name for attribution.
    #[must_use]
    pub fn signal_name(&self) -> &str {
        self.signal.name()
    }

    /// Sizing-policy name for attribution.
    #[must_use]
    pub fn sizing_name(&self) -> &str {
        self.sizing.name()
    }

    /// Loss-rule name for attribution.
    #[must_use]
    pub fn loss_name(&self) -> &str {
        self.loss.name()
    }

    /// Mint one independent signal instance for a run.
    pub fn bind(&self, cfg: &SignalConfig) -> Result<Box<dyn SignalInstance>, StrategyError> {
        self.signal.bind(cfg)
    }

    /// Run signal, structure, and sizing over closed bars and emit a
    /// [`Proposal`] in state [`Proposed`](ProposalState::Proposed), given a
    /// caller-assembled [`MarketView`].
    ///
    /// The view carries the closes the signal reads; it exists so signals can
    /// see point-in-time bars and chains without a growing parameter list.
    /// `signal` is the run's own instance, so stateful signals keep their
    /// state across bars and never share it between runs.
    /// Caller-supplied economics fail closed: a negative limit, max loss, or
    /// notional would sail through caps and poison budget accounting
    /// downward, so each refuses with a reason instead of clamping to zero.
    pub fn propose_with(
        &self,
        id: &str,
        input: ProposeInput<'_>,
        market: &MarketView<'_>,
        signal: &mut dyn SignalInstance,
    ) -> Result<Proposal, StrategyError> {
        if input.economics.is_config_supplied() {
            if input.limit_price.as_decimal().is_sign_negative() {
                return Err(StrategyError::InvalidElement(format!(
                    "limit price must not be negative: {}",
                    input.limit_price
                )));
            }
            if input.max_loss.as_decimal().is_sign_negative() {
                return Err(StrategyError::InvalidElement(format!(
                    "max loss must not be negative: {}",
                    input.max_loss
                )));
            }
            if input.notional.as_decimal().is_sign_negative() {
                return Err(StrategyError::InvalidElement(format!(
                    "notional must not be negative: {}",
                    input.notional
                )));
            }
        }
        let mut output = signal.evaluate(market)?;
        if let Some(source) = &self.activity {
            output = with_source_score(output, market, source.as_ref())?;
        }
        // Containment after the policy returns: the policy asks against the
        // observed facts (unobserved when the caller threaded none), then
        // the bounds decide. The per-unit price is the caller's own limit —
        // known before selection only on the config-supplied arm.
        let unobserved = PortfolioFacts::unobserved();
        let facts = input.caps.facts.unwrap_or(&unobserved);
        let unit = input
            .economics
            .is_config_supplied()
            .then_some(input.limit_price);
        let quantity = input.caps.contain(self.sizing.size(&output, facts), unit);
        let request = StructureRequest::new()
            .structure(input.structure)
            .dte_days(input.dte_days)
            .maybe_max_dte_days(input.max_dte_days)
            .quantity(quantity)
            .fee_per_contract(input.fee_per_contract)
            .credit(input.credit)
            .build();
        let selection = self
            .structure
            .select(market, &output, &request)
            .map_err(|refused| StrategyError::InvalidElement(refused.to_string()))?;
        let (limit_price, max_loss, notional, legs, economics) = if input
            .economics
            .is_config_supplied()
        {
            (
                input.limit_price,
                input.max_loss,
                input.notional,
                None,
                None,
            )
        } else {
            let economics = selection.economics.ok_or_else(|| {
                StrategyError::InvalidElement(
                    "chain-derived selection supplied no economics".to_owned(),
                )
            })?;
            let legs = selection.legs.clone().ok_or_else(|| {
                StrategyError::InvalidElement("chain-derived selection supplied no legs".to_owned())
            })?;
            (
                economics.mid_limit,
                economics.capital_requirement,
                economics.capital_requirement,
                Some(legs),
                Some(economics),
            )
        };
        Ok(Proposal::new()
            .id(id.to_owned())
            .strategy(input.strategy.to_owned())
            .underlying(input.underlying.to_owned())
            .direction(output.direction)
            .side(input.side)
            .structure(selection.kind)
            .quantity(quantity)
            .limit_price(limit_price)
            .max_loss(max_loss)
            .notional(notional)
            .dte_days(selection.dte_days)
            .conviction(output.conviction)
            .activity_score(output.activity_score)
            .maybe_legs(legs)
            .maybe_economics(economics)
            .state(ProposalState::Proposed)
            .created_at(input.created_at)
            .build())
    }

    /// Run signal, structure, and sizing over the closes a caller supplies
    /// directly, emitting a [`Proposal`] in state
    /// [`Proposed`](ProposalState::Proposed).
    ///
    /// A thin delegate over [`propose_with`](Self::propose_with): it builds a
    /// minimal bar-less, chain-less view from `input` and binds a fresh
    /// instance, so there is exactly one proposal body. Callers holding a run
    /// across bars should bind once and call `propose_with` with that instance.
    pub fn propose(&self, id: &str, input: ProposeInput<'_>) -> Result<Proposal, StrategyError> {
        let data = MarketData::empty(input.dte_days);
        let view = MarketView {
            data: &data,
            closes: input.closes,
            as_of: input.created_at,
        };
        let mut signal = self.bind(&SignalConfig::new(data.timeframe))?;
        self.propose_with(id, input, &view, signal.as_mut())
    }

    /// Ask loss management for protective actions over a marked position.
    /// An absent mark acts on nothing — the rule, not the caller, refuses
    /// to invent a price.
    #[must_use]
    pub fn review_position(&self, position: &PositionView, mark: &MarkContext) -> Vec<LossAction> {
        self.loss.review(position, mark)
    }

    /// Ask the monitor for read-only observations over facts and positions.
    #[must_use]
    pub fn observe(&self, facts: &PortfolioFacts, positions: &[PositionView]) -> Vec<MonitorAlert> {
        self.monitor.observe(facts, positions)
    }
}

#[cfg(test)]
#[path = "../tests/elements.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/sizing_calibration.rs"]
mod sizing_calibration_tests;

#[cfg(test)]
#[path = "../tests/loss_calibration.rs"]
mod loss_calibration_tests;
