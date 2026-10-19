//! Read-only views over `&dyn TerminalGateway`.
//!
//! The terminal renders view-models, never raw gateway DTOs and never the
//! trading path: provenance order follows the journal's sequence order —
//! sorted at build, never arrival order, with no graph in between —, a
//! halted trading state renders halted with no entry affordance, and pulls
//! are uncached reads of fresh gateway state (a repeat pull rebuilds an
//! equal bundle; a changed journal rebuilds a changed one).

use crate::views::{
    load_bundle, HealthModel, InboxAction, InboxModel, PortfolioModel, Provenance, TicketAction,
    TicketModel, ViewsBundle,
};
use protopyne_view::{HealthView, MockGateway, PortfolioView, ProposalView, TerminalGateway};

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
    assert_eq!(
        provenance.timeline(),
        vec![1, 2],
        "provenance follows journal sequence order"
    );
    assert_eq!(
        provenance.followed_by(1),
        vec![2],
        "the fill follows the intent"
    );
    assert!(
        provenance.followed_by(2).is_empty(),
        "nothing follows the fill"
    );
    assert!(
        provenance.followed_by(99).is_empty(),
        "unknown rows have no followers"
    );
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
    let arrival = vec![
        journal_row(3, "a", "fill"),
        journal_row(1, "a", "intent"),
        journal_row(2, "b", "intent"),
    ];
    let provenance = Provenance::of(arrival).expect("shuffled journal builds");
    assert_eq!(
        provenance.timeline(),
        vec![1, 2, 3],
        "causal order, never arrival order"
    );
    assert_eq!(
        provenance.followed_by(1),
        vec![3],
        "a's fill follows a's intent across b's interleaved row"
    );
    assert!(
        provenance.followed_by(2).is_empty(),
        "b's chain ends at its intent"
    );

    let bundle = ViewsBundle::of(
        &mock_health(),
        &mock_gateway().portfolio().expect("mock portfolio reads"),
        Vec::new(),
        vec![
            journal_row(3, "a", "fill"),
            journal_row(1, "a", "intent"),
            journal_row(2, "b", "intent"),
        ],
    )
    .expect("shuffled journal shapes");
    assert_eq!(
        bundle.journal.iter().map(|row| row.seq).collect::<Vec<_>>(),
        vec![1, 2, 3],
        "the bundle stores sequence order, not arrival order"
    );
    assert_eq!(
        bundle.provenance_order,
        vec![1, 2, 3],
        "order and journal agree"
    );
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
    assert!(
        !halted.entry_affordance(),
        "halted shows no entry affordance"
    );
    assert!(!halted.exit_affordance(), "Disabled routes no exits either");

    let exit_only = HealthModel::of(&HealthView {
        trading_state: "ExitOnly".to_owned(),
        allows_entries: false,
        allows_exits: true,
        ..mock_health()
    });
    assert!(exit_only.halted(), "ExitOnly is not the enabled state");
    assert!(
        !exit_only.entry_affordance(),
        "exit-only shows no entry affordance"
    );
    assert!(
        exit_only.exit_affordance(),
        "exit-only keeps the exit affordance"
    );

    let enabled = HealthModel::of(&mock_health());
    assert!(!enabled.halted(), "Enabled renders as live");
    assert!(
        enabled.entry_affordance(),
        "enabled keeps the entry affordance"
    );
    assert!(
        enabled.exit_affordance(),
        "enabled keeps the exit affordance"
    );
}

#[test]
fn portfolio_model_mirrors_the_dto() {
    let gateway = mock_gateway();
    let dto = gateway.portfolio().expect("mock portfolio reads");
    let model = PortfolioModel::of(&dto);
    assert_eq!(model.positions, dto.positions);
    assert_eq!(model.filled_orders, dto.filled_orders);
    assert_eq!(model.total_fees, dto.total_fees);
    assert_eq!(model.open_contracts, dto.open_contracts);
    assert_eq!(model.trading_state, dto.trading_state);
    assert_eq!(model.source, dto.source, "the source tag echoes the DTO");
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
    assert_eq!(
        first.provenance_order,
        vec![1, 2],
        "mock journal carries intent then fill"
    );

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
    assert_eq!(
        third.journal.len(),
        3,
        "the rebuilt bundle carries the new row"
    );
    assert_ne!(first, third, "an append never reads back as the old bundle");
}

fn inbox_row(id: &str, state: &str) -> ProposalView {
    ProposalView {
        id: id.to_owned(),
        strategy: "ribbon".to_owned(),
        underlying: "SPY".to_owned(),
        direction: "Long".to_owned(),
        structure: "Vertical".to_owned(),
        limit_price: protopyne_types::Money::from(100),
        conviction: 0.8,
        state: state.to_owned(),
    }
}

#[test]
fn only_waiting_rows_carry_an_affordance() {
    // The inbox split: `PendingHuman` rows await a human act; decided,
    // proposed, and in-flight rows settle — including tags the terminal
    // has never seen, which fail closed with no affordance.
    let inbox = InboxModel::of(vec![
        inbox_row("waiting", "PendingHuman"),
        inbox_row("fresh", "Proposed"),
        inbox_row("checked", "RiskChecked"),
        inbox_row("auto", "AutoApproved"),
        inbox_row("yes", "Approved"),
        inbox_row("no", "Rejected"),
        inbox_row("old", "Expired"),
        inbox_row("sent", "Submitted"),
        inbox_row("work", "Working"),
        inbox_row("fill", "Filled"),
        inbox_row("part", "Partial"),
        inbox_row("gone", "Cancelled"),
        inbox_row("future", "AwaitingQuantumReview"),
    ]);
    let actionable: Vec<&str> = inbox
        .actionable()
        .iter()
        .map(|row| row.id.as_str())
        .collect();
    assert_eq!(actionable, vec!["waiting"], "only the waiting row acts");
    assert_eq!(inbox.settled().len(), 12, "everything else settles");
    assert!(!inbox.is_empty(), "a waiting row is non-empty");
    assert!(InboxModel::of(Vec::new()).is_empty(), "no rows is empty");
    assert!(
        InboxModel::of(vec![inbox_row("yes", "Approved")]).is_empty(),
        "decided rows are empty"
    );
    assert!(InboxModel::is_actionable("PendingHuman"));
    assert!(
        !InboxModel::is_actionable("Approved"),
        "decided tags carry no affordance"
    );
    assert!(
        !InboxModel::is_actionable("AwaitingQuantumReview"),
        "unknown tags fail closed"
    );
}

#[test]
fn decided_states_survive_the_bundle_round_trip() {
    // The journaled states render: decided rows keep their tags through
    // the bundle, so what the journal projected is what the glass shows.
    let bundle = ViewsBundle::of(
        &mock_health(),
        &mock_gateway().portfolio().expect("mock portfolio reads"),
        vec![
            inbox_row("waiting", "PendingHuman"),
            inbox_row("yes", "Approved"),
        ],
        Vec::new(),
    )
    .expect("decided rows shape");
    let inbox = InboxModel::of(bundle.proposals);
    assert_eq!(inbox.actionable().len(), 1, "the waiting row still acts");
    assert_eq!(inbox.actionable()[0].state, "PendingHuman");
    assert_eq!(inbox.settled().len(), 1, "the decided row still settles");
    assert_eq!(inbox.settled()[0].state, "Approved");
}

/// A gateway whose booked read differs from its plain portfolio read.
struct SourceGateway;

impl TerminalGateway for SourceGateway {
    fn health(&self) -> Result<HealthView, protopyne_view::GatewayError> {
        mock_gateway().health()
    }

    fn proposals(&self) -> Result<Vec<protopyne_view::ProposalView>, protopyne_view::GatewayError> {
        mock_gateway().proposals()
    }

    fn portfolio(&self) -> Result<PortfolioView, protopyne_view::GatewayError> {
        Ok(source_portfolio("rehearsal", 1))
    }

    fn portfolio_booked(&self) -> Result<PortfolioView, protopyne_view::GatewayError> {
        Ok(source_portfolio("booked", 2))
    }

    fn journal(&self) -> Result<Vec<protopyne_view::JournalRowView>, protopyne_view::GatewayError> {
        mock_gateway().journal()
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

fn source_portfolio(source: &str, filled: u64) -> PortfolioView {
    PortfolioView {
        positions: Vec::new(),
        filled_orders: filled,
        total_fees: protopyne_types::Money::from(0),
        open_contracts: 0,
        trading_state: "Enabled".to_owned(),
        source: source.to_owned(),
    }
}

#[test]
fn load_bundle_prefers_the_booked_portfolio() {
    let bundle = load_bundle(&SourceGateway).expect("bundle builds");
    assert_eq!(
        bundle.portfolio.source, "booked",
        "the booked read wins over the plain portfolio read"
    );
    assert_eq!(bundle.portfolio.filled_orders, 2);
}

/// A gateway counting every write verb it serves.
#[derive(Default)]
struct CountingGateway {
    approvals: std::sync::atomic::AtomicUsize,
    rejections: std::sync::atomic::AtomicUsize,
    issues: std::sync::atomic::AtomicUsize,
    confirms: std::sync::atomic::AtomicUsize,
    imports: std::sync::atomic::AtomicUsize,
}

impl TerminalGateway for CountingGateway {
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
        mock_gateway().journal()
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

    fn approve(&self, _proposal_id: &str) -> Result<(), protopyne_view::GatewayError> {
        self.approvals
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    fn reject(
        &self,
        _proposal_id: &str,
        _reason: &str,
    ) -> Result<(), protopyne_view::GatewayError> {
        self.rejections
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    fn issue_ticket(&self, _ticket: serde_json::Value) -> Result<(), protopyne_view::GatewayError> {
        self.issues
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    fn confirm_ticket(
        &self,
        _ticket_id: &str,
        _fill: serde_json::Value,
    ) -> Result<(), protopyne_view::GatewayError> {
        self.confirms
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    fn import_tickets(
        &self,
        _snapshot: serde_json::Value,
    ) -> Result<(), protopyne_view::GatewayError> {
        self.imports
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn inbox_action_invokes_exactly_one_gateway_write() {
    let gateway = CountingGateway::default();
    InboxAction::approve("waiting")
        .apply(&gateway)
        .expect("approve served");
    assert_eq!(
        gateway.approvals.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "one approve click is one write"
    );
    assert_eq!(
        gateway.rejections.load(std::sync::atomic::Ordering::SeqCst),
        0
    );

    InboxAction::reject("waiting", "too much size")
        .apply(&gateway)
        .expect("reject served");
    assert_eq!(
        gateway.rejections.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "one reject click is one write"
    );
    assert_eq!(
        gateway.approvals.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a reject never also approves"
    );
}

#[test]
fn ticket_action_invokes_exactly_one_gateway_write() {
    let gateway = CountingGateway::default();
    TicketAction::issue(serde_json::json!({"client_id": "rh-1"}))
        .apply(&gateway)
        .expect("issue served");
    assert_eq!(
        gateway.issues.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "one issue click is one write"
    );
    TicketAction::confirm("rh-1", serde_json::json!({"quantity": 2}))
        .apply(&gateway)
        .expect("confirm served");
    assert_eq!(
        gateway.confirms.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "one confirm click is one write"
    );
    TicketAction::import(serde_json::json!([]))
        .apply(&gateway)
        .expect("import served");
    assert_eq!(
        gateway.imports.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "one import click is one write"
    );
    for (name, count) in [
        ("approvals", &gateway.approvals),
        ("rejections", &gateway.rejections),
    ] {
        assert_eq!(
            count.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a ticket action never touches {name}"
        );
    }
}

#[test]
fn ticket_model_pulls_waiting_and_drifts_from_the_mock() {
    // The mock's canned wait is the panel's empty-or-one shape: one row
    // waiting, no drift on record.
    let model = TicketModel::pull(&mock_gateway()).expect("mock pulls");
    assert_eq!(model.waiting().len(), 1);
    assert_eq!(model.waiting()[0].ticket_id, "mock-ticket-1");
    assert!(model.drifts().is_empty());
    assert!(!model.is_empty());
}
