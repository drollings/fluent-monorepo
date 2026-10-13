//! Read-only terminal views over `&dyn TerminalGateway`.
//!
//! Headless view-models the terminal renders: health/trading-state with its
//! affordances, the portfolio projection, and the proposal-provenance
//! timeline over journal rows. Pulling stays behind [`load_bundle`] (or the
//! cached [`ViewCache::load`]); rendering never touches the gateway, and no
//! view holds order authority of its own.

pub mod cache;
pub mod health;
pub mod journal;
pub mod portfolio;

pub use cache::{DEFAULT_VIEW_CACHE_CAPACITY, ViewCache};
pub use health::HealthModel;
pub use journal::Provenance;
pub use portfolio::PortfolioModel;

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
    /// Shape pulled DTOs; refuses a journal the provenance graph cannot
    /// order (duplicate sequence numbers) with the misbehavior named, never
    /// a substituted order.
    pub fn of(
        health: &HealthView,
        portfolio: &PortfolioView,
        proposals: Vec<ProposalView>,
        journal: Vec<JournalRowView>,
    ) -> Result<Self, GatewayError> {
        let provenance_order = Provenance::of(&journal)
            .map_err(|refused| {
                GatewayError::Malformed(format!("journal has no causal order: {refused}"))
            })?
            .timeline()
            .map_err(|refused| {
                GatewayError::Malformed(format!("journal has no causal order: {refused}"))
            })?;
        Ok(Self {
            health: HealthModel::of(health),
            portfolio: PortfolioModel::of(portfolio),
            proposals,
            journal,
            provenance_order,
        })
    }
}

/// Pull every gateway read and shape the bundle (uncached; prefer
/// [`ViewCache::load`] on a repeating render path).
pub fn load_bundle(gateway: &dyn TerminalGateway) -> Result<ViewsBundle, GatewayError> {
    ViewsBundle::of(
        &gateway.health()?,
        &gateway.portfolio()?,
        gateway.proposals()?,
        gateway.journal()?,
    )
}

#[cfg(test)]
#[path = "../../tests/views.rs"]
mod tests;
