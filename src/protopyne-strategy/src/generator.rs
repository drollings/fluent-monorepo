//! RSRS-gated vertical generator and reconcile-narrowed rebalancer.
//!
//! One pipeline, two stages: [`Generator`] composes closed bars → the
//! kernels RSRS score → entry proposals (the same score ladder the backtest
//! reports, never a candle pattern), and [`rebalance`] splits proposals
//! into entries and exits under the reconciliation reading. Proposals live
//! in an explicitly sorted [`Vec`] by [`utility`]; the re-score map is the
//! only per-key state — no second registry.
//!
//! The ladder is thresholds on the standardized score only: at or above
//! `entry_threshold` emits at full quantity, at or above `hold_threshold`
//! emits at half quantity, anything lower — or no score at all — vetoes.
//! A missing score is absence of signal, never a default trade.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive as _;
use rust_decimal::Decimal;

use protopyne_types::{Bar, Money, Portfolio, PortfolioId, Qty};

use crate::elements::MonitorAlert;
use crate::proposal::{Direction, Proposal, ProposalSide, ProposalState, StructureKind};

/// RSRS lookbacks matching the backtest report: 18-bar slopes scored over a
/// 600-bar window. Shorter histories read `None` and veto fail-closed.
pub const REPORT_SLOPE_LOOKBACK: usize = 18;
/// Score window matching the backtest report.
pub const REPORT_SCORE_WINDOW: usize = 600;

/// Which rung of the ladder a score lands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rung {
    /// Emit at full quantity.
    Full,
    /// Emit at half quantity.
    Half,
    /// Emit nothing.
    Veto,
}

/// Score thresholds only: `score >= entry` is [`Rung::Full`],
/// `score >= hold` is [`Rung::Half`], anything lower — or no score — is
/// [`Rung::Veto`]. Pure over values; a missing score vetoes.
#[must_use]
pub fn ladder_rung(score: Option<f64>, entry: f64, hold: f64) -> Rung {
    match score {
        Some(value) if value >= entry => Rung::Full,
        Some(value) if value >= hold => Rung::Half,
        _ => Rung::Veto,
    }
}

/// Ranking weight: conviction times activity. Linear in conviction at fixed
/// activity, so re-sorts track signal strength and nothing else.
#[must_use]
pub fn utility(conviction: f64, activity_score: f64) -> f64 {
    conviction * activity_score
}

/// Latest finite RSRS score over `bars` (`None` when the history is shorter
/// than the window or carries no variance). Same kernels function and the
/// same last-finite reading the backtest report uses.
#[must_use]
pub fn rsrs_score(bars: &[Bar], slope_lookback: usize, score_window: usize) -> Option<f64> {
    let highs: Vec<f64> = bars
        .iter()
        .map(|bar| bar.h.to_f64().unwrap_or(f64::NAN))
        .collect();
    let lows: Vec<f64> = bars
        .iter()
        .map(|bar| bar.l.to_f64().unwrap_or(f64::NAN))
        .collect();
    protopyne_kernels::scores::rsrs(&highs, &lows, slope_lookback, score_window)
        .into_iter()
        .flatten()
        .next_back()
}

/// Caller-supplied economics for one generation attempt. Bars feed the
/// RSRS ladder; prices, loss, notional, and expiry come from the caller's
/// quote and structure math — the generator never invents them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GenerateInput<'a> {
    /// Strategy name for attribution.
    pub strategy: &'a str,
    /// Underlying symbol.
    pub underlying: &'a str,
    /// Closed bars, oldest first, feeding the RSRS ladder.
    pub bars: &'a [Bar],
    /// Full-rung quantity (contracts).
    pub quantity: u32,
    /// Limit price per contract.
    pub limit_price: Money,
    /// Worst-case loss if the position goes to zero value.
    pub max_loss: Money,
    /// Committed notional for budget accounting.
    pub notional: Money,
    /// Days to expiry of the front leg.
    pub dte_days: u32,
    /// Options-activity score in `[0, 1]` (no prototype feed; callers pass
    /// their read, `1.0` when they have none — documented, never derived).
    pub activity_score: f64,
    /// Proposal creation time.
    pub at: DateTime<Utc>,
}

/// RSRS-gated vertical generator. Proposals accumulate in an explicitly
/// sorted [`Vec`] (best utility first, ties by proposal ID); the re-score
/// map is the only per-key state.
pub struct Generator {
    slope_lookback: usize,
    score_window: usize,
    entry_threshold: f64,
    hold_threshold: f64,
    scores: HashMap<String, f64>,
    proposals: Vec<Proposal>,
}

/// Builder for [`Generator`]: ladder lookbacks and thresholds, defaulting
/// to the report's lookbacks and a 1.0/0.0 entry/hold ladder.
#[derive(Default)]
pub struct GeneratorBuilder {
    slope_lookback: Option<usize>,
    score_window: Option<usize>,
    entry_threshold: Option<f64>,
    hold_threshold: Option<f64>,
}

impl GeneratorBuilder {
    /// Slope lookback in bars.
    #[must_use]
    pub fn slope_lookback(mut self, slope_lookback: usize) -> Self {
        self.slope_lookback = Some(slope_lookback);
        self
    }

    /// Score window in bars.
    #[must_use]
    pub fn score_window(mut self, score_window: usize) -> Self {
        self.score_window = Some(score_window);
        self
    }

    /// Score at or above which generation emits at full quantity.
    #[must_use]
    pub fn entry_threshold(mut self, entry_threshold: f64) -> Self {
        self.entry_threshold = Some(entry_threshold);
        self
    }

    /// Score at or above which generation emits at half quantity.
    #[must_use]
    pub fn hold_threshold(mut self, hold_threshold: f64) -> Self {
        self.hold_threshold = Some(hold_threshold);
        self
    }

    /// Finish building.
    #[must_use]
    pub fn build(self) -> Generator {
        Generator {
            slope_lookback: self.slope_lookback.unwrap_or(REPORT_SLOPE_LOOKBACK),
            score_window: self.score_window.unwrap_or(REPORT_SCORE_WINDOW),
            entry_threshold: self.entry_threshold.unwrap_or(1.0),
            hold_threshold: self.hold_threshold.unwrap_or(0.0),
            scores: HashMap::new(),
            proposals: Vec::new(),
        }
    }
}

impl Generator {
    /// Start building a generator.
    #[must_use]
    pub fn builder() -> GeneratorBuilder {
        GeneratorBuilder::default()
    }

    /// Latest re-scored conviction by proposal ID (the only per-key state).
    #[must_use]
    pub fn scores(&self) -> &HashMap<String, f64> {
        &self.scores
    }

    /// Proposals best-utility-first.
    #[must_use]
    pub fn ranked(&self) -> &[Proposal] {
        &self.proposals
    }

    /// Score `input.bars` through the ladder and emit an entry proposal, or
    /// veto. Emitted proposals accumulate in rank order.
    pub fn propose(&mut self, input: GenerateInput<'_>) -> Option<Proposal> {
        let score = rsrs_score(input.bars, self.slope_lookback, self.score_window)?;
        let rung = ladder_rung(Some(score), self.entry_threshold, self.hold_threshold);
        let quantity = match rung {
            Rung::Full => input.quantity,
            Rung::Half => input.quantity / 2,
            Rung::Veto => return None,
        };
        if quantity == 0 {
            return None;
        }
        let span = (self.entry_threshold - self.hold_threshold).max(f64::EPSILON);
        let conviction = ((score - self.hold_threshold) / span).clamp(0.0, 1.0);
        let direction = if score >= 0.0 {
            Direction::Long
        } else {
            Direction::Short
        };
        let id = format!(
            "{}-{}-{}",
            input.strategy,
            input.underlying,
            input.bars.len()
        );
        let proposal = Proposal::new()
            .id(id.clone())
            .strategy(input.strategy.to_owned())
            .underlying(input.underlying.to_owned())
            .direction(direction)
            .side(ProposalSide::Entry)
            .structure(StructureKind::Vertical)
            .quantity(Qty::new(Decimal::from(quantity)))
            .limit_price(input.limit_price)
            .max_loss(input.max_loss)
            .notional(input.notional)
            .dte_days(input.dte_days)
            .conviction(conviction)
            .activity_score(input.activity_score)
            .state(ProposalState::Proposed)
            .created_at(input.at)
            .build();
        self.scores.insert(id, conviction);
        self.proposals.push(proposal.clone());
        self.sort_ranked();
        Some(proposal)
    }

    /// Re-score a ranked proposal by ID and re-sort. Unknown IDs are
    /// ignored: scores track ranked proposals, never stray keys.
    pub fn rescore(&mut self, id: &str, conviction: f64) {
        if let Some(proposal) = self.proposals.iter_mut().find(|p| p.id == id) {
            proposal.conviction = conviction;
            self.scores.insert(id.to_owned(), conviction);
            self.sort_ranked();
        }
    }

    fn sort_ranked(&mut self) {
        self.proposals.sort_by(|left, right| {
            utility(right.conviction, right.activity_score)
                .partial_cmp(&utility(left.conviction, left.activity_score))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.id.cmp(&right.id))
        });
    }
}

/// One attributed open exposure feeding portfolio derivation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exposure {
    /// Underlying symbol.
    pub underlying: String,
    /// Strategy holding the exposure.
    pub strategy: String,
}

/// Derive the portfolio for `underlying` from attributed exposures:
/// deterministic handle and name, sorted unique strategies, no accounts
/// (the prototype has no account feed). Same exposures derive the same
/// portfolio at any step, so a carried portfolio and a fresh derivation
/// agree by construction.
#[must_use]
pub fn portfolio_for(underlying: &str, exposures: &[Exposure]) -> Portfolio {
    let mut strategies: Vec<String> = exposures
        .iter()
        .filter(|exposure| exposure.underlying == underlying)
        .map(|exposure| exposure.strategy.clone())
        .collect();
    strategies.sort();
    strategies.dedup();
    Portfolio::new()
        .id(PortfolioId::new(format!("pf-{underlying}")))
        .name(underlying.to_owned())
        .strategies(strategies)
        .build()
}

/// How wide the rebalancer may operate. A reconcile mismatch narrows to
/// [`RebalanceMode::NoNewTrades`]: entries halt, exits stay open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebalanceMode {
    /// Entries and exits both flow.
    Full,
    /// Entries halt; exits stay open.
    NoNewTrades,
}

/// One rebalance pass: entries, exits, the mode they ran under, and banked
/// monitor alerts for out-of-band delivery.
#[derive(Debug, Clone, PartialEq)]
pub struct RebalanceOutcome {
    /// Mode this pass ran under.
    pub mode: RebalanceMode,
    /// Entry proposals admitted this pass.
    pub entries: Vec<Proposal>,
    /// Exit and roll proposals admitted this pass (always admitted).
    pub exits: Vec<Proposal>,
    /// Banked alerts (reconcile mismatch); drained out-of-band, in the same
    /// drain pattern as the venue reconcile gate.
    pub alerts: Vec<MonitorAlert>,
}

/// Split `proposals` into entries and exits under the reconciliation
/// reading. Callers pass the venue's `ReconcileGate` read as
/// `reconciliation_healthy` (the same bit that feeds
/// `RiskState::reconciliation_healthy`); on mismatch the pass narrows to
/// [`RebalanceMode::NoNewTrades`] with entries held, exits open, and one
/// banked monitor alert naming the portfolio.
#[must_use]
pub fn rebalance(
    proposals: Vec<Proposal>,
    reconciliation_healthy: bool,
    portfolio: &str,
) -> RebalanceOutcome {
    if reconciliation_healthy {
        let (entries, exits) = split_sides(proposals);
        return RebalanceOutcome {
            mode: RebalanceMode::Full,
            entries,
            exits,
            alerts: Vec::new(),
        };
    }
    let (_, exits) = split_sides(proposals);
    RebalanceOutcome {
        mode: RebalanceMode::NoNewTrades,
        entries: Vec::new(),
        exits,
        alerts: vec![MonitorAlert {
            portfolio: portfolio.to_owned(),
            message: "reconciliation mismatch: entries halted, exits stay open".to_owned(),
        }],
    }
}

fn split_sides(proposals: Vec<Proposal>) -> (Vec<Proposal>, Vec<Proposal>) {
    let mut entries = Vec::new();
    let mut exits = Vec::new();
    for proposal in proposals {
        if proposal.side == ProposalSide::Entry {
            entries.push(proposal);
        } else {
            exits.push(proposal);
        }
    }
    (entries, exits)
}

#[cfg(test)]
#[path = "../tests/generator.rs"]
mod tests;
