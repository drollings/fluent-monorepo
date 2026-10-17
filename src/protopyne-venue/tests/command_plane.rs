//! Live command plane wiring: journaled operator commands, bar gating, and
//! close-all exactly-once.
//!
//! Protocol: operator commands (halt, resume, close-all, cancel-all) enter
//! as journaled `JournalEvent` rows through `CommandUnit` under the
//! `VenueStorage` permit — never by direct mutation — so replay rebuilds
//! the trading state. Every bar consults the shared `StateHandle`: halted
//! bars refuse entries with a reason while exits still flow. Close-all
//! intents travel through `safe_submit` (retries are query-by-ID lookups);
//! cancel-all sweeps travel through the registry `cancel`.
//!
//! Enforcement is **(correctness)**-shaped (refuse with reason, fail
//! closed): operator intent is an authority signal, never a merit judgment,
//! and nothing here prices a trade.

use super::*;
use crate::journal::{Journal, VenueStorage};
use crate::order::{OrderIntent, OrderSide};
use crate::paper::{PaperConfig, PaperVenue};
use crate::quote::Quote;
use crate::registry::VenueRegistry;
use crate::session::{SessionConfig, SubmitJob, TradingSession};
use crate::venue::Venue;
use chrono::{TimeZone, Utc};
use protopyne_types::Money;
use rust_decimal::Decimal;
use std::sync::Arc;

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 5).unwrap()
}

fn quote() -> Quote {
    Quote::new()
        .bid(money("600.00"))
        .ask(money("600.10"))
        .bid_size(50)
        .ask_size(50)
        .ts(Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap())
        .build()
}

fn intent(id: &str, underlying: &str) -> OrderIntent {
    OrderIntent::new()
        .client_id(id.to_owned())
        .proposal_id("prop-1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying(underlying.to_owned())
        .side(OrderSide::Buy)
        .quantity(2)
        .limit_price(money("600.50"))
        .build()
}

fn job(journal: &Arc<Journal>, id: &str, underlying: &str, is_exit: bool) -> SubmitJob {
    let proposal_id = format!("prop-{id}");
    let mut order = intent(id, underlying);
    order.proposal_id = proposal_id.clone();
    SubmitJob {
        intent: order,
        quote: quote(),
        at: now(),
        is_exit,
        approved: approved_for_entry(&proposal_id),
        journal: Arc::clone(journal),
    }
}

/// Risk approval minted for the job's entry proposal, matching its intent —
/// mirroring the close-all helper below for the exit-free submission tests.
fn approved_for_entry(proposal_id: &str) -> protopyne_strategy::RiskApproved {
    let engine = protopyne_strategy::RiskEngine::new(protopyne_strategy::RiskLimits::default());
    let proposal = protopyne_strategy::Proposal::new()
        .id(proposal_id.to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .direction(protopyne_strategy::Direction::Long)
        .side(protopyne_strategy::ProposalSide::Entry)
        .structure(protopyne_strategy::StructureKind::Vertical)
        .quantity(protopyne_types::Qty::new(Decimal::from(2)))
        .limit_price(money("600.50"))
        .max_loss(money("300"))
        .notional(money("1_000"))
        .dte_days(30)
        .conviction(0.8)
        .activity_score(0.7)
        .created_at(now())
        .build();
    let state = protopyne_strategy::RiskState {
        quote_age_secs: 5,
        clock_skew_secs: 1,
        reconciliation_healthy: true,
        is_limit_order: true,
        is_spread: true,
    };
    engine.check(&proposal, &state).expect("healthy entry")
}

fn paper() -> Arc<PaperVenue> {
    Arc::new(PaperVenue::new(PaperConfig::new().build()))
}

fn registry_over(paper: &Arc<PaperVenue>) -> Arc<VenueRegistry> {
    let paper: Arc<dyn Venue> = Arc::clone(paper) as Arc<dyn Venue>;
    Arc::new(VenueRegistry::single(paper))
}

fn session_over(registry: Arc<VenueRegistry>) -> TradingSession {
    session_over_with(registry, VenueStorage::new("test.db".into()))
}

fn session_over_with(registry: Arc<VenueRegistry>, storage: VenueStorage) -> TradingSession {
    TradingSession::boot(&SessionConfig::new().build(), registry, storage).expect("boot")
}

/// `WorkContext` carrying the storage permit: `CommandUnit::execute` reads
/// the permit from its explicit context (the same channel `run_bar`
/// installs), while the journal gate reads the task-local scope.
fn ctx_with(permit: &VenueStorage) -> WorkContext {
    WorkContext {
        caps: fluent_wvr::CapabilitySet::new().with(permit.clone()),
        ..WorkContext::default()
    }
}

async fn scoped<F, T>(permit: VenueStorage, f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    let caps = fluent_wvr::CapabilitySet::new().with(permit);
    fluent_wvr::CURRENT_CAPS.scope(caps, f).await
}

fn journal_at(path: &std::path::Path) -> (Journal, VenueStorage) {
    let journal = Journal::open(path).expect("open");
    let permit = VenueStorage::new(path.to_owned());
    (journal, permit)
}

/// Risk approval minted for the close-all proposal: exits over a healthy
/// book, so the submission route — not risk — is what this suite pins.
fn approved_for_close() -> protopyne_strategy::RiskApproved {
    let engine = protopyne_strategy::RiskEngine::new(protopyne_strategy::RiskLimits::default());
    let proposal = protopyne_strategy::Proposal::new()
        .id("close-1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .direction(protopyne_strategy::Direction::Long)
        .side(protopyne_strategy::ProposalSide::Exit)
        .structure(protopyne_strategy::StructureKind::Vertical)
        .quantity(protopyne_types::Qty::new(Decimal::from(2)))
        .limit_price(money("600.50"))
        .max_loss(money("300"))
        .notional(money("1_000"))
        .dte_days(30)
        .conviction(0.8)
        .activity_score(0.7)
        .created_at(now())
        .build();
    let state = protopyne_strategy::RiskState {
        quote_age_secs: 5,
        clock_skew_secs: 1,
        reconciliation_healthy: true,
        is_limit_order: true,
        is_spread: true,
    };
    engine.check(&proposal, &state).expect("healthy exit")
}

// ── journal-replay: command rows rebuild state ──────────────────────

/// Disarm and resume flow through `CommandUnit` into journaled rows; replay
/// folds them back into the halted-then-live state. No direct mutation
/// without a row: the state a restart rebuilds is the state the operator
/// left.
#[tokio::test]
async fn command_rows_replay_into_trading_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    let (journal, permit) = journal_at(&path);
    let journal = Arc::new(journal);
    let venue = paper();
    let state = StateHandle::new(TradingState::Enabled);
    let portfolios = Arc::new(PortfolioRegistry::new());

    scoped(permit.clone(), async {
        let ctx = ctx_with(&permit);
        let disarm = CommandUnit::new()
            .name("cmd.disarm".to_owned())
            .command(CommandKind::Disarm)
            .venue(Arc::clone(&venue))
            .state(state.clone())
            .registry(Arc::clone(&portfolios))
            .journal(Arc::clone(&journal))
            .build();
        disarm.execute(&ctx).expect("disarm executes");
        assert_eq!(state.get(), TradingState::Disabled);

        let mid = journal.replay(&permit).expect("replay").events;
        assert_eq!(
            replay_trading_state(&mid),
            TradingState::Disabled,
            "replay rebuilds the halt from rows alone"
        );

        let resume = CommandUnit::new()
            .name("cmd.resume".to_owned())
            .command(CommandKind::Resume)
            .venue(Arc::clone(&venue))
            .state(state.clone())
            .registry(Arc::clone(&portfolios))
            .journal(Arc::clone(&journal))
            .build();
        resume.execute(&ctx).expect("resume executes");
        assert_eq!(state.get(), TradingState::Enabled);
    })
    .await;

    scoped(permit.clone(), async {
        let replayed = journal.replay(&permit).expect("replay");
        let kinds: Vec<&str> = replayed.events.iter().map(|event| event.kind.as_str()).collect();
        assert!(
            kinds.contains(&COMMAND_DISARM_KIND),
            "disarm journaled its row, got: {kinds:?}"
        );
        assert!(
            kinds.contains(&COMMAND_RESUME_KIND),
            "resume journaled its row, got: {kinds:?}"
        );
        assert_eq!(
            replay_trading_state(&replayed.events),
            TradingState::Enabled,
            "last command wins, exactly as the live handle reads"
        );
        assert!(
            replayed
                .events
                .iter()
                .all(|event| event.client_id == OPERATOR_CLIENT_ID),
            "command rows carry the operator client id"
        );

        // The monitor replica ignores command rows: no phantom orders appear.
        let view = StateReplica::apply(&replayed.events);
        assert!(
            view.orders.is_empty(),
            "command rows must not read as orders, got: {:?}",
            view.orders
        );
    })
    .await;
}

/// A journal-bound command without the storage capability refuses instead
/// of mutating unjournaled: journal-first is structural, not advisory.
#[tokio::test]
async fn unpermitted_command_refuses_without_mutating() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    let (journal, _permit) = journal_at(&path);
    let venue = paper();
    let state = StateHandle::new(TradingState::Enabled);
    let portfolios = Arc::new(PortfolioRegistry::new());
    let disarm = CommandUnit::new()
        .name("cmd.disarm".to_owned())
        .command(CommandKind::Disarm)
        .venue(Arc::clone(&venue))
        .state(state.clone())
        .registry(Arc::clone(&portfolios))
        .journal(Arc::new(journal))
        .build();
    // No capability scope: the permit path is absent.
    let refusal = disarm.execute(&WorkContext::default()).expect_err("must refuse");
    assert!(
        matches!(refusal, WorkError::Execution(_)),
        "refusal is an execution failure, got: {refusal:?}"
    );
    assert_eq!(
        state.get(),
        TradingState::Enabled,
        "refused commands change nothing"
    );
}

// ── halt: entries refused, exits fill, resume restores ───────────────

/// Halted sessions refuse entries with a reason while exits still fill;
/// resume restores entries. The gate is the shared handle the bars also
/// read — one authority, two enforcement points.
#[tokio::test]
async fn halted_entries_refuse_while_exits_fill() {
    let venue = paper();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("session.db");
    let journal = Arc::new(Journal::open(&path).expect("open"));
    let session = session_over_with(registry_over(&venue), VenueStorage::new(path));
    let state = session.state_handle().clone();
    assert!(state.get().allows(false), "sessions boot allowing everything");

    state.set(TradingState::Disabled);
    let refusal = session
        .submit_order(job(&journal, "h#entry", "SPY", false))
        .await
        .expect_err("halted entries must refuse");
    let reason = refusal.to_string();
    assert!(
        reason.contains("halt") || reason.contains("Disabled"),
        "refusal names its cause, got: {reason}"
    );
    assert!(venue.orders().is_empty(), "refused entries never reach the venue");

    let exit = session
        .submit_order(job(&journal, "h#exit", "SPY", true))
        .await
        .expect("exits still flow while halted");
    assert_eq!(exit.filled_quantity, 2);

    state.set(TradingState::Enabled);
    let restored = session
        .submit_order(job(&journal, "h#entry2", "SPY", false))
        .await
        .expect("resume restores entries");
    assert_eq!(restored.filled_quantity, 2);
    session.shutdown().await;
}

/// Halted bars fail the trading sensor with its reason while independent
/// (exit/monitor) units still complete — the same containment shape as the
/// stream-health sensor, on the authority signal instead of the confidence
/// one.
#[tokio::test]
async fn halted_bar_fails_sensor_while_independents_complete() {
    use fluent_concurrency::batch::SupervisedBatchEvent;

    struct Plain {
        name: String,
    }

    impl WorkUnit for Plain {
        fn name(&self) -> &str {
            &self.name
        }

        fn depends(&self) -> &[ArcIntern<str>] {
            &[]
        }

        fn provides(&self) -> &[ArcIntern<str>] {
            &[]
        }

        fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
            Ok(WorkOutput::ok("monitored"))
        }
    }

    impl_fieldless!(Plain);

    impl Describable for Plain {
        fn describe(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
    }

    impl_component!(Plain);

    let venue = paper();
    let session = session_over(registry_over(&venue));
    session.state_handle().set(TradingState::Disabled);
    let summary = session
        .run_bar(vec![Arc::new(Plain {
            name: "monitor".to_owned(),
        })])
        .await;
    let failed: Vec<String> = summary
        .failed
        .iter()
        .filter_map(|event| match event {
            SupervisedBatchEvent::Failed { name, .. } => Some(name.to_string()),
            _ => None,
        })
        .collect();
    assert!(
        failed.iter().any(|name| name == "trading.state.sensor"),
        "halted bars fail the trading sensor, got: {failed:?}"
    );
    let completed: Vec<String> = summary
        .completed
        .iter()
        .filter_map(|event| match event {
            SupervisedBatchEvent::Completed { name, .. } => Some(name.to_string()),
            _ => None,
        })
        .collect();
    assert!(
        completed.iter().any(|name| name == "monitor"),
        "independent units still complete while halted, got: {completed:?}"
    );
    session.shutdown().await;
}

// ── close-all: exactly one intent per position, retries are lookups ──

/// Every open position emits exactly one close intent; resubmitting the
/// same intents returns the original records (query-by-ID, never a second
/// order); cancel sweeps refuse unknown ids and never chase terminal
/// orders.
#[tokio::test]
async fn close_all_emits_exactly_once_and_retries_lookup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    let (journal, permit) = journal_at(&path);
    let journal = Arc::new(journal);
    let venue = paper();
    let registry = registry_over(&venue);
    let session = session_over_with(Arc::clone(&registry), VenueStorage::new(path.clone()));
    let portfolios = Arc::new(PortfolioRegistry::new());
    portfolios.register(
        protopyne_types::Portfolio::new()
            .id(protopyne_types::PortfolioId::new("pf-paper".to_owned()))
            .name("paper".to_owned())
            .strategies(vec!["ribbon".to_owned()])
            .accounts(vec!["paper-acct".to_owned()])
            .build(),
    );

    scoped(permit.clone(), async {
        // Two underlyings open two positions through the live registry.
        for (id, underlying) in [("o#spy", "SPY"), ("o#aapl", "AAPL")] {
            registry
                .submit(intent(id, underlying), &quote(), now())
                .await
                .expect("position opens");
        }
        assert_eq!(registry.positions().len(), 2);

        let close = CommandUnit::new()
            .name("cmd.close.paper".to_owned())
            .command(CommandKind::CloseAll {
                portfolio: "paper".to_owned(),
            })
            .venue(Arc::clone(&venue))
            .state(session.state_handle().clone())
            .registry(Arc::clone(&portfolios))
            .limit(money("1.00"))
            .proposal_id("close-1".to_owned())
            .strategy("ribbon".to_owned())
            .journal(Arc::clone(&journal))
            .build();
        let output = close.execute(&ctx_with(&permit)).expect("close-all executes");
        let intents: Vec<OrderIntent> =
            serde_json::from_value(output.data.clone()).expect("intents round-trip as data");
        assert_eq!(intents.len(), 2, "one intent per position, never per leg");
        let mut ids: Vec<&str> = intents.iter().map(|intent| intent.client_id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec!["close-0-AAPL", "close-1-SPY"]);

        // Same groups, same intents: emission is deterministic.
        let again = close.execute(&ctx_with(&permit)).expect("close-all executes");
        let intents_again: Vec<OrderIntent> =
            serde_json::from_value(again.data.clone()).expect("intents round-trip as data");
        assert_eq!(intents, intents_again, "close-all is deterministic");

        // Every intent submits through the safe path exactly once ...
        let approved = approved_for_close();
        let first_round = session
            .submit_close_intents(&journal, &approved, intents.clone(), &quote(), now())
            .await
            .expect("closes submit");
        assert_eq!(first_round.len(), 2);
        assert_eq!(registry.orders().len(), 4, "two opens plus two closes");

        // ... and resubmitting returns the originals: retries are lookups.
        let second_round = session
            .submit_close_intents(&journal, &approved, intents.clone(), &quote(), now())
            .await
            .expect("retries look up");
        assert_eq!(second_round, first_round, "retry returns the original records");
        assert_eq!(registry.orders().len(), 4, "no duplicate orders minted");

        // Cancel sweeps: every order here is filled (terminal), so the sweep
        // is empty — sweeps never chase terminal orders ...
        assert!(
            cancel_all_ids(&registry.orders()).is_empty(),
            "filled orders never re-list for cancellation"
        );
        // ... unknown ids refuse with reason, and the empty sweep succeeds.
        let unknown = session
            .cancel_orders(&["no-such-order".to_owned()], now())
            .await
            .expect_err("unknown cancel must refuse");
        assert!(
            unknown.to_string().contains("no-such-order"),
            "refusal names the id, got: {unknown}"
        );
        let empty = session
            .cancel_orders(&[], now())
            .await
            .expect("empty sweep succeeds");
        assert!(empty.is_empty());

        // The close-all invocation journaled its audit row.
        let tailed = journal.replay(&permit).expect("replay").events;
        assert!(
            tailed.iter().any(|event| event.kind == COMMAND_CLOSE_ALL_KIND),
            "close-all journals its audit row"
        );
    })
    .await;
    session.shutdown().await;
}

// ── approval inbox: human decisions are journaled lifecycle rows ──

/// A proposal waiting on a human: risk passed, no grant covers it. Setup
/// moves stay in memory; only the human act journals — the tests pin that
/// the decision row is what replay projects.
fn pending_proposal(id: &str) -> protopyne_strategy::Proposal {
    use protopyne_strategy::ProposalEvent as E;
    let mut proposal = protopyne_strategy::Proposal::new()
        .id(id.to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .direction(protopyne_strategy::Direction::Long)
        .side(protopyne_strategy::ProposalSide::Entry)
        .structure(protopyne_strategy::StructureKind::Vertical)
        .quantity(protopyne_types::Qty::new(Decimal::from(2)))
        .limit_price(money("600.50"))
        .max_loss(money("300"))
        .notional(money("1_000"))
        .dte_days(30)
        .conviction(0.8)
        .activity_score(0.7)
        .created_at(now())
        .build();
    proposal.apply(E::PassRisk, "risk approved", now()).expect("pass risk");
    proposal.apply(E::RequireHuman, "no grant covers entry", now()).expect("ask human");
    assert_eq!(proposal.state, protopyne_strategy::ProposalState::PendingHuman);
    proposal
}

fn transition_rows(events: &[crate::journal::JournalEvent]) -> Vec<&crate::journal::JournalEvent> {
    events
        .iter()
        .filter(|event| event.kind == PROPOSAL_TRANSITION_KIND)
        .collect()
}

/// Approving journals one lifecycle row, advances the proposal, replays to
/// `Approved`, and leaves the order replica empty: a human decision is
/// authority over a proposal, never a phantom order.
#[tokio::test]
async fn approval_journals_advances_and_replays() {
    use protopyne_strategy::{ProposalEvent as E, ProposalState as S};
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    let (journal, permit) = journal_at(&path);
    let mut proposal = pending_proposal("prop-approve-1");
    scoped(permit.clone(), async {
        let entry = decide_proposal(&journal, &permit, &mut proposal, E::Approve, "operator approved", now())
            .expect("pending approval records");
        assert_eq!((entry.from, entry.to), (S::PendingHuman, S::Approved));
        assert_eq!(proposal.state, S::Approved);
        let replayed = journal.replay(&permit).expect("replay").events;
        assert_eq!(transition_rows(&replayed).len(), 1, "exactly one decision row");
        assert_eq!(
            replay_proposal_states(&replayed).get("prop-approve-1"),
            Some(&S::Approved),
            "replay projects the journaled decision"
        );
        assert!(
            StateReplica::apply(&replayed).orders.is_empty(),
            "lifecycle rows must not read as orders"
        );
    })
    .await;
}

/// Rejecting journals one lifecycle row, advances to `Rejected`, and
/// replays — the same shape as approval, the other terminal human answer.
#[tokio::test]
async fn rejection_journals_advances_and_replays() {
    use protopyne_strategy::{ProposalEvent as E, ProposalState as S};
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    let (journal, permit) = journal_at(&path);
    let mut proposal = pending_proposal("prop-reject-1");
    scoped(permit.clone(), async {
        let entry = decide_proposal(&journal, &permit, &mut proposal, E::Reject, "too much size", now())
            .expect("pending rejection records");
        assert_eq!((entry.from, entry.to), (S::PendingHuman, S::Rejected));
        assert_eq!(proposal.state, S::Rejected);
        let replayed = journal.replay(&permit).expect("replay").events;
        assert_eq!(transition_rows(&replayed).len(), 1, "exactly one decision row");
        assert_eq!(
            replay_proposal_states(&replayed).get("prop-reject-1"),
            Some(&S::Rejected)
        );
    })
    .await;
}

/// Approving a proposal that is not waiting on a human refuses with the
/// state named, journals nothing, and leaves the proposal untouched.
#[tokio::test]
async fn approval_outside_pending_human_refuses_without_a_row() {
    use protopyne_strategy::{ProposalEvent as E, ProposalState as S};
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    let (journal, permit) = journal_at(&path);
    let mut proposal = pending_proposal("prop-early-1");
    proposal.state = S::Proposed;
    scoped(permit.clone(), async {
        let refusal = decide_proposal(&journal, &permit, &mut proposal, E::Approve, "too eager", now())
            .expect_err("approval from Proposed must refuse");
        assert!(refusal.to_string().contains("Proposed"), "refusal names the state, got: {refusal}");
        assert_eq!(proposal.state, S::Proposed, "refused decisions change nothing");
        let replayed = journal.replay(&permit).expect("replay").events;
        assert!(transition_rows(&replayed).is_empty(), "no row without a decision");
        assert!(replay_proposal_states(&replayed).is_empty(), "no projection without rows");
    })
    .await;
}

/// A second decision on a settled proposal refuses: terminal states take
/// no further human act, and the first row stands alone.
#[tokio::test]
async fn second_decision_on_a_settled_proposal_refuses() {
    use protopyne_strategy::ProposalEvent as E;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    let (journal, permit) = journal_at(&path);
    let mut proposal = pending_proposal("prop-once-1");
    scoped(permit.clone(), async {
        decide_proposal(&journal, &permit, &mut proposal, E::Approve, "yes", now())
            .expect("first decision records");
        decide_proposal(&journal, &permit, &mut proposal, E::Approve, "yes again", now())
            .expect_err("settled proposals take no second act");
        let replayed = journal.replay(&permit).expect("replay").events;
        assert_eq!(transition_rows(&replayed).len(), 1, "the first row stands alone");
    })
    .await;
}

/// The decision path carries human acts only: machine lifecycle events
/// (`Submit`, `Fill`, …) refuse here and travel the run path instead, so
/// the journal never confuses who authorized what.
#[tokio::test]
async fn machine_events_refuse_on_the_decision_path() {
    use protopyne_strategy::{ProposalEvent as E, ProposalState as S};
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    let (journal, permit) = journal_at(&path);
    let mut proposal = pending_proposal("prop-submit-1");
    scoped(permit.clone(), async {
        let refusal = decide_proposal(&journal, &permit, &mut proposal, E::Submit, "machine act", now())
            .expect_err("machine events must refuse");
        assert!(refusal.to_string().contains("Approve"), "refusal names the plane, got: {refusal}");
        assert_eq!(proposal.state, S::PendingHuman, "refused events change nothing");
        let replayed = journal.replay(&permit).expect("replay").events;
        assert!(transition_rows(&replayed).is_empty(), "no row for a refused event");
    })
    .await;
}

/// The replay fold is last-wins per proposal, skips unparseable rows and
/// every other kind: a corrupt detail never moves a projection.
#[test]
fn replay_fold_is_last_wins_and_skips_noise() {
    use protopyne_strategy::{JournaledTransition, ProposalState as S};
    let row = |seq: i64, id: &str, to: S| crate::journal::JournalEvent {
        seq,
        client_id: id.to_owned(),
        kind: PROPOSAL_TRANSITION_KIND.to_owned(),
        detail: serde_json::to_string(&JournaledTransition {
            proposal_id: id.to_owned(),
            from: S::PendingHuman,
            to,
            reason: "test".to_owned(),
            at: now(),
        })
        .expect("detail encodes"),
        at_ms: 1_700_000_000_000 + seq,
    };
    let events = vec![
        row(1, "a", S::Approved),
        crate::journal::JournalEvent {
            seq: 2,
            client_id: "a".to_owned(),
            kind: "filled".to_owned(),
            detail: "{}".to_owned(),
            at_ms: 1_700_000_000_002,
        },
        crate::journal::JournalEvent {
            seq: 3,
            client_id: "b".to_owned(),
            kind: PROPOSAL_TRANSITION_KIND.to_owned(),
            detail: "not json".to_owned(),
            at_ms: 1_700_000_000_003,
        },
        row(4, "a", S::Rejected),
        row(5, "c", S::Approved),
    ];
    let states = replay_proposal_states(&events);
    assert_eq!(states.get("a"), Some(&S::Rejected), "last row wins");
    assert_eq!(states.get("c"), Some(&S::Approved));
    assert!(!states.contains_key("b"), "corrupt details never project");
    assert_eq!(states.len(), 2);
}
