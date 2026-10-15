//! The capability gate: ungranted tasks are denied, not defaulted.
//!
//! Store effects check the task-local capability set before touching the
//! database. Outside a set carrying the store token, every gated
//! operation refuses with `Denied` — reads included, because a read that
//! reaches the database is an effect. This suite proves the denial from
//! both sides: without the token it fails, and the failure names the
//! gate rather than masquerading as an empty store or a database error.

use super::*;
use protopyne_types::Timeframe;

use crate::commit::test_tapes::at_ms;
use crate::key::{SymbolId, TapeKey};
use crate::sqlite_tape::test_setup::{VENUE, memory_tape, with_memory_caps};
use crate::store::{CandleStore, WritePolicy};

fn key_of(id: SymbolId) -> TapeKey {
    TapeKey {
        symbol_id: id,
        timeframe: Timeframe::M1,
    }
}

#[tokio::test]
async fn append_outside_the_capability_is_denied() {
    let (tape, key) = with_memory_caps(async {
        let tape = memory_tape();
        let id = tape
            .register(
                "SPY",
                VENUE,
                crate::catalog::AssetClass::Stock,
                crate::catalog::PriceScale::of(2).expect("valid scale"),
                "UTC",
                protopyne_types::Timeframe::M1,
            )
            .expect("register");
        (tape, key_of(id))
    })
    .await;
    // No capability installed here: the gate fires before the database.
    let err = tape
        .append(
            key,
            &[at_ms(1_767_225_900_000)],
            WritePolicy::InsertIfAbsent,
        )
        .expect_err("ungranted append");
    assert!(
        matches!(err, StoreError::Denied(_)),
        "an ungranted task is denied, not defaulted: {err:?}"
    );
}

#[tokio::test]
async fn coverage_outside_the_capability_is_denied() {
    let (tape, key) = with_memory_caps(async {
        let tape = memory_tape();
        let id = tape
            .register(
                "SPY",
                VENUE,
                crate::catalog::AssetClass::Stock,
                crate::catalog::PriceScale::of(2).expect("valid scale"),
                "UTC",
                protopyne_types::Timeframe::M1,
            )
            .expect("register");
        (tape, key_of(id))
    })
    .await;
    let err = tape.coverage(key).expect_err("ungranted coverage");
    assert!(
        matches!(err, StoreError::Denied(_)),
        "reads are effects too: {err:?}"
    );
}

#[tokio::test]
async fn empty_capability_set_is_denied() {
    let (tape, key) = with_memory_caps(async {
        let tape = memory_tape();
        let id = tape
            .register(
                "SPY",
                VENUE,
                crate::catalog::AssetClass::Stock,
                crate::catalog::PriceScale::of(2).expect("valid scale"),
                "UTC",
                protopyne_types::Timeframe::M1,
            )
            .expect("register");
        (tape, key_of(id))
    })
    .await;
    // A set is installed, but it carries no store token: still denied.
    let caps = fluent_wvr::CapabilitySet::new();
    let err = fluent_wvr::CURRENT_CAPS
        .scope(caps, async {
            tape.append(
                key,
                &[at_ms(1_767_225_900_000)],
                WritePolicy::InsertIfAbsent,
            )
        })
        .await
        .expect_err("tokenless set");
    assert!(
        matches!(err, StoreError::Denied(_)),
        "presence of a set is not presence of the token: {err:?}"
    );
}

#[tokio::test]
async fn granted_task_operates_normally() {
    // The positive control: the same operations succeed with the token
    // installed, so the denials above prove the gate and not a broken
    // store.
    with_memory_caps(async {
        let tape = memory_tape();
        let id = tape
            .register(
                "SPY",
                VENUE,
                crate::catalog::AssetClass::Stock,
                crate::catalog::PriceScale::of(2).expect("valid scale"),
                "UTC",
                protopyne_types::Timeframe::M1,
            )
            .expect("register");
        let key = key_of(id);
        let outcome = tape
            .append(
                key,
                &[at_ms(1_767_225_900_000)],
                WritePolicy::InsertIfAbsent,
            )
            .expect("granted append");
        assert_eq!(outcome.inserted, 1);
        assert!(tape.coverage(key).expect("granted coverage").is_some());
    })
    .await;
}
