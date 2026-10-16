//! Journal characterization: replay/outbox/status semantics, pinned before the log swap.
//!
//! Re-open persistence, idempotent intent recording, outbox membership,
//! leg-fill additivity, and status/event agreement are the contract the
//! file-backed journal must preserve.

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

async fn scoped<F, T>(permit: VenueStorage, f: F) -> T
where
    F: Future<Output = T>,
{
    let caps = fluent_wvr::CapabilitySet::new().with(permit);
    fluent_wvr::CURRENT_CAPS.scope(caps, f).await
}

fn setup() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    (dir, path)
}

#[tokio::test]
async fn replay_survives_reopen_with_intents_statuses_and_events() {
    let (_dir, path) = setup();
    let now = Utc::now();
    let permit = VenueStorage::new(path.clone());
    scoped(permit.clone(), async {
        let journal = Journal::open(&path).expect("open");
        journal.record_intent(&permit, &intent("a#0"), now).expect("intent a");
        journal.record_intent(&permit, &intent("b#0"), now).expect("intent b");
        journal
            .mark_status(&permit, "a#0", OrderStatus::Submitted, now)
            .expect("submitted");
        journal
            .record_transition(&permit, "a#0", "acknowledged", "{}", now)
            .expect("transition");
        let before = journal.replay(&permit).expect("replay");
        drop(journal);
        let reopened = Journal::open(&path).expect("reopen");
        let after = reopened.replay(&permit).expect("replay after reopen");
        assert_eq!(before, after, "replay is stable across re-open");
        assert_eq!(after.intents.len(), 2);
        assert!(
            after.events.iter().any(|e| e.client_id == "a#0" && e.kind == "intent"),
            "intent event survives reopen"
        );
        assert_eq!(
            reopened.intent_status(&permit, "a#0").expect("status"),
            Some("acknowledged".to_owned()),
            "status is the last status-kind event: the single log folds the \
             transition row, where the old dual table kept the mark_status \
             column — status survives reopen either way"
        );
        let pending = reopened.outbox_pending(&permit).expect("outbox");
        let ids: Vec<String> = pending.into_iter().map(|i| i.client_id).collect();
        assert!(ids.contains(&"a#0".to_owned()), "submitted stays in the outbox: {ids:?}");
        assert!(ids.contains(&"b#0".to_owned()), "pending stays in the outbox: {ids:?}");
    })
    .await;
}

#[tokio::test]
async fn status_and_events_agree_after_every_transition() {
    let (_dir, path) = setup();
    let now = Utc::now();
    let permit = VenueStorage::new(path.clone());
    scoped(permit.clone(), async {
        let journal = Journal::open(&path).expect("open");
        journal.record_intent(&permit, &intent("a#0"), now).expect("intent");
        for status in [OrderStatus::Submitted, OrderStatus::Acknowledged, OrderStatus::Filled] {
            journal.mark_status(&permit, "a#0", status, now).expect("transition");
            let stored = journal.intent_status(&permit, "a#0").expect("status");
            let state = journal.replay(&permit).expect("replay");
            let last = state
                .events
                .iter().rfind(|e| e.client_id == "a#0")
                .expect("transition event exists");
            let expected = match status {
                OrderStatus::Submitted => "submitted",
                OrderStatus::Acknowledged => "acknowledged",
                OrderStatus::Filled => "filled",
                _ => unreachable!(),
            };
            assert_eq!(stored.as_deref(), Some(expected));
            assert_eq!(last.kind, expected, "event and status agree");
        }
    })
    .await;
}

#[tokio::test]
async fn leg_fills_are_additive_rows() {
    use crate::venue::LegExecution;
    let (_dir, path) = setup();
    let now = Utc::now();
    let permit = VenueStorage::new(path.clone());
    scoped(permit.clone(), async {
        let journal = Journal::open(&path).expect("open");
        journal.record_intent(&permit, &intent("spread#0"), now).expect("intent");
        let before = journal.replay(&permit).expect("replay").events.len();
        for fill in ["SPY 260117C00600000", "SPY 260117P00550000"] {
            let leg = LegExecution {
                contract: protopyne_types::ContractId::new(fill),
                side: OrderSide::Buy,
                quantity: 1,
                price: money("2.50"),
                fee: money("0.65"),
            };
            journal.record_leg_fill(&permit, "spread#0", &leg, now).expect("leg fill");
        }
        let after = journal.replay(&permit).expect("replay");
        let legs: Vec<_> = after
            .events
            .iter()
            .filter(|e| e.kind == LEG_FILL_KIND)
            .collect();
        assert_eq!(legs.len(), 2, "one row per executed leg");
        assert_eq!(after.events.len(), before + 2);
        drop(journal);
        let reopened = Journal::open(&path).expect("reopen");
        let reread = reopened.replay(&permit).expect("replay");
        assert_eq!(reread, after, "leg rows survive reopen");
    })
    .await;
}
