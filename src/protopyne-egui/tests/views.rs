//! Read-only views over `&dyn TerminalGateway`.
//!
//! The terminal renders view-models, never raw gateway DTOs and never the
//! trading path: provenance order follows the journal's sequence order
//! through the shared dependency graph (no hand-rolled walk), a halted
//! trading state renders halted with no entry affordance, and derived view
//! state rides the shared bounded cache (a repeat pull is a hit, a changed
//! journal rebuilds).

use crate::views::{HealthModel, PortfolioModel, Provenance, ViewCache, ViewsBundle, load_bundle};
use protopyne_view::{HealthView, MockGateway, PortfolioView, TerminalGateway};

fn mock_gateway() -> MockGateway {
    MockGateway::new()
}

fn mock_health() -> HealthView {
    mock_gateway().health().expect("mock health reads")
}

#[test]
fn provenance_ordering_matches_journal_order() {
    let gateway = mock_gateway();
    let rows = gateway.journal().expect("mock journal reads");
    assert_eq!(rows.len(), 2, "mock journal carries intent then fill");

    let provenance = Provenance::of(&rows).expect("acyclic journal builds");
    let order = provenance.timeline().expect("timeline orders");
    assert_eq!(order, vec![1, 2], "provenance follows journal sequence order");
    assert_eq!(provenance.followed_by(1), vec![2], "the fill follows the intent");
    assert!(provenance.followed_by(2).is_empty(), "nothing follows the fill");
    assert!(provenance.followed_by(99).is_empty(), "unknown rows have no followers");
    assert_eq!(
        provenance.row(1).expect("row 1 exists").kind,
        "intent",
        "rows stay addressable by sequence number"
    );
}

#[test]
fn halted_trading_state_renders_halted() {
    let halted = HealthModel::of(&HealthView {
        trading_state: "Disabled".to_owned(),
        allows_entries: false,
        allows_exits: false,
        ..mock_health()
    });
    assert!(halted.halted(), "Disabled renders as halted");
    assert!(!halted.entry_affordance(), "halted shows no entry affordance");
    assert!(!halted.exit_affordance(), "Disabled routes no exits either");

    let exit_only = HealthModel::of(&HealthView {
        trading_state: "ExitOnly".to_owned(),
        allows_entries: false,
        allows_exits: true,
        ..mock_health()
    });
    assert!(exit_only.halted(), "ExitOnly is not the enabled state");
    assert!(!exit_only.entry_affordance(), "exit-only shows no entry affordance");
    assert!(exit_only.exit_affordance(), "exit-only keeps the exit affordance");

    let enabled = HealthModel::of(&mock_health());
    assert!(!enabled.halted(), "Enabled renders as live");
    assert!(enabled.entry_affordance(), "enabled keeps the entry affordance");
    assert!(enabled.exit_affordance(), "enabled keeps the exit affordance");
}

#[test]
fn portfolio_model_mirrors_the_dto() {
    let gateway = mock_gateway();
    let dto = gateway.portfolio().expect("mock portfolio reads");
    let model = PortfolioModel::of(&dto);
    assert_eq!(model.positions, dto.positions);
    assert_eq!(model.filled_orders, dto.filled_orders);
    assert_eq!(model.open_contracts, dto.open_contracts);
    assert_eq!(model.trading_state, dto.trading_state);
    assert!(!model.is_empty(), "the canned position is not empty");

    let empty = PortfolioModel::of(&PortfolioView::default());
    assert!(empty.is_empty(), "no positions and no fills is empty");
}

/// A gateway whose journal grows by one row: the second distinct state must
/// rebuild, not serve the first state's bundle.
struct ExtraJournal {
    extra: protopyne_view::JournalRowView,
}

impl TerminalGateway for ExtraJournal {
    fn health(&self) -> Result<HealthView, protopyne_view::GatewayError> {
        mock_gateway().health()
    }

    fn proposals(&self) -> Result<Vec<protopyne_view::ProposalView>, protopyne_view::GatewayError> {
        mock_gateway().proposals()
    }

    fn portfolio(&self) -> Result<PortfolioView, protopyne_view::GatewayError> {
        mock_gateway().portfolio()
    }

    fn journal(&self) -> Result<Vec<protopyne_view::JournalRowView>, protopyne_view::GatewayError> {
        let mut rows = mock_gateway().journal()?;
        rows.push(self.extra.clone());
        Ok(rows)
    }

    fn chart_bars(&self) -> Result<protopyne_view::ChartBars, protopyne_view::GatewayError> {
        mock_gateway().chart_bars()
    }

    fn chart_overlays(
        &self,
        kind: &str,
        period: i64,
    ) -> Result<protopyne_view::OverlaySeries, protopyne_view::GatewayError> {
        mock_gateway().chart_overlays(kind, period)
    }

    fn arm(
        &self,
        grant: serde_json::Value,
    ) -> Result<protopyne_view::ArmResponse, protopyne_view::GatewayError> {
        mock_gateway().arm(grant)
    }
}

#[test]
fn view_cache_hits_identical_state_rebuilds_changed() {
    let gateway = mock_gateway();
    let cache = ViewCache::new();
    assert!(cache.is_empty(), "a fresh cache holds no view state");

    let first: ViewsBundle =
        cache.load(&gateway).expect("first pull builds the bundle");
    assert_eq!(cache.len(), 1, "one distinct state cached");
    let second: ViewsBundle =
        cache.load(&gateway).expect("second pull hits the same state");
    assert_eq!(cache.len(), 1, "a repeat pull is a hit, not a second entry");
    assert_eq!(first.provenance_order, second.provenance_order);
    assert_eq!(first.health, second.health);

    let grown = ExtraJournal {
        extra: protopyne_view::JournalRowView {
            seq: 3,
            client_id: "mock-1".to_owned(),
            kind: "settled".to_owned(),
            detail: "{}".to_owned(),
            at_ms: 1_700_000_120_000,
        },
    };
    let third: ViewsBundle = cache.load(&grown).expect("changed journal rebuilds");
    assert_eq!(cache.len(), 2, "a new state is a second entry");
    assert_eq!(
        third.provenance_order,
        vec![1, 2, 3],
        "the rebuilt timeline extends the journal order"
    );
    assert_eq!(
        load_bundle(&gateway).expect("direct load builds").provenance_order,
        vec![1, 2],
        "the uncached loader agrees with the cache on the same state"
    );
}
