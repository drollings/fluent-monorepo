use super::*;
use crate::order::{OrderIntent, OrderSide, OrderStatus};
use crate::venue::LegExecution;
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

fn leg(contract: &str) -> LegExecution {
    LegExecution {
        contract: protopyne_types::ContractId::new(contract),
        side: OrderSide::Buy,
        quantity: 1,
        price: money("2.50"),
        fee: money("0.65"),
    }
}

/// Independent linear fold over the log's events — the oracle the index must
/// reproduce: intent ids in append order, each with its status (last
/// status-kind row, `pending` when none).
fn linear_scan(events: &[JournalEvent]) -> Vec<(String, String)> {
    let mut seen = std::collections::HashSet::new();
    let mut folded: Vec<(String, String)> = Vec::new();
    for event in events {
        if event.kind == INTENT_KIND {
            if seen.insert(event.client_id.clone()) {
                folded.push((event.client_id.clone(), "pending".to_owned()));
            }
        } else if is_status_kind(&event.kind) {
            if let Some(entry) = folded.iter_mut().find(|(id, _)| *id == event.client_id) {
                entry.1.clone_from(&event.kind);
            }
        }
    }
    folded
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
        assert!(journal
            .record_intent(&permit, &intent("a#0"), now)
            .expect("first"));
        assert!(!journal
            .record_intent(&permit, &intent("a#0"), now)
            .expect("replay"));
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
        journal
            .record_intent(&permit, &intent("a#0"), now)
            .expect("intent a");
        journal
            .record_intent(&permit, &intent("b#0"), now)
            .expect("intent b");
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
        assert!(journal
            .record_from_replay(&permit, "zzz")
            .expect("miss")
            .is_none());
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

#[tokio::test]
async fn intent_write_leaves_no_half_written_state() {
    // Intent plus its event commit atomically: replay must show both or the
    // test would catch a crash-between-statements shape.
    let (_dir, journal, permit) = setup();
    let now = Utc::now();
    scoped(permit.clone(), async {
        journal
            .record_intent(&permit, &intent("a#0"), now)
            .expect("intent");
        let state = journal.replay(&permit).expect("replay");
        assert_eq!(state.intents.len(), 1);
        assert!(
            state
                .events
                .iter()
                .any(|e| e.client_id == "a#0" && e.kind == "intent"),
            "the intent event must land with the intent row"
        );
        assert_eq!(
            journal.intent_status(&permit, "a#0").expect("status"),
            Some("pending".to_owned())
        );
    })
    .await;
}

#[tokio::test]
async fn status_write_leaves_no_half_written_state() {
    // Status UPDATE plus its event commit atomically: the stored status and
    // the replayed events must agree after every transition.
    let (_dir, journal, permit) = setup();
    let now = Utc::now();
    scoped(permit.clone(), async {
        journal
            .record_intent(&permit, &intent("a#0"), now)
            .expect("intent");
        journal
            .mark_status(&permit, "a#0", OrderStatus::Submitted, now)
            .expect("submitted");
        assert_eq!(
            journal.intent_status(&permit, "a#0").expect("status"),
            Some("submitted".to_owned())
        );
        let state = journal.replay(&permit).expect("replay");
        assert!(
            state
                .events
                .iter()
                .any(|e| e.client_id == "a#0" && e.kind == "submitted"),
            "the transition event must land with the status row"
        );
        let record = journal
            .record_from_replay(&permit, "a#0")
            .expect("record")
            .expect("known");
        assert_eq!(record.status, OrderStatus::Submitted);
    })
    .await;
}

#[tokio::test]
async fn outbox_covers_every_mid_flight_status() {
    // Crash recovery resolves through query-by-ID every intent that never
    // reached terminal: pending, submitted, acknowledged, AND partial must
    // all stay in the outbox, or a mid-flight ack would strand an unknown
    // position.
    let (_dir, journal, permit) = setup();
    let now = Utc::now();
    scoped(permit.clone(), async {
        journal
            .record_intent(&permit, &intent("a#0"), now)
            .expect("intent a");
        journal
            .record_intent(&permit, &intent("b#0"), now)
            .expect("intent b");
        journal
            .record_intent(&permit, &intent("c#0"), now)
            .expect("intent c");
        journal
            .record_intent(&permit, &intent("d#0"), now)
            .expect("intent d");
        journal
            .mark_status(&permit, "a#0", OrderStatus::Submitted, now)
            .expect("submitted");
        journal
            .mark_status(&permit, "b#0", OrderStatus::Acknowledged, now)
            .expect("acknowledged");
        journal
            .mark_status(&permit, "c#0", OrderStatus::Partial, now)
            .expect("partial");
        journal
            .mark_status(&permit, "d#0", OrderStatus::Filled, now)
            .expect("filled");
        let pending: Vec<String> = journal
            .outbox_pending(&permit)
            .expect("outbox")
            .into_iter()
            .map(|intent| intent.client_id)
            .collect();
        assert!(
            pending.contains(&"a#0".to_owned()),
            "submitted stays pending: {pending:?}"
        );
        assert!(
            pending.contains(&"b#0".to_owned()),
            "acknowledged stays pending — it is still mid-flight: {pending:?}"
        );
        assert!(
            pending.contains(&"c#0".to_owned()),
            "partial stays pending — the venue may still change it: {pending:?}"
        );
        assert!(
            !pending.contains(&"d#0".to_owned()),
            "filled leaves the outbox"
        );
    })
    .await;
}

#[tokio::test]
async fn index_rebuild_matches_the_linear_scan() {
    // The index is a derived accelerator, so a reopen must reproduce exactly
    // what an independent linear fold of the durable rows says: intent order,
    // per-client status, and the outbox.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    let permit = VenueStorage::new(path.clone());
    let now = Utc::now();
    let before = scoped(permit.clone(), async {
        let journal = Journal::open(&path).expect("open");
        journal
            .record_intent(&permit, &intent("a#0"), now)
            .expect("intent a");
        journal
            .record_intent(&permit, &intent("b#0"), now)
            .expect("intent b");
        journal
            .record_intent(&permit, &intent("c#0"), now)
            .expect("intent c");
        journal
            .mark_status(&permit, "a#0", OrderStatus::Submitted, now)
            .expect("submitted");
        journal
            .mark_status(&permit, "a#0", OrderStatus::Acknowledged, now)
            .expect("acknowledged");
        journal
            .mark_status(&permit, "b#0", OrderStatus::Filled, now)
            .expect("filled");
        // A `verdict` row is not a status kind: it must never move a status.
        journal
            .record_transition(&permit, "c#0", "verdict", "status=Partial", now)
            .expect("verdict");
        journal.replay(&permit).expect("replay")
    })
    .await;

    scoped(permit.clone(), async {
        let journal = Journal::open(&path).expect("reopen rebuilds the index");
        let state = journal.replay(&permit).expect("replay");
        assert_eq!(state, before, "replay is identical across the rebuild");
        let oracle = linear_scan(&state.events);
        let ids: Vec<String> = oracle.iter().map(|(id, _)| id.clone()).collect();
        assert_eq!(
            state
                .intents
                .iter()
                .map(|i| i.client_id.clone())
                .collect::<Vec<_>>(),
            ids,
            "rebuilt intents match the linear scan"
        );
        for (id, status) in &oracle {
            assert_eq!(
                journal
                    .intent_status(&permit, id)
                    .expect("status")
                    .as_deref(),
                Some(status.as_str()),
                "index status for {id} matches the linear scan"
            );
        }
        let pending: Vec<String> = journal
            .outbox_pending(&permit)
            .expect("outbox")
            .into_iter()
            .map(|intent| intent.client_id)
            .collect();
        let expected: Vec<String> = oracle
            .iter()
            .filter(|(_, status)| {
                matches!(
                    status.as_str(),
                    "pending" | "submitted" | "acknowledged" | "partial"
                )
            })
            .map(|(id, _)| id.clone())
            .collect();
        assert_eq!(pending, expected, "the outbox matches the linear scan");
        assert_eq!(
            journal
                .intent_status(&permit, "c#0")
                .expect("status")
                .as_deref(),
            Some("pending"),
            "a non-status transition row leaves the status alone"
        );
        assert!(journal
            .intent_status(&permit, "absent")
            .expect("status")
            .is_none());
    })
    .await;
}

#[tokio::test]
async fn every_byte_offset_rebuilds_a_consistent_index() {
    // Crash injection at every offset: reopening a truncated log rebuilds an
    // index that agrees with a linear fold of the committed prefix — never a
    // half-written or drifting answer.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("journal.db");
    let permit = VenueStorage::new(path.clone());
    let now = Utc::now();
    scoped(permit.clone(), async {
        let journal = Journal::open(&path).expect("open");
        journal
            .record_intent(&permit, &intent("a#0"), now)
            .expect("a");
        journal
            .record_intent(&permit, &intent("b#0"), now)
            .expect("b");
        journal
            .mark_status(&permit, "a#0", OrderStatus::Submitted, now)
            .expect("submitted");
        journal
            .mark_status(&permit, "a#0", OrderStatus::Filled, now)
            .expect("filled");
        journal
            .record_leg_fill(&permit, "b#0", &leg("SPY 260117C00600000"), now)
            .expect("leg");
    })
    .await;
    let raw = std::fs::read(&path).expect("journal bytes");
    for offset in 0..=raw.len() {
        let probe = path.with_extension(format!("cut{offset}"));
        std::fs::write(&probe, &raw[..offset]).expect("cut");
        let probe_permit = VenueStorage::new(probe.clone());
        let caps = fluent_wvr::CapabilitySet::new().with(probe_permit.clone());
        fluent_wvr::CURRENT_CAPS
            .scope(caps, async {
                let journal = Journal::open(&probe).expect("every cut reopens");
                let state = journal.replay(&probe_permit).expect("replay");
                let oracle = linear_scan(&state.events);
                let ids: Vec<String> = oracle.iter().map(|(id, _)| id.clone()).collect();
                assert_eq!(
                    state
                        .intents
                        .iter()
                        .map(|i| i.client_id.clone())
                        .collect::<Vec<_>>(),
                    ids,
                    "offset {offset}: rebuilt intents match the linear scan"
                );
                for (id, status) in &oracle {
                    assert_eq!(
                        journal
                            .intent_status(&probe_permit, id)
                            .expect("status")
                            .as_deref(),
                        Some(status.as_str()),
                        "offset {offset}: index status for {id}"
                    );
                }
            })
            .await;
        std::fs::remove_file(&probe).ok();
    }
}
