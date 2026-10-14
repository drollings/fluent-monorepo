//! Read-only views over `&dyn TerminalGateway`.
//!
//! The terminal renders view-models, never raw gateway DTOs and never the
//! trading path: provenance order follows the journal's sequence order —
//! sorted at build, never arrival order, with no graph in between —, a
//! halted trading state renders halted with no entry affordance, and pulls
//! are uncached reads of fresh gateway state (a repeat pull rebuilds an
//! equal bundle; a changed journal rebuilds a changed one).

use crate::views::{HealthModel, PortfolioModel, Provenance, ViewsBundle, load_bundle};
use protopyne_view::{HealthView, MockGateway, PortfolioView, TerminalGateway};

fn mock_gateway() -> MockGateway {
    MockGateway::new()
}

fn mock_health() -> HealthView {
    mock_gateway().health().expect("mock health reads")
}

fn journal_row(seq: i64, client: &str, kind: &str) -> protopyne_view::JournalRowView {
    protopyne_view::JournalRowView {
        seq,
        client_id: client.to_owned(),
        kind: kind.to_owned(),
        detail: "{}".to_owned(),
        at_ms: 1_700_000_000_000 + seq * 1_000,
    }
}

#[test]
fn provenance_ordering_matches_journal_order() {
    let gateway = mock_gateway();
    let rows = gateway.journal().expect("mock journal reads");
    assert_eq!(rows.len(), 2, "mock journal carries intent then fill");

    let provenance = Provenance::of(rows).expect("acyclic journal builds");
    assert_eq!(provenance.timeline(), vec![1, 2], "provenance follows journal sequence order");
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
fn out_of_order_arrival_still_renders_causal_order() {
    // The only input that distinguishes causal order from arrival order:
    // rows arrive fill-first across two interleaved clients, and the
    // timeline must still follow sequence numbers — with follow-ups
    // skipping the other client's rows.
    let arrival =
        vec![journal_row(3, "a", "fill"), journal_row(1, "a", "intent"), journal_row(2, "b", "intent")];
    let provenance = Provenance::of(arrival).expect("shuffled journal builds");
    assert_eq!(provenance.timeline(), vec![1, 2, 3], "causal order, never arrival order");
    assert_eq!(
        provenance.followed_by(1),
        vec![3],
        "a's fill follows a's intent across b's interleaved row"
    );
    assert!(provenance.followed_by(2).is_empty(), "b's chain ends at its intent");

    let bundle = ViewsBundle::of(
        &mock_health(),
        &mock_gateway().portfolio().expect("mock portfolio reads"),
        Vec::new(),
        vec![journal_row(3, "a", "fill"), journal_row(1, "a", "intent"), journal_row(2, "b", "intent")],
    )
    .expect("shuffled journal shapes");
    assert_eq!(
        bundle.journal.iter().map(|row| row.seq).collect::<Vec<_>>(),
        vec![1, 2, 3],
        "the bundle stores sequence order, not arrival order"
    );
    assert_eq!(bundle.provenance_order, vec![1, 2, 3], "order and journal agree");
}

#[test]
fn duplicate_sequence_numbers_refuse_without_a_substituted_order() {
    let gateway = mock_gateway();
    let mut rows = gateway.journal().expect("mock journal reads");
    rows.push(rows[0].clone());
    let refused = ViewsBundle::of(
        &mock_health(),
        &gateway.portfolio().expect("mock portfolio reads"),
        Vec::new(),
        rows,
    )
    .expect_err("duplicated sequence numbers refuse");
    assert!(
        refused.to_string().contains("causal order"),
        "the refusal names the violated order: {refused}"
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
/// rebuild a changed bundle, never serve the first state's.
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

    fn replay(&self) -> Result<protopyne_view::ReplayView, protopyne_view::GatewayError> {
        mock_gateway().replay()
    }

    fn preview_arm(
        &self,
        patch: serde_json::Value,
    ) -> Result<protopyne_view::WorstCase, protopyne_view::GatewayError> {
        mock_gateway().preview_arm(patch)
    }

    fn grant_tiers(
        &self,
    ) -> Result<std::collections::BTreeMap<String, String>, protopyne_view::GatewayError> {
        mock_gateway().grant_tiers()
    }

    fn arm(
        &self,
        grant: serde_json::Value,
    ) -> Result<protopyne_view::ArmResponse, protopyne_view::GatewayError> {
        mock_gateway().arm(grant)
    }
}

#[test]
fn identical_state_pulls_identical_bundles() {
    // The no-cache control: with nothing stored between pulls, equal
    // gateway state still shapes equal bundles — determinism lives in the
    // shaping, not in a stored copy.
    let gateway = mock_gateway();
    let first: ViewsBundle = load_bundle(&gateway).expect("first pull builds");
    let second: ViewsBundle = load_bundle(&gateway).expect("second pull builds");
    assert_eq!(first, second, "identical state pulls identical bundles");
}

#[test]
fn grown_journal_rebuilds_a_changed_bundle() {
    // The invalidation control: any journal append changes what the next
    // pull shapes — a new row can never read back as the old bundle.
    let gateway = mock_gateway();
    let first: ViewsBundle = load_bundle(&gateway).expect("first pull builds");
    assert_eq!(first.provenance_order, vec![1, 2], "mock journal carries intent then fill");

    let grown = ExtraJournal {
        extra: protopyne_view::JournalRowView {
            seq: 3,
            client_id: "mock-1".to_owned(),
            kind: "settled".to_owned(),
            detail: "{}".to_owned(),
            at_ms: 1_700_000_120_000,
        },
    };
    let third: ViewsBundle = load_bundle(&grown).expect("changed journal rebuilds");
    assert_eq!(
        third.provenance_order,
        vec![1, 2, 3],
        "the rebuilt timeline extends the journal order"
    );
    assert_eq!(third.journal.len(), 3, "the rebuilt bundle carries the new row");
    assert_ne!(first, third, "an append never reads back as the old bundle");
}
