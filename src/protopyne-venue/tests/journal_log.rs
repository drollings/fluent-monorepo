//! Journal over the event log: contract pins for the file-backed swap.
//!
//! The storage idiom (one plain-text event log), the projection rules
//! (append-order intents, status as the last status-kind event), and the
//! durability rule (every byte-offset cut replays a prefix; mid-file
//! corruption refuses; a torn tail repairs exactly once) are pinned here.
//! Idempotent re-record, leg-fill additivity, and status/event agreement
//! are the preserved contract from the characterization suite.

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

/// Three events over two intents: the small journal every durability probe
/// below replays. Returns the replayed events in log order.
fn seed_small(journal: &Journal, permit: &VenueStorage) -> Vec<JournalEvent> {
    let now = Utc::now();
    journal
        .record_intent(permit, &intent("a#0"), now)
        .expect("intent a");
    journal
        .record_intent(permit, &intent("b#0"), now)
        .expect("intent b");
    journal
        .mark_status(permit, "a#0", OrderStatus::Submitted, now)
        .expect("submitted");
    journal.replay(permit).expect("replay").events
}

#[tokio::test]
async fn storage_is_a_plain_text_event_log() {
    // The SQLite file format is gone: the journal is a readable CSV event
    // log whose first line names the domain and version and whose second
    // line is the event header.
    let (_dir, path) = setup();
    let permit = VenueStorage::new(path.clone());
    scoped(permit.clone(), async {
        let journal = Journal::open(&path).expect("open");
        seed_small(&journal, &permit);
        drop(journal);
        let raw = std::fs::read(&path).expect("journal file reads");
        let text = String::from_utf8(raw).expect("the journal is plain text");
        let mut lines = text.lines();
        let version = lines.next().expect("version line");
        assert!(
            version.starts_with("venue_journal,"),
            "first line names the domain: {version:?}"
        );
        assert_eq!(
            lines.next().expect("header line"),
            "seq,client_id,kind,detail,at_ms",
            "second line is the event header"
        );
    })
    .await;
}

#[tokio::test]
async fn intents_replay_in_append_order_with_projected_status() {
    // Same stamp for every write: ordering comes from the log, never from
    // wall-clock ties. Status is the last status-kind event (`submitted`
    // here), `pending` when no status event exists.
    let (_dir, path) = setup();
    let permit = VenueStorage::new(path.clone());
    scoped(permit.clone(), async {
        let journal = Journal::open(&path).expect("open");
        let now = Utc::now();
        journal
            .record_intent(&permit, &intent("b#0"), now)
            .expect("intent b");
        journal
            .record_intent(&permit, &intent("a#0"), now)
            .expect("intent a");
        journal
            .mark_status(
                &permit,
                &intent("a#0").client_id,
                OrderStatus::Submitted,
                now,
            )
            .expect("submitted");
        let state = journal.replay(&permit).expect("replay");
        let ids: Vec<_> = state.intents.iter().map(|i| i.client_id.clone()).collect();
        assert_eq!(
            ids,
            vec!["b#0".to_owned(), "a#0".to_owned()],
            "append order wins"
        );
        assert_eq!(
            journal.intent_status(&permit, "a#0").expect("status"),
            Some("submitted".to_owned())
        );
        assert_eq!(
            journal.intent_status(&permit, "b#0").expect("status"),
            Some("pending".to_owned()),
            "no status event means pending"
        );
        let pending = journal.outbox_pending(&permit).expect("outbox");
        assert_eq!(pending.len(), 2, "both are still mid-flight");
    })
    .await;
}

#[tokio::test]
async fn status_is_a_projection_of_every_status_kind_event() {
    // A status-kind row lands the same status no matter which method wrote
    // it: the stored status is a fold over the log, not a second table.
    let (_dir, path) = setup();
    let permit = VenueStorage::new(path.clone());
    scoped(permit.clone(), async {
        let journal = Journal::open(&path).expect("open");
        let now = Utc::now();
        journal
            .record_intent(&permit, &intent("a#0"), now)
            .expect("intent");
        journal
            .record_transition(&permit, "a#0", "acknowledged", "{}", now)
            .expect("transition");
        assert_eq!(
            journal.intent_status(&permit, "a#0").expect("status"),
            Some("acknowledged".to_owned()),
            "the last status-kind event wins, whatever wrote it"
        );
        let pending: Vec<String> = journal
            .outbox_pending(&permit)
            .expect("outbox")
            .into_iter()
            .map(|i| i.client_id)
            .collect();
        assert!(
            pending.contains(&"a#0".to_owned()),
            "acknowledged stays mid-flight"
        );
    })
    .await;
}

#[tokio::test]
async fn idempotent_rerecord_writes_no_second_row() {
    let (_dir, path) = setup();
    let permit = VenueStorage::new(path.clone());
    scoped(permit.clone(), async {
        let journal = Journal::open(&path).expect("open");
        let now = Utc::now();
        assert!(journal
            .record_intent(&permit, &intent("a#0"), now)
            .expect("first"));
        assert!(!journal
            .record_intent(&permit, &intent("a#0"), now)
            .expect("replay"));
        let events = journal.replay(&permit).expect("replay").events;
        assert_eq!(
            events
                .iter()
                .filter(|e| e.client_id == "a#0" && e.kind == "intent")
                .count(),
            1,
            "one intent row, never two"
        );
    })
    .await;
}

#[tokio::test]
async fn leg_fills_are_additive_and_survive_reopen() {
    use crate::venue::LegExecution;
    let (_dir, path) = setup();
    let permit = VenueStorage::new(path.clone());
    scoped(permit.clone(), async {
        let journal = Journal::open(&path).expect("open");
        let now = Utc::now();
        journal
            .record_intent(&permit, &intent("spread#0"), now)
            .expect("intent");
        for fill in ["SPY 260117C00600000", "SPY 260117P00550000"] {
            journal
                .record_leg_fill(
                    &permit,
                    "spread#0",
                    &LegExecution {
                        contract: protopyne_types::ContractId::new(fill),
                        side: OrderSide::Buy,
                        quantity: 1,
                        price: money("2.50"),
                        fee: money("0.65"),
                    },
                    now,
                )
                .expect("leg fill");
        }
        let before = journal.replay(&permit).expect("replay");
        assert_eq!(
            before
                .events
                .iter()
                .filter(|e| e.kind == LEG_FILL_KIND)
                .count(),
            2,
            "one row per executed leg"
        );
        drop(journal);
        let reopened = Journal::open(&path).expect("reopen");
        assert_eq!(
            reopened.replay(&permit).expect("replay"),
            before,
            "leg rows survive reopen"
        );
    })
    .await;
}

#[tokio::test]
async fn torn_tail_repairs_exactly_once_on_reopen() {
    // Cutting the final byte tears the last record: reopen drops it with a
    // repair, and the next reopen changes nothing further.
    let (_dir, path) = setup();
    let permit = VenueStorage::new(path.clone());
    let full = scoped(permit.clone(), async {
        let journal = Journal::open(&path).expect("open");
        let events = seed_small(&journal, &permit);
        drop(journal);
        events
    })
    .await;
    let raw = std::fs::read(&path).expect("journal bytes");
    std::fs::write(&path, &raw[..raw.len() - 1]).expect("tear the tail");
    let repaired = scoped(permit.clone(), async {
        let journal = Journal::open(&path).expect("reopen repairs");
        journal.replay(&permit).expect("replay")
    })
    .await;
    assert_eq!(
        repaired.events.len(),
        full.len() - 1,
        "the torn record drops, whole records stay"
    );
    assert_eq!(&repaired.events, &full[..full.len() - 1]);
    let len_after_repair = std::fs::read(&path).expect("bytes").len();
    scoped(permit.clone(), async {
        let journal = Journal::open(&path).expect("second reopen");
        assert_eq!(
            journal.replay(&permit).expect("replay"),
            repaired,
            "the repair happens exactly once"
        );
    })
    .await;
    assert_eq!(
        std::fs::read(&path).expect("bytes").len(),
        len_after_repair,
        "no second truncation"
    );
}

#[tokio::test]
async fn every_byte_offset_replays_a_prefix() {
    // Crash-injection at every offset: cutting the log anywhere reopens to
    // the committed prefix — whole records only, sequence-contiguous —
    // never to a half-written state.
    let (_dir, path) = setup();
    let permit = VenueStorage::new(path.clone());
    let full = scoped(permit.clone(), async {
        let journal = Journal::open(&path).expect("open");
        let events = seed_small(&journal, &permit);
        drop(journal);
        events
    })
    .await;
    let raw = std::fs::read(&path).expect("journal bytes");
    assert!(raw.len() > 64, "the probe journal must have room to cut");
    for offset in 0..=raw.len() {
        let probe = path.with_extension(format!("cut{offset}"));
        std::fs::write(&probe, &raw[..offset]).expect("cut");
        let permit = VenueStorage::new(probe.clone());
        let caps = fluent_wvr::CapabilitySet::new().with(permit.clone());
        let replayed = fluent_wvr::CURRENT_CAPS
            .scope(caps, async {
                let journal = Journal::open(&probe).expect("every cut reopens");
                journal.replay(&permit).expect("every cut replays")
            })
            .await;
        let n = replayed.events.len();
        assert!(n <= full.len(), "offset {offset}: no invented rows");
        assert_eq!(
            &replayed.events,
            &full[..n],
            "offset {offset}: a committed prefix"
        );
        assert!(
            replayed
                .events
                .iter()
                .enumerate()
                .all(|(i, e)| e.seq == i as i64 + 1),
            "offset {offset}: sequences stay contiguous from 1"
        );
        std::fs::remove_file(&probe).ok();
    }
}

#[tokio::test]
async fn mid_file_corruption_refuses_loudly() {
    // A flipped byte in a committed row is corruption, not a tear: bytes
    // still follow it, so reopen must refuse naming the damage instead of
    // silently dropping the row.
    let (_dir, path) = setup();
    let permit = VenueStorage::new(path.clone());
    scoped(permit.clone(), async {
        let journal = Journal::open(&path).expect("open");
        seed_small(&journal, &permit);
        drop(journal);
        let mut raw = std::fs::read(&path).expect("journal bytes");
        // Corrupt the first data row's kind field (past the two header
        // lines) while leaving the rest of the file intact.
        let second_newline = raw
            .iter()
            .enumerate()
            .filter(|(_, b)| **b == b'\n')
            .nth(1)
            .expect("headers")
            .0;
        let victim = second_newline + 8;
        raw[victim] = if raw[victim] == b'x' { b'y' } else { b'x' };
        // If the flip landed inside a field the row may still parse (a
        // renamed kind); force undecodability by breaking the sequence
        // number instead, which must be an integer.
        let seq_end = raw[second_newline + 1..]
            .iter()
            .position(|b| *b == b',')
            .expect("seq field")
            + second_newline
            + 1;
        raw[second_newline + 1..seq_end].fill(b'q');
        std::fs::write(&path, &raw).expect("corrupt");
        let Err(err) = Journal::open(&path) else {
            panic!("corruption refuses")
        };
        let text = err.to_string();
        assert!(
            text.contains("corrupt"),
            "the refusal names the damage: {text}"
        );
    })
    .await;
}
