//! Option-chain vocabulary: contracts, quotes, spreads, portfolios.
//!
//! These types describe *what* can be traded (one option contract, one
//! multi-leg spread, one named portfolio grouping) without deciding anything:
//! no venue calls, no clock reads, no order authority. Hot-path reads stay on
//! direct fields; [`Portfolio`] additionally derives `FieldAccess`/
//! `Describable` because portfolio membership is runtime configuration edited
//! through the control plane.
//!
//! Defined-risk rule: a spread must hold at least one long leg. A lone short
//! leg — or an all-short leg set — is a naked short with unbounded loss and
//! is refused with a reason, never clamped into something else.

use bon::Builder;
use chrono::{DateTime, NaiveDate, Utc};
use fluent_wvr::{Describable, FieldAccess};
use rust_decimal::prelude::ToPrimitive as _;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;

use crate::error::TypesError;
use crate::money::Money;

/// Opaque handle for one option contract. No arithmetic by construction:
///
/// ```text
/// let _: ContractId = PortfolioId::new("x"); // fails to compile
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContractId(String);

impl ContractId {
    /// Wrap an opaque contract identifier (typically an OCC symbol).
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Borrow the identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ContractId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Opaque handle for one named portfolio. Never interchangeable with
/// [`ContractId`]: the compiler rejects any mixing at every call site.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PortfolioId(String);

impl PortfolioId {
    /// Wrap an opaque portfolio identifier.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Borrow the identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PortfolioId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Call or put.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OptionKind {
    /// Right to buy the underlying at the strike.
    Call,
    /// Right to sell the underlying at the strike.
    Put,
}

impl fmt::Display for OptionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Call => f.write_str("call"),
            Self::Put => f.write_str("put"),
        }
    }
}

/// One listed option contract: underlying, expiry, strike, call-or-put.
#[derive(Debug, Clone, Builder, PartialEq, Eq, Serialize, Deserialize)]
#[builder(start_fn = new)]
pub struct OptionContract {
    /// Underlying symbol (1–6 characters, e.g. `SPY`).
    pub underlying: String,
    /// Expiry date of this contract.
    pub expiry: NaiveDate,
    /// Strike price per share.
    pub strike: Money,
    /// Call or put.
    pub kind: OptionKind,
}

impl crate::validate::Validate for OptionContract {
    type Err = TypesError;
    fn validate(&self) -> Result<(), Self::Err> {
        OptionContract::validate(self)
    }
}

impl OptionContract {
    /// Fail-closed structural check: nonempty underlying of at most 6
    /// characters (the OCC root width) and a strictly positive strike.
    /// Expiry is not compared against the clock — historical chains must
    /// validate identically to live ones.
    pub fn validate(&self) -> Result<(), TypesError> {
        if self.underlying.is_empty() || self.underlying.len() > 6 {
            return Err(TypesError::InvalidContract(format!(
                "underlying must be 1-6 characters, got '{}'",
                self.underlying
            )));
        }
        if self.strike.as_decimal() <= Decimal::ZERO {
            return Err(TypesError::InvalidContract(format!(
                "strike must be positive, got {}",
                self.strike
            )));
        }
        Ok(())
    }

    /// OCC-style option symbol: root left-justified to 6 characters, then
    /// `YYMMDD`, then `C`/`P`, then strike in thousandths zero-padded to 8
    /// digits. E.g. SPY 2026-01-16 $600 call → `SPY   260116C00600000`.
    /// Pure formatting over stored fields; validation stays in [`validate`](Self::validate).
    #[must_use]
    pub fn occ_symbol(&self) -> String {
        let root = format!("{:<6}", self.underlying);
        let date = self.expiry.format("%y%m%d").to_string();
        let kind = match self.kind {
            OptionKind::Call => 'C',
            OptionKind::Put => 'P',
        };
        let mills = (self.strike.as_decimal() * Decimal::from(1_000)).round();
        let strike = mills.to_u64().unwrap_or(0);
        format!("{root}{date}{kind}{strike:08}")
    }

    /// Opaque handle for this contract, derived from its OCC symbol.
    #[must_use]
    pub fn contract_id(&self) -> ContractId {
        ContractId::new(self.occ_symbol())
    }
}

/// Model greeks for one contract: instantaneous sensitivities, not money.
/// Plain `f64` by design — greeks never enter money math directly.
#[derive(Debug, Clone, Copy, Builder, PartialEq, Serialize, Deserialize)]
#[builder(start_fn = new)]
pub struct Greeks {
    /// Change in contract value per $1 move in the underlying.
    #[builder(default)]
    pub delta: f64,
    /// Change in delta per $1 move in the underlying.
    #[builder(default)]
    pub gamma: f64,
    /// Change in contract value per day of time decay.
    #[builder(default)]
    pub theta: f64,
    /// Change in contract value per point of implied volatility.
    #[builder(default)]
    pub vega: f64,
}

/// One point-in-time quote for one option contract.
#[derive(Debug, Clone, Builder, PartialEq, Serialize, Deserialize)]
#[builder(start_fn = new)]
pub struct ChainQuote {
    /// Contract this quote is for.
    pub contract: OptionContract,
    /// Best bid per share.
    pub bid: Money,
    /// Best ask per share.
    pub ask: Money,
    /// Contracts bid at [`bid`](Self::bid).
    #[builder(default)]
    pub bid_size: u32,
    /// Contracts offered at [`ask`](Self::ask).
    #[builder(default)]
    pub ask_size: u32,
    /// Open interest in contracts.
    #[builder(default)]
    pub open_interest: u64,
    /// Implied volatility, if the feed provides one.
    pub iv: Option<f64>,
    /// Greeks, if the feed provides them.
    pub greeks: Option<Greeks>,
    /// When this quote was observed.
    pub timestamp: DateTime<Utc>,
}

impl crate::validate::Validate for ChainQuote {
    type Err = TypesError;
    fn validate(&self) -> Result<(), Self::Err> {
        ChainQuote::validate(self)
    }
}

impl ChainQuote {
    /// Fail-closed structural check: non-negative bid, strictly positive ask.
    /// A crossed quote (`bid > ask`) is *not* refused here — it is market
    /// data the fill policy must reject with per-leg reasons, so the policy
    /// tests need to be able to construct one. See [`is_crossed`](Self::is_crossed).
    pub fn validate(&self) -> Result<(), TypesError> {
        self.contract.validate()?;
        if self.bid.as_decimal() < Decimal::ZERO {
            return Err(TypesError::InvalidContract(format!(
                "bid must be non-negative, got {}",
                self.bid
            )));
        }
        if self.ask.as_decimal() <= Decimal::ZERO {
            return Err(TypesError::InvalidContract(format!(
                "ask must be positive, got {}",
                self.ask
            )));
        }
        Ok(())
    }

    /// `true` when the market is crossed (`bid > ask`): no valid mid exists
    /// and no fill policy may touch this quote.
    #[must_use]
    pub fn is_crossed(&self) -> bool {
        self.bid.as_decimal() > self.ask.as_decimal()
    }

    /// Ask minus bid. Negative when crossed — callers must check
    /// [`is_crossed`](Self::is_crossed) first.
    #[must_use]
    pub fn spread_width(&self) -> Money {
        Money::new(self.ask.as_decimal() - self.bid.as_decimal())
    }

    /// Mid price, or `None` when crossed (there is no valid mid on a crossed
    /// market — fail closed rather than averaging a bad book).
    #[must_use]
    pub fn midpoint(&self) -> Option<Money> {
        if self.is_crossed() {
            return None;
        }
        Some(Money::new(
            (self.bid.as_decimal() + self.ask.as_decimal()) / Decimal::from(2),
        ))
    }
}

/// Which side of a spread one leg sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LegSide {
    /// Long this contract.
    Long,
    /// Short this contract.
    Short,
}

impl fmt::Display for LegSide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Long => f.write_str("long"),
            Self::Short => f.write_str("short"),
        }
    }
}

/// One leg of a spread: a contract, a side, and a share ratio.
#[derive(Debug, Clone, Builder, PartialEq, Eq, Serialize, Deserialize)]
#[builder(start_fn = new)]
pub struct SpreadLeg {
    /// Contract for this leg.
    pub contract: OptionContract,
    /// Long or short.
    pub side: LegSide,
    /// Contracts of this leg per spread unit (1 for a plain vertical).
    #[builder(default = 1)]
    pub ratio: u32,
}

impl crate::validate::Validate for SpreadLeg {
    type Err = TypesError;
    fn validate(&self) -> Result<(), Self::Err> {
        SpreadLeg::validate(self)
    }
}

impl SpreadLeg {
    /// Fail-closed check: the contract must validate and the ratio must be
    /// at least 1 (a zero-ratio leg contributes nothing and hides a broken
    /// structure definition).
    pub fn validate(&self) -> Result<(), TypesError> {
        self.contract.validate()?;
        if self.ratio < 1 {
            return Err(TypesError::InvalidSpread(format!(
                "leg ratio must be at least 1, got {}",
                self.ratio
            )));
        }
        Ok(())
    }
}

/// Maximum legs in one atomic spread (condor shape).
pub const MAX_SPREAD_LEGS: usize = 4;

/// One atomic spread: 1–4 legs that submit as a single multi-leg order,
/// never leg by leg. Construction is validated — see [`try_new`](Self::try_new).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpreadLegs {
    legs: Vec<SpreadLeg>,
}

impl SpreadLegs {
    /// Validated constructor. Refuses, with reasons and never by clamping:
    ///
    /// - empty leg sets and sets longer than [`MAX_SPREAD_LEGS`];
    /// - any leg whose own [`validate`](SpreadLeg::validate) fails;
    /// - all-short leg sets: a lone short leg (or several) with no long leg
    ///   is a naked short with unbounded loss, outside the defined-risk
    ///   envelope.
    pub fn try_new(legs: Vec<SpreadLeg>) -> Result<Self, TypesError> {
        let candidate = Self { legs };
        candidate.validate()?;
        Ok(candidate)
    }

    /// Fail-closed structural check shared by construction and any future
    /// deserialization boundary.
    pub fn validate(&self) -> Result<(), TypesError> {
        if self.legs.is_empty() {
            return Err(TypesError::InvalidSpread(
                "spread must hold at least 1 leg".to_owned(),
            ));
        }
        if self.legs.len() > MAX_SPREAD_LEGS {
            return Err(TypesError::InvalidSpread(format!(
                "spread holds at most {} legs, got {}",
                MAX_SPREAD_LEGS,
                self.legs.len()
            )));
        }
        for leg in &self.legs {
            leg.validate()?;
        }
        if !self.legs.iter().any(|leg| leg.side == LegSide::Long) {
            return Err(TypesError::InvalidSpread(
                "naked short: a spread with no long leg has unbounded loss and \
                 is outside the defined-risk envelope"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    /// Borrow the legs in order.
    #[must_use]
    pub fn legs(&self) -> &[SpreadLeg] {
        &self.legs
    }

    /// Number of legs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.legs.len()
    }

    /// `true` when there are no legs (only observable on values that
    /// bypassed [`try_new`](Self::try_new), e.g. deserialized rows — those
    /// still fail [`validate`](Self::validate)).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.legs.is_empty()
    }
}

/// Named grouping of strategies and accounts: a tastytrade account, a
/// manually confirmed ticket set, a paper portfolio, or a backtest run.
/// Membership is runtime configuration (hence `FieldAccess`/`Describable`);
/// per-spread reads stay on direct fields.
#[derive(
    Debug,
    Clone,
    bon::Builder,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    FieldAccess,
    Describable,
)]
#[builder(start_fn = new)]
pub struct Portfolio {
    /// Opaque portfolio handle. Skipped from runtime field access: handles
    /// are minted at registration, never edited by name.
    #[field(skip)]
    pub id: PortfolioId,
    /// Human-readable portfolio name.
    #[field(desc = "Human-readable portfolio name")]
    pub name: String,
    /// Strategy names trading in this portfolio.
    #[field(skip)]
    #[builder(default)]
    pub strategies: Vec<String>,
    /// Account identifiers grouped in this portfolio.
    #[field(skip)]
    #[builder(default)]
    pub accounts: Vec<String>,
}

impl crate::validate::Validate for SpreadLegs {
    type Err = TypesError;
    fn validate(&self) -> Result<(), Self::Err> {
        SpreadLegs::validate(self)
    }
}

impl crate::validate::Validate for Portfolio {
    type Err = TypesError;
    fn validate(&self) -> Result<(), Self::Err> {
        Portfolio::validate(self)
    }
}

impl Portfolio {
    /// Fail-closed check: the name must be nonempty (an unnamed portfolio
    /// cannot appear in attribution or the terminal contract).
    pub fn validate(&self) -> Result<(), TypesError> {
        if self.name.trim().is_empty() {
            return Err(TypesError::InvalidPortfolio(
                "portfolio name must be nonempty".to_owned(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../tests/chain.rs"]
mod tests;
