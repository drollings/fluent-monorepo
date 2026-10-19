use super::*;
use crate::journal::{Journal, VenueStorage};
use crate::order::{OrderIntent, OrderSide, OrderStatus};
use crate::paper::{Fault, PaperConfig, PaperVenue};
use crate::venue::Venue;
use chrono::{TimeZone, Utc};
use protopyne_strategy::{RiskApproved, RiskEngine, RiskLimits, RiskState};
use protopyne_types::Money;
use rust_decimal::Decimal;
use std::sync::Arc;

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
        facts: protopyne_strategy::PortfolioFacts::unobserved(),
    };
    engine
        .check(
            &proposal,
            protopyne_strategy::OrderShape::new(true, true),
            &state,
        )
        .expect("healthy")
}

fn setup() -> (tempfile::TempDir, Journal, VenueStorage, PaperVenue) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    let journal = Journal::open(&path).expect("open");
    let permit = VenueStorage::new(path);
    let venue = PaperVenue::new(PaperConfig::new().build());
    (dir, journal, permit, venue)
}

async fn scoped<F, T>(permit: VenueStorage, f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    let caps = fluent_wvr::CapabilitySet::new().with(permit);
    fluent_wvr::CURRENT_CAPS.scope(caps, f).await
}

#[tokio::test]
async fn happy_path_journals_every_step() {
    let (_dir, journal, permit, venue) = setup();
    let token = approved();
    scoped(permit.clone(), async {
        let record = safe_submit(
            &permit,
            &journal,
            &venue,
            &token,
            intent("s#0"),
            &quote(),
            now(),
        )
        .await
        .expect("submit");
        assert_eq!(record.status, OrderStatus::Filled);
        assert_eq!(
            journal.intent_status(&permit, "s#0").expect("status"),
            Some("filled".to_owned())
        );
        let replayed = journal.replay(&permit).expect("replay");
        assert!(replayed.events.iter().any(|e| e.kind == "intent"));
        assert!(replayed.events.iter().any(|e| e.kind == "submitted"));
        assert!(replayed.events.iter().any(|e| e.kind == "filled"));
    })
    .await;
}

#[tokio::test]
async fn approval_for_another_proposal_is_refused() {
    let (_dir, journal, permit, venue) = setup();
    let token = approved();
    scoped(permit.clone(), async {
        let mut other = intent("s#0");
        other.proposal_id = "prop-2".to_owned();
        let err = safe_submit(&permit, &journal, &venue, &token, other, &quote(), now())
            .await
            .expect_err("token mismatch");
        assert!(matches!(err, VenueError::Invalid(_)));
    })
    .await;
}

#[tokio::test]
async fn lost_ack_recovers_through_query_with_one_order() {
    let (_dir, journal, permit, venue) = setup();
    venue.inject(Fault::LoseAck);
    let token = approved();
    scoped(permit.clone(), async {
        let record = safe_submit(
            &permit,
            &journal,
            &venue,
            &token,
            intent("k#0"),
            &quote(),
            now(),
        )
        .await
        .expect("recovers by ID");
        assert_eq!(record.status, OrderStatus::Filled);
        assert_eq!(venue.orders().len(), 1);
        assert_eq!(
            journal.intent_status(&permit, "k#0").expect("status"),
            Some("filled".to_owned())
        );
    })
    .await;
}

#[tokio::test]
async fn kill_mid_submit_replay_leaves_no_unknown_position() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    let venue = PaperVenue::new(PaperConfig::new().build());
    venue.inject(Fault::LoseAck);

    {
        let journal = Journal::open(&path).expect("open");
        let permit = VenueStorage::new(path.clone());
        let token = approved();
        scoped(permit.clone(), async {
            let crashed = safe_submit(
                &permit,
                &journal,
                &venue,
                &token,
                intent("c#0"),
                &quote(),
                now(),
            )
            .await
            .expect("retry dedups the lost ack");
            assert_eq!(crashed.status, OrderStatus::Filled);
        })
        .await;
    }

    // A fresh handle after the "crash": replay the outbox, resolve by ID.
    let journal = Journal::open(&path).expect("reopen");
    let permit = VenueStorage::new(path);
    scoped(permit.clone(), async {
        let pending = journal.outbox_pending(&permit).expect("outbox");
        assert!(pending.is_empty(), "filled intent leaves the outbox");
        let state = journal.replay(&permit).expect("replay");
        assert_eq!(state.intents.len(), 1);
        let record = venue.query("c#0").await.expect("venue truth");
        assert_eq!(record.status, OrderStatus::Filled);
        assert_eq!(venue.orders().len(), 1, "exactly one order exists");
        let shared = Arc::new(venue);
        assert_eq!(shared.orders().len(), 1);
    })
    .await;
}
