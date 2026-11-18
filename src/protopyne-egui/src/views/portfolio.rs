//! Portfolio view-model: a thin read-only projection of one
//! [`PortfolioView`](protopyne_view::PortfolioView) read.
//!
//! Amounts stay exact (`Money` formats through `Display`; no `f64` ever
//! appears here, so the display-purity fence has nothing to mark). The model
//! adds no numbers of its own — it echoes the DTO and names the empty
//! portfolio so the terminal can render it without re-deriving the check.

use protopyne_types::Money;
use protopyne_view::{ConcentrationReport, ConcentrationShare, PortfolioView, PositionView};

/// Read-only portfolio projection over the latest run.
#[derive(Debug, Clone, PartialEq)]
pub struct PortfolioModel {
    /// Net positions by underlying.
    pub positions: Vec<PositionView>,
    /// Orders filled in the latest run.
    pub filled_orders: u64,
    /// Commission paid in the latest run.
    pub total_fees: Money,
    /// Net signed contracts across positions.
    pub open_contracts: i64,
    /// Trading state when the view was published, as the stable tag.
    pub trading_state: String,
    /// Which source produced the view (`booked` or `rehearsal`).
    pub source: String,
}

impl PortfolioModel {
    /// Project one portfolio read; infallible — every field echoes the DTO.
    #[must_use]
    pub fn of(view: &PortfolioView) -> Self {
        Self {
            positions: view.positions.clone(),
            filled_orders: view.filled_orders,
            total_fees: view.total_fees,
            open_contracts: view.open_contracts,
            trading_state: view.trading_state.clone(),
            source: view.source.clone(),
        }
    }

    /// Whether there is nothing to show: no positions and no fills.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty() && self.filled_orders == 0
    }
}

/// Concentration table: a read-only echo of one
/// [`ConcentrationReport`](protopyne_view::ConcentrationReport) DTO. The
/// model adds no numbers of its own — it echoes the report's rows in the
/// DTO's order, so the paint edge never computes a share.
#[derive(Debug, Clone, PartialEq)]
pub struct ConcentrationModel {
    /// Open-notional shares by underlying symbol.
    pub by_underlying: Vec<ConcentrationShare>,
    /// Open-notional shares by expiry.
    pub by_expiry: Vec<ConcentrationShare>,
    /// Open-notional shares by event window.
    pub by_event: Vec<ConcentrationShare>,
    /// Total open notional the shares partition.
    pub total_notional: Money,
}

impl ConcentrationModel {
    /// Project one concentration report; infallible — every field echoes
    /// the DTO.
    #[must_use]
    pub fn of(report: &ConcentrationReport) -> Self {
        Self {
            by_underlying: report.by_underlying.clone(),
            by_expiry: report.by_expiry.clone(),
            by_event: report.by_event.clone(),
            total_notional: report.total_notional,
        }
    }

    /// Whether there is nothing to show: no buckets in any dimension.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_underlying.is_empty() && self.by_expiry.is_empty() && self.by_event.is_empty()
    }
}
