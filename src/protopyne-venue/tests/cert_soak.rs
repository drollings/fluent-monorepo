//! Cert-sandbox soak, hermetic twin: the deterministic fault schedule runs
//! against the paper venue with no credentials and no network, and every
//! number is asserted exactly.
//!
//! The credential-gated live run executes the same schedule shape against
//! the certification sandbox; nothing about the soak's logic is provable
//! only with credentials.

use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;

use protopyne_strategy::fixtures::grant_with;
use protopyne_strategy::{
    Direction, Grant, GrantLedger, GrantMarket, OrderShape, PortfolioFacts, Proposal, ProposalSide,
    ReAttestationWindow, RiskEngine, RiskLimits, RiskState, StructureKind,
};
use protopyne_types::{Money, Qty};

use crate::cert_soak::{
    fault_schedule, run_paper_soak, ALERTS_FILE, BREAKER_FILE, JOURNAL_FILE, RECONCILE_FILE,
};
use crate::command::{StateHandle, TradingState};
use crate::health::{HealthLimits, HealthState};
use crate::journal::{Journal, VenueStorage, INTENT_KIND};
use crate::pipeline::{DecisionPipeline, PipelineRequest};
use crate::{Alert, AlertSource, Fault};

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap()
}

fn proposal(side: ProposalSide) -> Proposal {
    Proposal::new()
        .id("soak-twin-1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .direction(Direction::Long)
        .side(side)
        .structure(StructureKind::Vertical)
        .quantity(Qty::new(Decimal::from(2)))
        .limit_price(money("600.50"))
        .max_loss(money("300"))
        .notional(money("1000"))
        .dte_days(30)
        .conviction(0.8)
        .activity_score(0.7)
        .created_at(now())
        .build()
}

fn risk_state() -> RiskState {
    RiskState {
        quote_age_secs: 0,
        clock_skew_secs: 0,
        reconciliation_healthy: true,
        facts: PortfolioFacts::unobserved(),
    }
}

fn attested_grant() -> Grant {
    let mut grant = grant_with(|grant| {
        grant.id = "soak-attested".to_owned();
        grant.signal.min_conviction = 0.5;
        grant.signal.min_activity_score = 0.5;
        grant.re_attestation = Some(ReAttestationWindow {
            cadence: std::time::Duration::from_hours(24),
            grace: std::time::Duration::from_hours(1),
        });
    });
    grant.valid_from = now() - chrono::Duration::hours(30);
    grant
}

fn evaluate_attested(ago_secs: i64, is_exit: bool) -> crate::pipeline::PipelineVerdict<'static> {
    // The ledger must outlive the verdict's reservation borrow; leaking is
    // test-only and keeps the assertion shape exact.
    let ledger: &'static GrantLedger = Box::leak(Box::new(GrantLedger::new(attested_grant())));
    ledger.observe_attested(now() - chrono::Duration::seconds(ago_secs));
    let pipeline = DecisionPipeline::new(StateHandle::new(TradingState::Enabled));
    let state = HealthState {
        quote_age_secs: 5,
        clock_skew_secs: 1,
        stream_connected: true,
        store: protopyne_store::StoreHealth::default(),
        reconciled: true,
        backfilled: true,
    };
    let limits = HealthLimits::new().build();
    let risk = RiskEngine::new(RiskLimits::new().build());
    let market = GrantMarket::new()
        .quote_age_secs(0)
        .spread_width(money("1"))
        .build();
    let proposal = Box::leak(Box::new(proposal(if is_exit {
        ProposalSide::Exit
    } else {
        ProposalSide::Entry
    })));
    let risk_state = Box::leak(Box::new(risk_state()));
    let market = Box::leak(Box::new(market));
    let state = Box::leak(Box::new(state));
    let limits = Box::leak(Box::new(limits));
    let risk = Box::leak(Box::new(risk));
    pipeline.evaluate(&PipelineRequest {
        proposal,
        shape: OrderShape::new(true, true),
        is_exit,
        health: Some((state, limits)),
        risk_state,
        market,
        now: now(),
        risk,
        ledger,
    })
}

#[test]
fn the_schedule_is_three_named_faults_behind_the_one_injector() {
    // No second injector: the stream half drives the supervised breaker and
    // the reconcile half observes a scripted drift; only venue faults ride
    // the queue.
    assert_eq!(
        fault_schedule(),
        vec![
            Fault::DropConnection,
            Fault::Status500,
            Fault::Status500,
            Fault::Status500,
            Fault::Status500,
            Fault::LoseAck,
        ]
    );
}

/// The paper run executes the schedule exactly: every count the runbook's
/// pass criteria name is asserted here first, so the live run inherits
/// proven numbers rather than inventing its own.
#[tokio::test]
async fn paper_run_executes_the_schedule_exactly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let outcome = run_paper_soak(dir.path()).await.expect("soak runs");

    assert_eq!(outcome.intents_journaled, 4, "one row per submitted ID");
    assert_eq!(
        outcome.orders, 3,
        "the clean fill, the retried drop, and the lost ack"
    );
    assert_eq!(outcome.unjournaled_intents, 0);
    assert_eq!(outcome.unknown_positions, 0);

    assert_eq!(outcome.stream_attempts, 3, "trip on the third error");
    assert!(outcome.breaker_tripped);
    assert!(
        outcome.breaker_reason.contains("3 consecutive"),
        "got: {}",
        outcome.breaker_reason
    );
    assert!(
        outcome.fresh_breaker_untripped,
        "recovery is a new run, never a reset"
    );

    assert!(outcome.open_matched, "the run opens on a clean book");
    assert_eq!(outcome.drift_mismatches, 1, "the scheduled drift");
    assert!(outcome.halted_during_drift);
    assert!(outcome.cleared_after, "the closing pass clears the halt");

    assert_eq!(outcome.alerts_delivered, 2, "one trip page, one drift page");
    assert_eq!(outcome.repoll_delivered, 0, "delivery drains");
}

/// The paper run emits every artifact the runbook names, and each one
/// parses back to the run's own numbers.
#[tokio::test]
async fn paper_run_emits_the_four_runbook_artifacts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let outcome = run_paper_soak(dir.path()).await.expect("soak runs");

    let journal_bytes = dir.path().join(JOURNAL_FILE);
    let reconcile_path = dir.path().join(RECONCILE_FILE);
    let breaker_path = dir.path().join(BREAKER_FILE);
    let alerts_path = dir.path().join(ALERTS_FILE);
    for path in [&journal_bytes, &reconcile_path, &breaker_path, &alerts_path] {
        assert!(path.is_file(), "missing artifact: {}", path.display());
    }

    // Journal bytes replay to the run's own intent census.
    let permit = VenueStorage::new(journal_bytes.clone());
    let caps = fluent_wvr::CapabilitySet::new().with(permit.clone());
    let intents = fluent_wvr::CURRENT_CAPS
        .scope(caps, async {
            Journal::open(&journal_bytes)
                .expect("journal parses")
                .replay(&permit)
                .expect("journal replays")
                .events
                .into_iter()
                .filter(|event| event.kind == INTENT_KIND)
                .count()
        })
        .await;
    assert_eq!(intents, outcome.intents_journaled);

    // Reconcile history: clean, drift, clean — matching the run's verdicts.
    let history = std::fs::read_to_string(&reconcile_path).expect("reconcile reads");
    let lines: Vec<serde_json::Value> = history
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("reconcile line parses"))
        .collect();
    assert_eq!(lines.len(), 3, "open, drift, closing");
    assert_eq!(lines[0]["matched"], serde_json::Value::Bool(true));
    assert_eq!(lines[1]["matched"], serde_json::Value::Bool(false));
    assert_eq!(lines[1]["mismatches"], serde_json::Value::from(1));
    assert_eq!(lines[2]["matched"], serde_json::Value::Bool(true));

    // Breaker log: the storm opens cold, trips, and the fresh run is cold.
    let log = std::fs::read_to_string(&breaker_path).expect("breaker log reads");
    let transitions: Vec<serde_json::Value> = log
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("breaker line parses"))
        .collect();
    assert_eq!(transitions.len(), 3);
    assert_eq!(transitions[0]["tripped"], serde_json::Value::Bool(false));
    assert_eq!(transitions[1]["tripped"], serde_json::Value::Bool(true));
    assert_eq!(transitions[2]["tripped"], serde_json::Value::Bool(false));

    // Alert file: exactly the run's delivered pages, typed per trigger.
    let pages = std::fs::read_to_string(&alerts_path).expect("alerts read");
    let alerts: Vec<Alert> = pages
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("alert line parses"))
        .collect();
    assert_eq!(alerts.len(), outcome.alerts_delivered);
    assert!(
        alerts
            .iter()
            .any(|alert| alert.source == AlertSource::BreakerTrip),
        "the trip pages"
    );
    assert!(
        alerts
            .iter()
            .any(|alert| alert.source == AlertSource::ReconcileMismatch),
        "the drift pages"
    );
}

/// A lapsed attestation disarms entries and still routes exits under the
/// same loop-shaped evaluation the soak runs per bar.
#[test]
fn a_lapsed_cadence_disarms_entries_while_exits_route() {
    // Attested 26 h ago against a 24 h cadence + 1 h grace: lapsed.
    let entry = evaluate_attested(26 * 3600, false);
    assert!(!entry.allowed);
    assert!(entry.risk_pass, "risk judges the trade, not the cadence");
    assert!(
        entry
            .grant_authorization_reasons
            .iter()
            .any(|reason| reason.contains("attestation")),
        "{:?}",
        entry.grant_authorization_reasons
    );
    let exit = evaluate_attested(26 * 3600, true);
    assert!(exit.allowed, "{:?}", exit.reasons);
}

/// The soak exercises the greeks path it depends on: put-call parity holds
/// to float rounding at the traded point, so a parity break would fail the
/// soak rather than silently mispricing the book.
#[test]
fn greeks_parity_holds_at_the_soaked_point() {
    use protopyne_kernels::greeks::bs_price;
    use protopyne_types::OptionKind;

    let spot = 600.0;
    let strike = 600.0;
    let years = 30.0 / 365.0;
    let rate = 0.04;
    let vol = 0.2;
    let call = bs_price(OptionKind::Call, spot, strike, years, rate, vol);
    let put = bs_price(OptionKind::Put, spot, strike, years, rate, vol);
    let parity = spot - strike * (-rate * years).exp();
    assert!(
        (call - put - parity).abs() < 1e-9,
        "call {call} put {put} parity {parity}"
    );
}

/// Each pass criterion from the runbook's table, asserted individually: the
/// live run must satisfy the same table, and this is the hermetic proof it
/// can be satisfied.
#[tokio::test]
async fn paper_run_satisfies_every_pass_criterion() {
    let dir = tempfile::tempdir().expect("tempdir");
    let outcome = run_paper_soak(dir.path()).await.expect("soak runs");

    assert_eq!(
        outcome.unjournaled_intents, 0,
        "zero unjournaled intents: every submit journals first"
    );
    assert_eq!(
        outcome.unknown_positions, 0,
        "zero unknown positions: the closing census reconciles"
    );
    assert_eq!(
        outcome.alerts_delivered, 2,
        "every mismatch produced exactly one alert (plus the one trip page)"
    );
    assert_eq!(outcome.repoll_delivered, 0, "no duplicate pages on re-poll");
    assert!(
        outcome.breaker_tripped && outcome.fresh_breaker_untripped,
        "the breaker tripped as scheduled and recovery is a fresh run"
    );
    assert!(
        outcome.open_matched && outcome.cleared_after,
        "the book opens clean and the drift clears on the next clean pass"
    );
}
