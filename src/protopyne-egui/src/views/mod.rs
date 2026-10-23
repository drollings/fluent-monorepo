//! Read-only terminal views over `&dyn TerminalGateway`.
//!
//! Headless view-models the terminal renders: health/trading-state with its
//! affordances, the portfolio projection, and the proposal-provenance
//! timeline over journal rows. Pulling stays behind [`load_bundle`];
//! rendering never touches the gateway, and no view holds order authority
//! of its own.
//!
//! Pulls are deliberately uncached: the gateway offers no monotonic
//! producer revision (its state mutates through paths outside any
//! counter's reach, including an externally owned trading-state handle),
//! so any revision key would risk serving a stale trading display — and a
//! content key costs a full serialization per pull to save only trivial
//! shaping. Fresh locks on every pull is the honest price of fresh data.

pub mod broker;
pub mod health;
pub mod inbox;
pub mod journal;
pub mod portfolio;
pub mod tickets;

pub use broker::{paint_broker_panel, BrokerModel};
pub use health::HealthModel;
pub use inbox::{InboxAction, InboxModel};
pub use journal::Provenance;
pub use portfolio::{ConcentrationModel, PortfolioModel};
pub use tickets::{TicketAction, TicketModel};

use protopyne_view::{
    GatewayError, HealthView, JournalRowView, PortfolioView, ProposalView, TerminalGateway,
};

/// One pull of every gateway read, shaped for rendering.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewsBundle {
    /// Health projection with its affordances.
    pub health: HealthModel,
    /// Portfolio projection over the latest run.
    pub portfolio: PortfolioModel,
    /// Strategy inbox rows, newest last.
    pub proposals: Vec<ProposalView>,
    /// Journaled lifecycle events in sequence order.
    pub journal: Vec<JournalRowView>,
    /// Provenance timeline: sequence numbers in causal order.
    pub provenance_order: Vec<i64>,
}

impl ViewsBundle {
    /// Shape pulled DTOs; refuses a journal with no causal order (duplicate
    /// sequence numbers) with the misbehavior named, never a substituted
    /// order. The journal is stored in sequence order — the sorted rows the
    /// provenance was built from, not the gateway's arrival order.
    pub fn of(
        health: &HealthView,
        portfolio: &PortfolioView,
        proposals: Vec<ProposalView>,
        journal: Vec<JournalRowView>,
    ) -> Result<Self, GatewayError> {
        let provenance = Provenance::of(journal).map_err(|refused| {
            GatewayError::Malformed(format!("journal has no causal order: {refused}"))
        })?;
        let provenance_order = provenance.timeline();
        let journal = provenance.into_rows();
        Ok(Self {
            health: HealthModel::of(health),
            portfolio: PortfolioModel::of(portfolio),
            proposals,
            journal,
            provenance_order,
        })
    }
}

/// Pull every gateway read and shape the bundle. The booked portfolio is the
/// preferred source; a gateway with no venue book falls back to its own
/// portfolio read.
pub fn load_bundle(gateway: &dyn TerminalGateway) -> Result<ViewsBundle, GatewayError> {
    let portfolio = gateway
        .portfolio_booked()
        .or_else(|_| gateway.portfolio())?;
    ViewsBundle::of(
        &gateway.health()?,
        &portfolio,
        gateway.proposals()?,
        gateway.journal()?,
    )
}

#[cfg(test)]
#[path = "../../tests/views.rs"]
mod tests;
