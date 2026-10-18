//! The capability gate: ungranted tasks are denied, not defaulted.
//!
//! Store effects check the task-local capability set before touching the
//! tape. Outside a set carrying the store token, every gated operation
//! refuses with `Denied` — reads included, because a read that reaches the
//! tape is an effect. This suite proves the denial from both sides:
//! without the token it fails, and the failure names the gate rather than
//! masquerading as an empty store. The binding half pins the multi-store
//! case: a permit minted for one root never operates another, refused on
//! its face before presence is even consulted.

use super::*;
use protopyne_types::Timeframe;

use crate::commit::test_tapes::at_ms;
use crate::key::{SymbolId, TapeKey};
use crate::store::{CandleStore, WritePolicy};
use crate::tape::test_setup::{memory_tape, with_registry_caps, VENUE};

fn key_of(id: SymbolId) -> TapeKey {
    TapeKey {
        symbol_id: id,
        timeframe: Timeframe::M1,
    }
}

#[tokio::test]
async fn append_outside_the_capability_is_denied() {
    let (tape, key) = with_registry_caps(async {
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
    // No capability installed here: the gate fires before the tape.
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
    let (tape, key) = with_registry_caps(async {
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
    let (tape, key) = with_registry_caps(async {
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
    with_registry_caps(async {
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

#[tokio::test]
async fn scoped_read_with_a_foreign_permit_is_refused_on_its_face() {
    use crate::range::TimeRange;
    use crate::store::ReadRequest;
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("store");
    let foreign = dir.path().join("other");
    // Seed one bar under the root-bound token.
    let caps =
        fluent_wvr::CapabilitySet::new().with(crate::capability::StoreStorage::new(root.clone()));
    fluent_wvr::CURRENT_CAPS
        .scope(caps, async {
            let tape = crate::tape::MarketTape::open(&root).expect("tape opens");
            let symbol_id = tape
                .register(
                    "SPY",
                    VENUE,
                    crate::catalog::AssetClass::Stock,
                    crate::catalog::PriceScale::of(2).expect("valid scale"),
                    "UTC",
                    Timeframe::M1,
                )
                .expect("register");
            tape.append(
                crate::key::TapeKey {
                    symbol_id,
                    timeframe: Timeframe::M1,
                },
                &[at_ms(1_767_225_900_000)],
                WritePolicy::InsertIfAbsent,
            )
            .expect("seed appends");
        })
        .await;
    let tape = crate::tape::MarketTape::open(&root).expect("tape reopens");
    let req = ReadRequest {
        symbol: "SPY".to_owned(),
        venue: VENUE.to_owned(),
        timeframe: Timeframe::M1,
        range: TimeRange::unbounded(),
        limit: Some(1),
        policy: crate::fold::FoldPolicy::ExtendToFullBuckets,
    };
    // No capability installed at all: binding runs before presence, so the
    // foreign permit is refused on its face.
    let foreign_permit = crate::capability::StoreStorage::new(foreign);
    let err = tape
        .read_scoped(&foreign_permit, &req)
        .expect_err("foreign permit");
    assert!(
        matches!(err, StoreError::Denied(_)),
        "a permit for another root never operates this store: {err:?}"
    );
}

#[tokio::test]
async fn scoped_read_with_the_bound_permit_serves() {
    use crate::range::TimeRange;
    use crate::store::ReadRequest;
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("store");
    let permit = crate::capability::StoreStorage::new(root.clone());
    let caps = fluent_wvr::CapabilitySet::new().with(permit.clone());
    let bars = fluent_wvr::CURRENT_CAPS
        .scope(caps, async {
            let tape = crate::tape::MarketTape::open(&root).expect("tape opens");
            let symbol_id = tape
                .register(
                    "SPY",
                    VENUE,
                    crate::catalog::AssetClass::Stock,
                    crate::catalog::PriceScale::of(2).expect("valid scale"),
                    "UTC",
                    Timeframe::M1,
                )
                .expect("register");
            tape.append(
                crate::key::TapeKey {
                    symbol_id,
                    timeframe: Timeframe::M1,
                },
                &[at_ms(1_767_225_900_000)],
                WritePolicy::InsertIfAbsent,
            )
            .expect("seed appends");
            let series = tape
                .read_scoped(
                    &permit,
                    &ReadRequest {
                        symbol: "SPY".to_owned(),
                        venue: VENUE.to_owned(),
                        timeframe: Timeframe::M1,
                        range: TimeRange::unbounded(),
                        limit: Some(1),
                        policy: crate::fold::FoldPolicy::ExtendToFullBuckets,
                    },
                )
                .expect("bound permit serves");
            series.bars.len()
        })
        .await;
    assert_eq!(bars, 1, "the bound permit reads what it seeded");
}
