//! Strategy elements: one required signal plus swappable optional pieces.
//!
//! A strategy is a small declarative composition. The [`Signal`] is the only
//! required element; [`StructureSelector`], [`Sizing`], [`LossMgmt`], and
//! [`Monitor`] each ship a default so a strategy author overrides only what
//! the idea needs. [`Strategy::propose`] runs signal, structure, and sizing
//! in that order and emits a [`Proposal`](crate::proposal::Proposal) the
//! risk engine and grant evaluator then gate.

use std::sync::Arc;

use chrono::{DateTime, NaiveDate, Utc};
use protopyne_types::chain::LegSide;
use protopyne_types::{MarkBasis, Money, Qty, SpreadLegs, Timeframe};
use rust_decimal::Decimal;
use thiserror::Error;

use crate::activity::{with_source_score, OptionsActivity};
use crate::axes::{Confidence, GateAxis, Merit};
use crate::economics::{
    spread_economics, EconRefusal, PackageSide, SpreadEconomics, UnitEconomics, UnitSelection,
};
use crate::error::StrategyError;
use crate::legs::{LegRequest, LegSelector, StrikePolicy};
use crate::proposal::{Direction, Proposal, ProposalSide, ProposalState, StructureKind};
use crate::risk::{PortfolioFacts, RiskLimits};
use crate::view::{MarketData, MarketView};

/// What a signal concluded on closed bars: a direction, a confidence in that
/// direction, the task-value readings a policy may threshold, and the observed
/// edge/odds pair a research sizer prices.
///
/// **The two axes are distinct and are never substituted for one another.**
///
/// - [`SignalOutput::conviction`] is **producer confidence**: the signal's own
///   doubt about the reading it just produced. It may silence the producer
///   (disarm entries, withhold a reading); it must never deny a trade on merit.
///   Read it through [`SignalOutput::confidence`], which returns the typed
///   [`Confidence`] value.
/// - [`SignalOutput::activity_score`] and [`SignalOutput::volatility`] are
///   **task-value** readings: the opportunity and the regime a sizing policy
///   needs in order to judge the *outcome*. Read them through
///   [`SignalOutput::merit`].
///
/// A confident producer can be wrong for the task, and a hesitant one can be
/// right, so a policy that gates a trade must say that it is applying a
/// task-value threshold to a confidence reading — it must not pretend the two
/// are the same number.
///
/// `volatility` is `Option` on purpose — absence is not a zero: a policy that
/// needs σ and does not get one sizes **zero**, and the composed strategy
/// refuses the zero quantity with a reason
/// ([`EconRefusal::ZeroQuantity`](crate::economics::EconRefusal::ZeroQuantity)).
///
/// `edge` and `odds` are the same doctrine for a Kelly-style sizer: observed
/// estimates (edge per unit, gross odds per unit), `None` until a signal
/// measures them. No shipping signal fills them today; absence sizes zero.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SignalOutput {
    /// Direction of exposure.
    pub direction: Direction,
    /// The producer's **confidence** in its own reading, in `[0, 1]`.
    /// Producer-confidence axis; see the type doc.
    pub conviction: f64,
    /// Options-activity **task-value** score in `[0, 1]`.
    pub activity_score: f64,
    /// Realised per-bar volatility of returns at decision time, when the
    /// signal could measure it. `None` stays `None` all the way to the sizer.
    pub volatility: Option<f64>,
    /// Observed edge per unit at decision time, when the signal estimated
    /// one. `None` stays `None` all the way to the sizer.
    pub edge: Option<f64>,
    /// Observed gross odds per unit at decision time, when the signal
    /// estimated them. `None` stays `None` all the way to the sizer.
    pub odds: Option<f64>,
}

impl SignalOutput {
    /// The producer's confidence in its own reading, on the
    /// producer-confidence axis.
    ///
    /// This is the only typed view of [`conviction`](Self::conviction): a gate
    /// that wants "do not act on a reading the producer doubts" is applying a
    /// *task-value* policy to a confidence value, and must say so at its own
    /// call site (see [`SignalThresholds`](crate::grant::SignalThresholds)).
    #[must_use]
    pub fn confidence(&self) -> Confidence {
        Confidence::saturating(self.conviction)
    }

    /// The task-value readings a policy may threshold. Refuses a malformed
    /// activity score rather than letting a bad number reach a policy that
    /// gates a trade.
    pub fn merit(&self) -> Result<Merit, StrategyError> {
        Merit::new(self.activity_score, self.edge)
    }

    /// Attach the realised volatility the signal measured.
    #[must_use]
    pub fn with_volatility(mut self, volatility: Option<f64>) -> Self {
        self.volatility = volatility;
        self
    }

    /// Attach the observed edge the signal estimated.
    #[must_use]
    pub fn with_edge(mut self, edge: Option<f64>) -> Self {
        self.edge = edge;
        self
    }

    /// Attach the observed odds the signal estimated.
    #[must_use]
    pub fn with_odds(mut self, odds: Option<f64>) -> Self {
        self.odds = odds;
        self
    }
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
    volatility: Option<f64>,
    edge: Option<f64>,
    odds: Option<f64>,
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
            volatility: None,
            edge: None,
            odds: None,
        })
    }

    /// Declare the realised volatility this fixed signal reports: the knob that
    /// lets a volatility-aware sizer be exercised without a bar series.
    #[must_use]
    pub fn with_volatility(mut self, volatility: Option<f64>) -> Self {
        self.volatility = volatility;
        self
    }

    /// Declare the observed edge this fixed signal reports: the knob that
    /// lets a Kelly-style sizer be exercised without an estimating signal.
    #[must_use]
    pub fn with_edge(mut self, edge: Option<f64>) -> Self {
        self.edge = edge;
        self
    }

    /// Declare the observed odds this fixed signal reports: the knob that
    /// lets a Kelly-style sizer be exercised without an estimating signal.
    #[must_use]
    pub fn with_odds(mut self, odds: Option<f64>) -> Self {
        self.odds = odds;
        self
    }

    fn output(&self) -> SignalOutput {
        SignalOutput {
            direction: self.direction,
            conviction: self.conviction,
            activity_score: self.activity_score,
            volatility: self.volatility,
            edge: self.edge,
            odds: self.odds,
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
    /// The caller's per-unit limit price, when the caller prices the unit
    /// (the config-supplied arm). `None` on the chain arm, where the pricer
    /// names the price.
    pub limit_price: Option<Money>,
    /// The caller's per-unit worst-case loss, when the caller states one.
    /// `None` on the chain arm, where the pricer bounds the loss.
    pub max_loss: Option<Money>,
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

    /// Pick the structure and price one unit of it. The default runs
    /// [`select`](Self::select) and derives the unit: a chain-priced
    /// selection carries the one pricer's per-unit economics, and a
    /// kind-only selection carries the caller's per-unit prices, so no new
    /// implementation is needed for a selector that adds no pricing.
    fn select_unit(
        &self,
        view: &MarketView<'_>,
        signal: &SignalOutput,
        request: &StructureRequest,
    ) -> Result<UnitSelection, StructureRefusal> {
        let selection = self.select(view, signal, request)?;
        // A kind-only selection carries the caller's stated totals as its
        // unit: for the single-unit compositions that name them the two
        // coincide, and the proposal path passes config-arm totals through
        // verbatim rather than repricing them.
        let unit = match selection.economics {
            Some(economics) => economics.unit(),
            None => UnitEconomics {
                price_per_unit: request.limit_price.unwrap_or(Money::zero()),
                max_loss_per_unit: request.max_loss.unwrap_or(Money::zero()),
                bid_ask_width: Money::zero(),
                side: if signal.direction == Direction::Long {
                    PackageSide::Buy
                } else {
                    PackageSide::Sell
                },
            },
        };
        Ok(UnitSelection {
            kind: selection.kind,
            legs: selection.legs,
            dte_days: selection.dte_days,
            unit,
            economics: selection.economics,
        })
    }
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

impl LegSelector {
    /// The legs for one selection, without pricing: the shared leg-picking
    /// half of [`select`](StructureSelector::select) and
    /// [`select_unit`](StructureSelector::select_unit), so the two never
    /// diverge on what the package holds.
    fn selected_legs(
        self,
        view: &MarketView<'_>,
        direction: Direction,
        request: &StructureRequest,
    ) -> Result<SpreadLegs, StructureRefusal> {
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
        Ok(self.select_directional(
            &LegRequest {
                structure: request.structure,
                direction,
                credit: request.credit,
                strike_policy: request.strike_policy,
                dte_days: request.dte_days,
                max_dte_days: request.max_dte_days,
                underlying: chain.underlying(),
                touch,
                as_of: view.as_of.date_naive(),
            },
            chain.quotes(),
        )?)
    }

    /// The front expiry's DTE: the package lives until its front leg
    /// expires, so that window is what a grant's DTE tier judges rather
    /// than the requested minimum.
    fn legs_dte(legs: &SpreadLegs, as_of: chrono::NaiveDate) -> Result<u32, StructureRefusal> {
        let front = legs
            .legs()
            .iter()
            .map(|leg| leg.contract.expiry)
            .min()
            .ok_or(crate::legs::LegSelectRefusal::NoSuitableStrikes(
                "selected package carries no legs".to_owned(),
            ))?;
        Ok(crate::legs::dte_of(front, as_of))
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
        let legs = self.selected_legs(view, signal.direction, request)?;
        let chain = view
            .data
            .chain
            .as_ref()
            .ok_or(crate::legs::LegSelectRefusal::EmptySnapshot)?;
        let economics = spread_economics(
            &legs,
            chain.quotes(),
            request.quantity,
            request.fee_per_contract,
        )?;
        let dte_days = Self::legs_dte(&legs, view.as_of.date_naive())?;
        Ok(StructureSelection::priced(
            request.structure,
            legs,
            economics,
            dte_days,
        ))
    }

    fn select_unit(
        &self,
        view: &MarketView<'_>,
        signal: &SignalOutput,
        request: &StructureRequest,
    ) -> Result<UnitSelection, StructureRefusal> {
        let legs = self.selected_legs(view, signal.direction, request)?;
        let chain = view
            .data
            .chain
            .as_ref()
            .ok_or(crate::legs::LegSelectRefusal::EmptySnapshot)?;
        // The unit comes from the one pricer at a single unit, never from a
        // quantity-scaled pricing the caller could mistake for per-unit.
        let economics = spread_economics(
            &legs,
            chain.quotes(),
            Qty::new(Decimal::ONE),
            request.fee_per_contract,
        )?;
        Ok(UnitSelection {
            kind: request.structure,
            dte_days: Self::legs_dte(&legs, view.as_of.date_naive())?,
            legs: Some(legs),
            unit: economics.unit(),
            economics: Some(economics),
        })
    }
}

/// One open position as the loss and monitor elements see it: identity,
/// direction, size, and entry economics. A strategy-side view over the
/// venue's book — venue position types never cross into strategy code.
///
/// `legs` and `events` are `Option` on purpose — `None` means *unknown*, and
/// a rule that needs them **refuses to act**: absence is not a trigger, and
/// no rule invents a calendar or a leg it was not given. Every existing
/// producer leaves both `None`, so existing rules behave exactly as before.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionView<'a> {
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
    /// Per-leg detail, when the book names legs. Borrowed — the view never
    /// owns the book. `None` stays `None` all the way to the rule.
    pub legs: Option<&'a [LegView]>,
    /// Calendar facts about the position, when known. `None` stays `None`
    /// all the way to the rule.
    pub events: Option<PositionEvents>,
}

/// One leg of an open position, as a rule sees it: which side, when it
/// expires, at what strike, and which listed contract (OCC symbol).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegView {
    /// Long or short this contract.
    pub side: LegSide,
    /// Expiry date of the contract.
    pub expiry: NaiveDate,
    /// Strike price per share.
    pub strike: Money,
    /// Listed contract symbol (OCC).
    pub occ: String,
}

/// Calendar facts about an open position: the next ex-dividend date and
/// whether the venue flags an assignment window. Both are authoritative
/// inputs — the guard trusts them and never invents them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionEvents {
    /// Next ex-dividend date of the underlying, when known. `None` means the
    /// calendar is silent on ex-div — the ex-div arm holds, never a default.
    pub ex_dividend_on: Option<NaiveDate>,
    /// Whether the venue flags an assignment window on this position (e.g. a
    /// short American leg the desk could be assigned on tonight).
    pub assignment_eligible: bool,
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

/// What a sizing policy sees: the priced unit, the observed facts, and
/// the marked equity when the caller prices against equity rather than net
/// liquidation. The unit is always per-unit — a policy that needs a total
/// multiplies by the quantity it returns, never by a number it was handed.
#[derive(Debug, Clone, Copy)]
pub struct SizingCtx<'a> {
    /// The priced unit the policy sizes.
    pub unit: &'a UnitEconomics,
    /// Observed portfolio facts.
    pub facts: &'a PortfolioFacts,
    /// Marked equity for the trade, when the caller sizes against equity.
    /// `None` sizes against the observed net liquidation alone.
    pub equity: Option<f64>,
}

/// Determines quantity. Optional: defaults to a fixed contract count.
/// Sizing can only reduce what risk permits, never exceed it — the engine
/// re-checks every quantity, so an aggressive sizer is contained.
pub trait Sizing: Send + Sync {
    /// Policy name for attribution (which sizing priced a proposal).
    fn name(&self) -> &str;
    /// The axis this policy judges on. Required, never defaulted: a sizer
    /// that cannot name its axis cannot back a trade. `TaskValue` policies
    /// express edge or allocation; `Correctness` policies enforce a bound
    /// that must never be violated (affordability, notional caps).
    fn axis(&self) -> GateAxis;
    /// Contracts (or shares) for this signal given the observed portfolio
    /// facts. An unobserved bundle must size as if broke: a policy never
    /// invents buying power, and the composed strategy still contains the
    /// answer against its caps.
    fn size(&self, signal: &SignalOutput, facts: &PortfolioFacts) -> Qty;
    /// Contracts (or shares) for this signal against the priced unit. The
    /// default answers exactly as [`size`](Self::size), so a policy without
    /// instrument awareness keeps its pinned behavior under the new path.
    fn size_for(&self, signal: &SignalOutput, ctx: &SizingCtx<'_>) -> Qty {
        self.size(signal, ctx.facts)
    }
}

/// Whether a fact bundle can do sizing arithmetic: every number present is
/// finite. A NaN anywhere poisons the bundle to zero — sizing never
/// computes on numbers that cannot be trusted.
fn bundle_is_arithmetical(facts: &PortfolioFacts) -> bool {
    facts.net_delta().is_none_or(f64::is_finite)
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

    fn axis(&self) -> GateAxis {
        GateAxis::TaskValue
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

/// Shared sizing arithmetic for fraction-of-base policies
/// (`PercentNetLiq`, `BuyingPower`): `floor(fraction * observed /
/// per_unit)`, floored at zero. Absence is handled by the caller (no
/// `observed` → `Qty::zero` before this runs); a non-retainable fraction
/// (NaN, infinite) or a non-positive fraction/per-unit sizes zero —
/// fail-closed, never a nonsense quantity (M5.1).
fn size_from_fraction(fraction: f64, observed: Decimal, per_unit: Decimal) -> Qty {
    // A literal-built policy past the validating constructor still
    // fails closed: NaN or out-of-band fractions size zero.
    let Some(fraction) = Decimal::from_f64_retain(fraction) else {
        return Qty::zero();
    };
    if fraction <= Decimal::ZERO || per_unit <= Decimal::ZERO {
        return Qty::zero();
    }
    Qty::new(
        ((fraction * observed) / per_unit)
            .floor()
            .max(Decimal::ZERO),
    )
}

impl Sizing for PercentNetLiq {
    fn name(&self) -> &str {
        "percent-net-liq"
    }

    fn axis(&self) -> GateAxis {
        GateAxis::TaskValue
    }

    fn size(&self, _signal: &SignalOutput, facts: &PortfolioFacts) -> Qty {
        if !bundle_is_arithmetical(facts) {
            return Qty::zero();
        }
        let Some(net_liq) = facts.net_liq else {
            return Qty::zero();
        };
        size_from_fraction(
            self.fraction,
            net_liq.as_decimal(),
            self.per_unit_max_loss.as_decimal(),
        )
    }
}

/// Volatility-scaled sizing: `min(base, round(base * target_vol / max(σ,
/// vol_floor)))`.
///
/// **Reduce-only by construction.** The `min(base, …)` is the whole point: a
/// naive inverse-vol sizer *increases* size when volatility collapses, which is
/// the opposite of prudent. Here a collapsing σ can only ever return `base`, and
/// [`SizingCaps::contain`] remains the second, independent bound (the risk cap
/// and buying power bind after the policy, never before).
///
/// σ is the realised per-bar volatility of simple returns the signal measured
/// ([`SignalOutput::volatility`]): absence is **not** a zero — a missing or
/// non-finite σ sizes zero, and the composed strategy refuses that zero quantity
/// with a reason
/// ([`EconRefusal::ZeroQuantity`](crate::economics::EconRefusal::ZeroQuantity)).
///
/// `target_vol` and `vol_floor` are per-bar σ in the same unit as the signal's
/// measurement (unitless fractions, e.g. `0.004` = 0.4% per bar at the decision
/// timeframe). `vol_floor` is the σ below which size stops responding; keeping it
/// at or below `target_vol` is what makes a calm regime hold `base` rather than
/// clamp somewhere below it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VolatilityScaled {
    /// Contracts at the target volatility (and the hard upper bound).
    pub base: u32,
    /// Per-bar σ the policy sizes `base` at.
    pub target_vol: f64,
    /// Per-bar σ below which size stops growing.
    pub vol_floor: f64,
}

/// Documented per-bar σ target at **one-minute** spacing.
///
/// Measured, not tuned: the median of the 20-return population deviation over the
/// vendored SPY 1-minute tape (`protopyne-kernels/tests/data/SPY_1M.csv`, 436
/// bars / 416 windows), where the windows span `p10 = 5.2e-5`, median `1.06e-4`,
/// `p90 = 1.7e-4`. Other timeframes scale by `sqrt(timeframe / 1 minute)` — the
/// standard square-root-of-time assumption, stated rather than fitted, and the
/// calibration test recomputes the band from the tape. These are **task-value**
/// defaults: they are never tuned against a backtest return.
pub const VOL_TARGET_1M: f64 = 0.000_106;

impl VolatilityScaled {
    /// The documented default target σ for a decision timeframe: the measured
    /// one-minute target scaled by `sqrt(t / 1m)`.
    ///
    /// | timeframe | target σ/bar | floor (σ/2) |
    /// |---|---|---|
    /// | 1m | 0.000106 | 0.000053 |
    /// | 5m | 0.000237 | 0.000119 |
    /// | 15m | 0.000411 | 0.000205 |
    /// | 1h | 0.000821 | 0.000411 |
    /// | 1d (390 min) | 0.002093 | 0.001047 |
    #[must_use]
    pub fn default_target_vol(timeframe: Timeframe) -> f64 {
        VOL_TARGET_1M * (timeframe.total_seconds() as f64 / 60.0).sqrt()
    }

    /// The documented default floor for a decision timeframe: half the target,
    /// so a calm regime holds `base` rather than clamping below it.
    #[must_use]
    pub fn default_vol_floor(timeframe: Timeframe) -> f64 {
        Self::default_target_vol(timeframe) / 2.0
    }

    /// A policy at the documented defaults for `timeframe`.
    ///
    /// # Errors
    ///
    /// Only a non-positive `base` could fail, and it cannot: the defaults are
    /// validated by construction.
    pub fn for_timeframe(base: u32, timeframe: Timeframe) -> Result<Self, StrategyError> {
        Self::new(
            base,
            Self::default_target_vol(timeframe),
            Self::default_vol_floor(timeframe),
        )
    }

    /// Build a validated policy: `target_vol` and `vol_floor` must be positive
    /// and finite. A literal-built policy past this constructor still fails
    /// closed in [`size`](Sizing::size) rather than sizing an infinity.
    pub fn new(base: u32, target_vol: f64, vol_floor: f64) -> Result<Self, StrategyError> {
        if !(target_vol > 0.0 && target_vol.is_finite()) {
            return Err(StrategyError::InvalidElement(format!(
                "target volatility {target_vol} outside (0, ∞)"
            )));
        }
        if !(vol_floor > 0.0 && vol_floor.is_finite()) {
            return Err(StrategyError::InvalidElement(format!(
                "volatility floor {vol_floor} outside (0, ∞)"
            )));
        }
        Ok(Self {
            base,
            target_vol,
            vol_floor,
        })
    }
}

impl Sizing for VolatilityScaled {
    fn name(&self) -> &str {
        "volatility-scaled"
    }

    fn axis(&self) -> GateAxis {
        GateAxis::TaskValue
    }

    fn size(&self, signal: &SignalOutput, _facts: &PortfolioFacts) -> Qty {
        // Absence is not a zero: without a measurement there is no size to
        // defend, and the composed strategy refuses the zero with a reason.
        let Some(volatility) = signal.volatility else {
            return Qty::zero();
        };
        if !volatility.is_finite()
            || !self.target_vol.is_finite()
            || !self.vol_floor.is_finite()
            || self.target_vol <= 0.0
            || self.vol_floor <= 0.0
        {
            return Qty::zero();
        }
        let sigma = volatility.max(self.vol_floor);
        let scaled = (f64::from(self.base) * self.target_vol / sigma).round();
        // Reduce-only: rounding and a sub-target floor both stop at `base`.
        let bounded = scaled.min(f64::from(self.base));
        if !bounded.is_finite() || bounded <= 0.0 {
            return Qty::zero();
        }
        let Some(contracts) = Decimal::from_f64_retain(bounded) else {
            return Qty::zero();
        };
        Qty::new(contracts)
    }
}

/// Capped fractional-Kelly sizing: `floor(base * min(cap, fraction * f*))`
/// where `f* = edge / odds`.
///
/// **Research-only by construction.** This is the one sizing policy that must
/// never reach live money untuned, so the type exists only under the
/// `research` feature (compile-time, not a runtime flag): a build without it
/// cannot name the type at all.
///
/// `edge` and `odds` are observed estimates the signal carried on
/// [`SignalOutput`]: absence is **not** a zero — a missing or non-finite
/// input sizes zero, and the composed strategy refuses that zero quantity
/// with a reason
/// ([`EconRefusal::ZeroQuantity`](crate::economics::EconRefusal::ZeroQuantity)).
/// A non-positive edge sizes zero too: this is a *sizing* element, never a
/// direction element, so it never flips a losing idea into a short.
///
/// `fraction` and `cap` both live in `(0, 1]`, so the policy can only ever ask
/// for `base` at most — and [`SizingCaps::contain`] remains the second,
/// independent bound (the risk cap and buying power bind after the policy,
/// never before).
#[cfg(feature = "research")]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FractionalKelly {
    /// Contracts at the full Kelly fraction (and the hard upper bound).
    pub base: u32,
    /// Fraction of the Kelly stake taken, in `(0, 1]`.
    pub fraction: f64,
    /// Maximum fraction of `base` staked, in `(0, 1]`.
    pub cap: f64,
}

#[cfg(feature = "research")]
impl FractionalKelly {
    /// Build a validated policy: `fraction` and `cap` must sit in `(0, 1]` —
    /// anything else refuses rather than sizing a nonsense quantity. A
    /// literal-built policy past this constructor still fails closed in
    /// [`size`](Sizing::size) rather than sizing an infinity.
    pub fn new(base: u32, fraction: f64, cap: f64) -> Result<Self, StrategyError> {
        if !(fraction > 0.0 && fraction <= 1.0 && fraction.is_finite()) {
            return Err(StrategyError::InvalidElement(format!(
                "kelly fraction {fraction} outside (0, 1]"
            )));
        }
        if !(cap > 0.0 && cap <= 1.0 && cap.is_finite()) {
            return Err(StrategyError::InvalidElement(format!(
                "kelly cap {cap} outside (0, 1]"
            )));
        }
        Ok(Self {
            base,
            fraction,
            cap,
        })
    }
}

#[cfg(feature = "research")]
impl Sizing for FractionalKelly {
    fn name(&self) -> &str {
        "fractional-kelly"
    }

    fn axis(&self) -> GateAxis {
        GateAxis::TaskValue
    }

    fn size(&self, signal: &SignalOutput, _facts: &PortfolioFacts) -> Qty {
        // Absence is not a zero: without observed estimates there is no edge
        // to price, and the composed strategy refuses the zero with a reason.
        let (Some(edge), Some(odds)) = (signal.edge, signal.odds) else {
            return Qty::zero();
        };
        if !edge.is_finite() || !odds.is_finite() || odds <= 0.0 {
            return Qty::zero();
        }
        // A literal-built policy past the validating constructor still fails
        // closed: NaN or out-of-band knobs size zero.
        if !(self.fraction > 0.0
            && self.fraction <= 1.0
            && self.fraction.is_finite()
            && self.cap > 0.0
            && self.cap <= 1.0
            && self.cap.is_finite())
        {
            return Qty::zero();
        }
        let f_star = edge / odds;
        // No edge (or a negative one) is no stake — never a short flip.
        if !(f_star > 0.0) {
            return Qty::zero();
        }
        let staked = f64::from(self.base) * self.cap.min(self.fraction * f_star);
        if !staked.is_finite() || staked <= 0.0 {
            return Qty::zero();
        }
        let Some(contracts) = Decimal::from_f64_retain(staked.floor()) else {
            return Qty::zero();
        };
        Qty::new(contracts)
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

    fn axis(&self) -> GateAxis {
        GateAxis::Correctness
    }

    fn size(&self, _signal: &SignalOutput, facts: &PortfolioFacts) -> Qty {
        if !bundle_is_arithmetical(facts) {
            return Qty::zero();
        }
        let Some(power) = facts.buying_power else {
            return Qty::zero();
        };
        size_from_fraction(
            self.fraction,
            power.as_decimal(),
            self.per_unit_price.as_decimal(),
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

    /// Whether this element can emit anything other than a hold. The exit
    /// pass is opt-in on this fact, so a strategy with no protective rule
    /// keeps every bar byte-identical.
    fn has_exits(&self) -> bool {
        true
    }

    /// Propose actions for a position at its mark, in firing order; empty
    /// to hold. An absent mark acts on nothing — absence is not a trigger,
    /// so a rule that needs a price refuses rather than inventing one.
    fn review(&self, position: &PositionView<'_>, mark: &MarkContext) -> Vec<LossAction>;
}

/// Loss management that never acts (hold every position).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HoldLoss;

impl LossMgmt for HoldLoss {
    fn name(&self) -> &str {
        "hold"
    }

    fn has_exits(&self) -> bool {
        false
    }

    fn review(&self, _position: &PositionView<'_>, _mark: &MarkContext) -> Vec<LossAction> {
        Vec::new()
    }
}

/// Whole-book unrealised at the mark: the signed per-unit distance from
/// entry, scaled to the held quantity. Long gains when the mark prints
/// above entry; short gains when it prints below. Pure price distance —
/// the view carries no fee line, so no fee is estimated.
fn unrealised_at(position: &PositionView<'_>, mark: Money) -> Money {
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

/// Shared level review for threshold rules (`StopLoss`, `ProfitTarget`):
/// an absent mark holds; otherwise fires one `kind` action when
/// `breached(pnl, threshold)` holds. Boundaries are strict (exactly on the
/// threshold holds) — the caller's closure owns the direction, this owns
/// the shape. `noun` is the reason word (`"stop"`/`"target"`), kept
/// separate from `kind` (`"stop"`/`"profit-target"`) because the reason
/// vocabulary differs from the attribution vocabulary (M5.2: both pinned
/// verbatim — user-visible, never normalized).
fn review_level(
    position: &PositionView<'_>,
    mark: &MarkContext,
    kind: &'static str,
    noun: &'static str,
    limit: Money,
    breached: impl FnOnce(Decimal, Decimal) -> bool,
) -> Vec<LossAction> {
    let Some(price) = priced(mark) else {
        return Vec::new();
    };
    let pnl = unrealised_at(position, price);
    if breached(pnl.as_decimal(), limit.as_decimal()) {
        vec![LossAction {
            kind: kind.to_owned(),
            reason: format!("unrealised {pnl} past {noun} {limit}"),
        }]
    } else {
        Vec::new()
    }
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

    fn review(&self, position: &PositionView<'_>, mark: &MarkContext) -> Vec<LossAction> {
        review_level(
            position,
            mark,
            "stop",
            "stop",
            self.max_loss,
            |pnl, limit| pnl < -limit,
        )
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

    fn review(&self, position: &PositionView<'_>, mark: &MarkContext) -> Vec<LossAction> {
        review_level(
            position,
            mark,
            "profit-target",
            "target",
            self.target,
            |pnl, limit| pnl > limit,
        )
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

    fn review(&self, position: &PositionView<'_>, mark: &MarkContext) -> Vec<LossAction> {
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

/// Assignment and ex-dividend protection: fires `"assignment-protection"`
/// once when a **short leg** sits inside the event window.
///
/// Two arms, one action. The **ex-div arm** fires when the underlying goes
/// ex-dividend within `days_before_ex_div` days before (or on) a short leg's
/// own expiry — both edges inclusive, measured on the calendar both sides
/// name. The **assignment arm** fires when the venue flags
/// [`PositionEvents::assignment_eligible`] and the guard was built with
/// `include_assignment_window`. At most one action per review, however many
/// legs sit inside.
///
/// The guard needs legs *and* events and refuses to act without them:
/// [`can_judge`](Self::can_judge) reports whether the position carries both,
/// so an unknown calendar reads as "cannot judge" — a distinguishable state,
/// never silence and never a default window. Like every rule, it also holds
/// on an absent mark: a dark chain prices no exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssignmentGuard {
    /// Days before a short leg's expiry an ex-div still trips the guard.
    pub days_before_ex_div: u32,
    /// Whether a venue-flagged assignment window trips the guard on its own.
    pub include_assignment_window: bool,
}

impl AssignmentGuard {
    /// Documented default ex-div window: two days.
    ///
    /// | window | coverage | verdict |
    /// |---|---|---|
    /// | 0 | ex-div day only | misses the day-before borrow squeeze |
    /// | 1 | −1d … 0 | misses weekend gaps (Friday review, Monday ex-div) |
    /// | **2** | −2d … 0 | **default**: covers a weekend plus the session before |
    /// | 5+ | a full week | noisy: fires on ex-divs days from expiry while theta still pays |
    pub const DEFAULT_DAYS_BEFORE_EX_DIV: u32 = 2;

    /// Build the guard over the ex-div window. Any window stands, including
    /// zero (ex-div day only); the assignment arm is opt-in per installation.
    #[must_use]
    pub const fn new(days_before_ex_div: u32, include_assignment_window: bool) -> Self {
        Self {
            days_before_ex_div,
            include_assignment_window,
        }
    }

    /// Whether the position carries the legs and the calendar this rule
    /// judges: `false` means "cannot judge", which is distinct from the
    /// empty review of a judged-safe book.
    #[must_use]
    pub fn can_judge(&self, position: &PositionView<'_>) -> bool {
        position.legs.is_some() && position.events.is_some()
    }

    /// Days from the ex-div date to the leg's expiry, when the leg outlives
    /// the event. `None` when the ex-div falls past expiry — a dead leg
    /// cannot be assigned.
    fn days_to_expiry(ex_dividend_on: NaiveDate, expiry: NaiveDate) -> Option<i64> {
        let days = (expiry - ex_dividend_on).num_days();
        (days >= 0).then_some(days)
    }
}

impl LossMgmt for AssignmentGuard {
    fn name(&self) -> &str {
        "assignment-protection"
    }

    fn review(&self, position: &PositionView<'_>, mark: &MarkContext) -> Vec<LossAction> {
        if priced(mark).is_none() {
            return Vec::new();
        }
        let (Some(legs), Some(events)) = (position.legs, position.events) else {
            return Vec::new();
        };
        // The venue flag is the most urgent arm: assignment may already be
        // in flight, so it precedes the date-specific ex-div arm.
        if self.include_assignment_window
            && events.assignment_eligible
            && legs.iter().any(|leg| leg.side == LegSide::Short)
        {
            return vec![LossAction {
                kind: "assignment-protection".to_owned(),
                reason: format!(
                    "assignment window flagged on {} with a short leg — exit before the desk is assigned",
                    position.underlying,
                ),
            }];
        }
        let Some(ex_dividend_on) = events.ex_dividend_on else {
            return Vec::new();
        };
        let window = i64::from(self.days_before_ex_div);
        let short_in_window = legs.iter().find(|leg| {
            leg.side == LegSide::Short
                && Self::days_to_expiry(ex_dividend_on, leg.expiry)
                    .is_some_and(|days| days <= window)
        });
        short_in_window
            .map(|leg| LossAction {
                kind: "assignment-protection".to_owned(),
                reason: format!(
                    "short leg {} ex-dividend on {ex_dividend_on} within {window} days of {} expiry",
                    leg.occ, leg.expiry,
                ),
            })
            .into_iter()
            .collect()
    }
}

/// A roll/adjustment draft: the rule's priced decision, not yet a proposal.
///
/// An `Adjustment` is a **proposal draft** — structure, legs, target DTE,
/// credit intent, and the economics the rule priced off the mark — never an
/// order intent and never a [`LossAction`]. The types are distinct on purpose:
/// a roll is not an exit, and folding it into loss management would either
/// bypass the roll permission or collapse two authorities into one. Identity
/// (`id`, strategy, creation time) is the runner's; everything economic is
/// the rule's, set when the rule reviewed the position.
///
/// The legs travel as one validated [`SpreadLegs`]: construction already
/// refused naked-short leg sets, so a draft can only ever name a defined-risk
/// package, and the venue fills it atomically or not at all.
#[derive(Debug, Clone, PartialEq)]
pub struct Adjustment {
    /// Structure shape the roll takes.
    pub structure: StructureKind,
    /// The package, as one multi-leg order — never leg by leg.
    pub legs: SpreadLegs,
    /// Days to expiry the roll targets.
    pub target_dte_days: u32,
    /// Credit intent: sell the near-the-money anchor.
    pub credit: bool,
    /// Underlying the roll keeps (copied from the reviewed position).
    pub underlying: String,
    /// Direction the roll keeps (copied from the reviewed position — a roll
    /// never flips).
    pub direction: Direction,
    /// Contracts per spread unit.
    pub quantity: Qty,
    /// Limit price per spread unit, priced off the mark.
    pub limit_price: Money,
    /// Worst-case loss if the new package goes to zero value.
    pub max_loss: Money,
    /// Committed notional for budget accounting.
    pub notional: Money,
    /// Signal-grade conviction the grant's signal tier judges, in `[0, 1]`.
    pub conviction: f64,
    /// Options-activity score, in `[0, 1]`.
    pub activity_score: f64,
}

impl Adjustment {
    /// Mint the roll proposal this draft describes: `side` is always
    /// [`Roll`](ProposalSide::Roll), so the grant's roll permission and every
    /// gate apply — **no new submission path**. Refuses a draft that cannot
    /// be priced (zero quantity, a score outside `[0, 1]`, a negative limit)
    /// rather than minting an order the venue cannot fill.
    pub fn propose(
        &self,
        id: &str,
        strategy: &str,
        created_at: DateTime<Utc>,
    ) -> Result<Proposal, StrategyError> {
        if self.quantity.as_decimal() <= Decimal::ZERO {
            return Err(StrategyError::InvalidElement(
                "a roll of nothing mints nothing".to_owned(),
            ));
        }
        if !(0.0..=1.0).contains(&self.conviction) {
            return Err(StrategyError::InvalidElement(format!(
                "roll conviction {} outside [0, 1]",
                self.conviction
            )));
        }
        if !(0.0..=1.0).contains(&self.activity_score) {
            return Err(StrategyError::InvalidElement(format!(
                "roll activity score {} outside [0, 1]",
                self.activity_score
            )));
        }
        if self.limit_price.as_decimal().is_sign_negative() {
            return Err(StrategyError::InvalidElement(format!(
                "roll limit price must not be negative: {}",
                self.limit_price
            )));
        }
        Ok(Proposal::new()
            .id(id.to_owned())
            .strategy(strategy.to_owned())
            .underlying(self.underlying.clone())
            .direction(self.direction)
            .side(ProposalSide::Roll)
            .structure(self.structure)
            .quantity(self.quantity)
            .limit_price(self.limit_price)
            .max_loss(self.max_loss)
            .notional(self.notional)
            .dte_days(self.target_dte_days)
            .conviction(self.conviction)
            .activity_score(self.activity_score)
            .maybe_legs(Some(self.legs.clone()))
            .state(ProposalState::Proposed)
            .created_at(created_at)
            .build())
    }
}

/// Defines how a position is reshaped rather than exited. Optional: defaults
/// to no rules, so a strategy without adjustments drafts nothing.
///
/// A rule that needs a price holds on an absent mark — like every rule, it
/// refuses to invent one. Drafts flow to the runner, which mints roll
/// proposals through the existing pipeline; the rule itself never submits.
pub trait AdjustRule: Send + Sync {
    /// Rule name for attribution (which rule drafted a roll). Defaults to
    /// `"custom"` so third-party rules compile unchanged; every shipped rule
    /// names itself.
    fn name(&self) -> &str {
        "custom"
    }

    /// Draft adjustments for a position at its mark; empty to hold the shape.
    fn review(&self, position: &PositionView<'_>, mark: &MarkContext) -> Vec<Adjustment>;
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

    fn has_exits(&self) -> bool {
        self.rules.iter().any(|rule| rule.has_exits())
    }

    fn review(&self, position: &PositionView<'_>, mark: &MarkContext) -> Vec<LossAction> {
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
    fn observe(&self, facts: &PortfolioFacts, positions: &[PositionView<'_>]) -> Vec<MonitorAlert>;
}

/// Monitor that never alerts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NullMonitor;

impl Monitor for NullMonitor {
    fn observe(
        &self,
        _facts: &PortfolioFacts,
        _positions: &[PositionView<'_>],
    ) -> Vec<MonitorAlert> {
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
    fn observe(
        &self,
        facts: &PortfolioFacts,
        _positions: &[PositionView<'_>],
    ) -> Vec<MonitorAlert> {
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
    adjust: Vec<Arc<dyn AdjustRule>>,
    monitor: Arc<dyn Monitor>,
    activity: Option<Arc<dyn OptionsActivity>>,
}

impl Clone for Strategy {
    /// Share the composed elements: every role is already reference-counted,
    /// so cloning shares the handles and duplicates nothing. Pool workers
    /// each take a clone rather than sharing one handle across `await`
    /// points they do not co-own.
    fn clone(&self) -> Self {
        Self {
            signal: Arc::clone(&self.signal),
            structure: Arc::clone(&self.structure),
            sizing: Arc::clone(&self.sizing),
            loss: Arc::clone(&self.loss),
            adjust: self.adjust.iter().map(Arc::clone).collect(),
            monitor: Arc::clone(&self.monitor),
            activity: self.activity.as_ref().map(Arc::clone),
        }
    }
}

impl Strategy {
    /// Compose a strategy around a signal; optionals take their defaults
    /// (explicit vertical, one fixed contract, hold, no adjustments, silence).
    #[must_use]
    pub fn with_signal(signal: Arc<dyn Signal>) -> Self {
        Self {
            signal,
            structure: Arc::new(ExplicitStructure {
                structure: StructureKind::Vertical,
            }),
            sizing: Arc::new(FixedContracts { contracts: 1 }),
            loss: Arc::new(HoldLoss),
            adjust: Vec::new(),
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
            // The assignment guard reads legs and events no existing producer
            // fills, so it stays silent on every current run — and a stop
            // still precedes it, so a stop still wins.
            Arc::new(AssignmentGuard::new(
                AssignmentGuard::DEFAULT_DAYS_BEFORE_EX_DIV,
                true,
            )),
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

    /// Install one adjustment rule. Additive: rules accumulate in
    /// installation order and review in that order; the default is empty, so
    /// a strategy without adjustments drafts nothing.
    #[must_use]
    pub fn with_adjust_rule(mut self, rule: Arc<dyn AdjustRule>) -> Self {
        self.adjust.push(rule);
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
        // Select first: the instrument prices one unit before any quantity
        // exists, so sizing never sees a quantity-scaled number. The request
        // carries one unit because selection prices per unit now; the sized
        // quantity lands below, in containment.
        let request = StructureRequest::new()
            .structure(input.structure)
            .dte_days(input.dte_days)
            .maybe_max_dte_days(input.max_dte_days)
            .quantity(Qty::new(Decimal::ONE))
            .fee_per_contract(input.fee_per_contract)
            .credit(input.credit)
            .limit_price(input.limit_price)
            .max_loss(input.max_loss)
            .build();
        let selection = self
            .structure
            .select_unit(market, &output, &request)
            .map_err(|refused| StrategyError::InvalidElement(refused.to_string()))?;
        // Size against the priced unit, then contain: the per-unit price the
        // caller priced is what bounds buying power, so a chain-derived
        // package is bounded by its own package price rather than the
        // underlying's bar price.
        let unobserved = PortfolioFacts::unobserved();
        let facts = input.caps.facts.unwrap_or(&unobserved);
        let sizing_ctx = SizingCtx {
            unit: &selection.unit,
            facts,
            equity: None,
        };
        let quantity = input.caps.contain(
            self.sizing.size_for(&output, &sizing_ctx),
            Some(selection.unit.price_per_unit),
        );
        // A zero-sized package refuses by name rather than proposing a
        // zero-contract trade: selection priced the instrument, sizing
        // declined it, and containment must not invent a trade.
        if quantity == Qty::zero() {
            return Err(StrategyError::InvalidElement(
                "quantity must be positive".to_owned(),
            ));
        }
        let capital = Money::new(selection.unit.max_loss_per_unit.as_decimal() * quantity.0);
        let (limit_price, max_loss, notional, legs, economics) = match selection.legs {
            Some(legs) => {
                let mut priced = selection.economics.ok_or_else(|| {
                    StrategyError::InvalidElement(
                        "chain-derived selection supplied no economics".to_owned(),
                    )
                })?;
                // Only the requirement scales: every other field is already
                // per-unit, so the rescaled economics read exactly as a
                // pricing at the sized quantity would.
                priced.capital_requirement = capital;
                (priced.mid_limit, capital, capital, Some(legs), Some(priced))
            }
            // The config arm states totals, not per-unit economics: the
            // caller priced one trade (its cap), so the totals pass through
            // verbatim instead of scaling with the sized quantity.
            None => (
                input.limit_price,
                input.max_loss,
                input.notional,
                None,
                None,
            ),
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

    /// Whether the installed loss element can emit a real exit. The replay
    /// exit pass is opt-in on this: a hold-only composition leaves every bar
    /// byte-identical.
    #[must_use]
    pub fn has_exits(&self) -> bool {
        self.loss.has_exits()
    }

    /// One exit-proposal body: the entry supplies identity, direction, and
    /// structure; the intent supplies quantity and resting price. Callers
    /// with their own proposal id use [`exit_proposal_named`](Self::exit_proposal_named).
    #[must_use]
    pub fn exit_proposal(
        &self,
        entry: &Proposal,
        intent: &crate::slots::exit::ExitIntent,
        touch: Money,
        at: DateTime<Utc>,
    ) -> Proposal {
        let id = format!("{}-exit-{}", entry.id, intent.kind);
        self.exit_proposal_named(entry, entry.quantity, intent, touch, at, &id)
    }

    /// The same exit body with an explicit proposal id (the runner keeps its
    /// deterministic per-bar id).
    #[must_use]
    pub fn exit_proposal_named(
        &self,
        entry: &Proposal,
        remaining: Qty,
        intent: &crate::slots::exit::ExitIntent,
        touch: Money,
        at: DateTime<Utc>,
        id: &str,
    ) -> Proposal {
        use crate::slots::exit::{ExitQuantity, RestingPrice};
        let quantity = match intent.quantity {
            ExitQuantity::All => remaining,
            ExitQuantity::Fraction(fraction) => {
                let fraction = fraction.clamp(0.0, 1.0);
                Qty::new(remaining.0 * Decimal::from_f64_retain(fraction).unwrap_or_default())
            }
            ExitQuantity::Units(units) => Qty::new(Decimal::from(units)),
        };
        let price = match intent.price {
            RestingPrice::Market => touch,
            RestingPrice::At(price) => {
                let marketable = if entry.direction == Direction::Long {
                    price.as_decimal() <= touch.as_decimal()
                } else {
                    price.as_decimal() >= touch.as_decimal()
                };
                if marketable {
                    price
                } else {
                    touch
                }
            }
        };
        Proposal::new()
            .id(id.to_owned())
            .strategy(entry.strategy.clone())
            .underlying(entry.underlying.clone())
            .direction(entry.direction)
            .side(ProposalSide::Exit)
            .structure(entry.structure)
            .quantity(quantity)
            .limit_price(price)
            .max_loss(Money::zero())
            .notional(Money::new(
                price.as_decimal() * quantity.0.max(Decimal::ZERO),
            ))
            .dte_days(0)
            .conviction(1.0)
            .activity_score(1.0)
            .created_at(at)
            .build()
    }

    /// Ask loss management for protective actions over a marked position.
    /// An absent mark acts on nothing — the rule, not the caller, refuses
    /// to invent a price.
    #[must_use]
    pub fn review_exits(&self, position: &PositionView<'_>, mark: &MarkContext) -> Vec<LossAction> {
        self.review_position(position, mark)
    }

    /// Ask loss management for protective actions over a marked position.
    /// An absent mark acts on nothing — the rule, not the caller, refuses
    /// to invent a price.
    #[must_use]
    pub fn review_position(
        &self,
        position: &PositionView<'_>,
        mark: &MarkContext,
    ) -> Vec<LossAction> {
        self.loss.review(position, mark)
    }

    /// Ask the adjustment rules for roll drafts over a marked position, in
    /// installation order; empty when no rule is installed or no rule moves.
    /// This path never yields a [`LossAction`] and the loss path never yields
    /// an [`Adjustment`] — the two authorities stay separate by type.
    #[must_use]
    pub fn review_adjustments(
        &self,
        position: &PositionView<'_>,
        mark: &MarkContext,
    ) -> Vec<Adjustment> {
        self.adjust
            .iter()
            .flat_map(|rule| rule.review(position, mark))
            .collect()
    }

    /// Ask the monitor for read-only observations over facts and positions.
    #[must_use]
    pub fn observe(
        &self,
        facts: &PortfolioFacts,
        positions: &[PositionView<'_>],
    ) -> Vec<MonitorAlert> {
        self.monitor.observe(facts, positions)
    }
}

#[cfg(test)]
#[path = "../tests/elements.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/volatility_sizing.rs"]
mod volatility_sizing_tests;

#[cfg(all(test, feature = "research"))]
#[path = "../tests/kelly_sizing.rs"]
mod kelly_sizing_tests;

#[cfg(test)]
#[path = "../tests/sizing_calibration.rs"]
mod sizing_calibration_tests;

#[cfg(test)]
#[path = "../tests/loss_calibration.rs"]
mod loss_calibration_tests;

#[cfg(test)]
#[path = "../tests/assignment_guard.rs"]
mod assignment_guard_tests;

#[cfg(test)]
#[path = "../tests/adjustment.rs"]
mod adjustment_tests;

#[cfg(test)]
#[path = "../tests/sizing_exit_pins.rs"]
mod sizing_exit_pins_tests;

#[cfg(test)]
#[path = "../tests/sizing_axis_pins.rs"]
mod sizing_axis_pins_tests;
