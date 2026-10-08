use super::*;
use crate::order::{OrderIntent, OrderSide, OrderStatus};
use chrono::Utc;
use protopyne_types::Money;
use rust_decimal::Decimal;
use std::future::Future;

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
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

/// Run `f` with the storage capability installed in the task-local set.
/// Journal writes outside this scope fail closed by design.
async fn scoped<F, T>(permit: VenueStorage, f: F) -> T
where
    F: Future<Output = T>,
{
    let caps = fluent_wvr::CapabilitySet::new().with(permit);
    fluent_wvr::CURRENT_CAPS.scope(caps, f).await
}

fn setup() -> (tempfile::TempDir, Journal, VenueStorage) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    let journal = Journal::open(&path).expect("open");
    let permit = VenueStorage::new(path);
    (dir, journal, permit)
}

#[tokio::test]
async fn intent_journaling_is_idempotent() {
    let (_dir, journal, permit) = setup();
    let now = Utc::now();
    scoped(permit.clone(), async {
        assert!(journal.record_intent(&permit, &intent("a#0"), now).expect("first"));
        assert!(!journal.record_intent(&permit, &intent("a#0"), now).expect("replay"));
        assert_eq!(
            journal.intent_status(&permit, "a#0").expect("status"),
            Some("pending".to_owned())
        );
    })
    .await;
}

#[tokio::test]
async fn outbox_and_replay_rebuild_state() {
    let (_dir, journal, permit) = setup();
    let now = Utc::now();
    scoped(permit.clone(), async {
        journal.record_intent(&permit, &intent("a#0"), now).expect("intent a");
        journal.record_intent(&permit, &intent("b#0"), now).expect("intent b");
        journal
            .mark_status(&permit, "a#0", OrderStatus::Filled, now)
            .expect("filled");

        let pending = journal.outbox_pending(&permit).expect("outbox");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].client_id, "b#0");

        let replayed = journal.replay(&permit).expect("replay");
        assert_eq!(replayed.intents.len(), 2);
        assert!(replayed.events.len() >= 3);

        let record = journal
            .record_from_replay(&permit, "a#0")
            .expect("record")
            .expect("known");
        assert_eq!(record.status, OrderStatus::Filled);
        assert!(journal.record_from_replay(&permit, "zzz").expect("miss").is_none());
    })
    .await;
}

#[tokio::test]
async fn writes_fail_closed_without_the_capability() {
    let (_dir, journal, permit) = setup();
    let err = journal
        .record_intent(&permit, &intent("a#0"), Utc::now())
        .expect_err("no capability installed");
    assert!(matches!(err, VenueError::Journal(_)));

    let foreign = VenueStorage::new("/elsewhere/journal.db".into());
    let caps = fluent_wvr::CapabilitySet::new().with(foreign.clone());
    let err = fluent_wvr::CURRENT_CAPS
        .scope(caps, async {
            journal.record_intent(&foreign, &intent("a#0"), Utc::now())
        })
        .await
        .expect_err("permit bound elsewhere");
    assert!(matches!(err, VenueError::Journal(_)));
}
