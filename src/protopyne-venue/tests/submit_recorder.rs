//! Recorder seam: the safe-submission protocol is recorder-agnostic.
//!
//! `submit_guarded` writes through [`SubmissionRecorder`], so the durable
//! [`Journal`] and the in-memory [`RunIntentLog`] are interchangeable peers.
//! These tests pin that seam: a stub proves the protocol touches only the
//! trait, the journal recorder reproduces `safe_submit`'s rows byte-for-byte,
//! and the in-memory log replays and journals idempotently.

use super::*;
use crate::journal::{Journal, RunIntentLog, VenueStorage, INTENT_KIND};
use crate::order::{OrderIntent, OrderSide, OrderStatus};
use crate::paper::{PaperConfig, PaperVenue};
use crate::venue::LegExecution;
use chrono::{TimeZone, Utc};
use protopyne_strategy::{RiskApproved, RiskEngine, RiskLimits, RiskState};
use protopyne_types::Money;
use rust_decimal::Decimal;
use std::sync::Mutex;

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
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

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 5).unwrap()
}

fn intent(id: &str) -> OrderIntent {
    OrderIntent::new()
        .client_id(id.to_owned())
        .proposal_id("prop-1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(2)
        .limit_price(money("600.50"))
        .build()
}

fn approved() -> RiskApproved {
    let engine = RiskEngine::new(RiskLimits::default());
    let proposal = protopyne_strategy::Proposal::new()
        .id("prop-1".to_owned())
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
    let state = RiskState {
        quote_age_secs: 5,
        clock_skew_secs: 1,
        reconciliation_healthy: true,
        is_limit_order: true,
        is_spread: true,
    };
    engine.check(&proposal, &state).expect("healthy")
}

async fn scoped<F, T>(permit: VenueStorage, f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    let caps = fluent_wvr::CapabilitySet::new().with(permit);
    fluent_wvr::CURRENT_CAPS.scope(caps, f).await
}

/// A minimal recorder that captures the protocol's mutations in order,
/// proving `submit_guarded` touches only the trait — no journal, no file.
#[derive(Default)]
struct StubRecorder {
    calls: Mutex<Vec<String>>,
    intents: Mutex<usize>,
}

impl SubmissionRecorder for StubRecorder {
    fn record_intent(&self, intent: &OrderIntent, _now: DateTime<Utc>) -> Result<bool, VenueError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("intent:{}", intent.client_id));
        let mut intents = self.intents.lock().unwrap();
        *intents += 1;
        Ok(*intents == 1)
    }

    fn mark_status(
        &self,
        client_id: &str,
        status: OrderStatus,
        _now: DateTime<Utc>,
    ) -> Result<(), VenueError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("status:{client_id}:{status:?}"));
        Ok(())
    }

    fn record_leg_fill(
        &self,
        client_id: &str,
        _leg: &LegExecution,
        _now: DateTime<Utc>,
    ) -> Result<(), VenueError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("leg:{client_id}"));
        Ok(())
    }

    fn record_transition(
        &self,
        client_id: &str,
        kind: &str,
        _detail: &str,
        _now: DateTime<Utc>,
    ) -> Result<(), VenueError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("transition:{client_id}:{kind}"));
        Ok(())
    }
}

#[tokio::test]
async fn submit_guarded_is_recorder_agnostic() {
    let recorder = StubRecorder::default();
    let venue = PaperVenue::new(PaperConfig::new().build());
    let token = approved();
    let record = submit_guarded(&recorder, &venue, &token, intent("r#0"), &quote(), now())
        .await
        .expect("submit through the stub");
    assert_eq!(record.status, OrderStatus::Filled);
    let calls = recorder.calls.lock().unwrap().clone();
    assert_eq!(calls.first().map(String::as_str), Some("intent:r#0"));
    assert!(calls.iter().any(|call| call == "status:r#0:Submitted"));
    assert!(calls.iter().any(|call| call == "status:r#0:Filled"));
    assert!(calls
        .iter()
        .any(|call| call == "transition:r#0:verdict"));
    assert_eq!(venue.orders().len(), 1, "the venue still executed once");
}

#[tokio::test]
async fn journal_recorder_matches_safe_submit_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path_a = dir.path().join("a.db");
    let path_b = dir.path().join("b.db");
    let journal_a = Journal::open(&path_a).expect("open a");
    let journal_b = Journal::open(&path_b).expect("open b");
    let permit_a = VenueStorage::new(path_a);
    let permit_b = VenueStorage::new(path_b);
    let venue_a = PaperVenue::new(PaperConfig::new().build());
    let venue_b = PaperVenue::new(PaperConfig::new().build());
    let token = approved();

    let events_a = scoped(permit_a.clone(), async {
        safe_submit(&permit_a, &journal_a, &venue_a, &token, intent("m#0"), &quote(), now())
            .await
            .expect("safe_submit");
        journal_a.replay(&permit_a).expect("replay a").events
    })
    .await;

    let events_b = scoped(permit_b.clone(), async {
        let recorder = JournalRecorder {
            permit: &permit_b,
            journal: &journal_b,
        };
        submit_guarded(&recorder, &venue_b, &token, intent("m#0"), &quote(), now())
            .await
            .expect("submit_guarded");
        journal_b.replay(&permit_b).expect("replay b").events
    })
    .await;

    assert_eq!(
        events_a, events_b,
        "the journal recorder must produce byte-identical rows to safe_submit"
    );
}

#[tokio::test]
async fn run_intent_log_replays_recorded_state() {
    let log = RunIntentLog::new();
    let venue = PaperVenue::new(PaperConfig::new().build());
    let token = approved();
    submit_guarded(&log, &venue, &token, intent("v#0"), &quote(), now())
        .await
        .expect("submit through the in-memory log");

    let replayed = log.replay().expect("replay");
    assert_eq!(replayed.intents.len(), 1, "one intent recorded");
    assert_eq!(replayed.intents[0].client_id, "v#0");
    assert!(replayed.events.iter().any(|event| event.kind == INTENT_KIND));
    assert!(replayed
        .events
        .iter()
        .any(|event| event.kind == "filled"));
    assert!(replayed
        .events
        .iter()
        .any(|event| event.kind == "verdict"));
    assert_eq!(log.intent_status("v#0").as_deref(), Some("filled"));
    assert_eq!(log.intent_status("absent"), None);
}

#[tokio::test]
async fn run_intent_log_intent_recording_is_idempotent() {
    let log = RunIntentLog::new();
    assert!(log.record_intent(&intent("i#0"), now()).expect("first"));
    assert!(
        !log.record_intent(&intent("i#0"), now()).expect("replay"),
        "re-recording the same client id writes nothing"
    );
    assert_eq!(log.events().len(), 1, "exactly one intent row landed");
}
