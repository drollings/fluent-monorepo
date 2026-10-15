//! Coverage answers, including the answer a broken store gives.
//!
//! `Ok(None)` means the store answered "no rows". It must never mean
//! "the store could not be reached" — that confusion is what triggers a
//! multi-year backfill on the next repair. This suite pins both sides: an
//! empty table answers `None`, a one-bar table answers `Some` with
//! `first == last`, and a store whose table was dropped out from under it
//! answers `Err`, loudly, instead of laundering the failure as emptiness.

use super::*;

use crate::commit::test_tapes::at_ms;
use crate::sqlite_tape::test_setup::{seeded, with_memory_caps};

fn grid(first_ms: i64, count: i64) -> Vec<Bar> {
    (0..count)
        .map(|step| at_ms(first_ms + step * 60_000))
        .collect()
}

#[tokio::test]
async fn empty_table_answers_none() {
    with_memory_caps(async {
        let (tape, key) = seeded("SPY", &[]).await;
        assert_eq!(tape.coverage(key).expect("coverage"), None);
    })
    .await;
}

#[tokio::test]
async fn one_bar_table_covers_exactly_that_bar() {
    with_memory_caps(async {
        let (tape, key) = seeded("SPY", &grid(1_767_225_900_000, 1)).await;
        let coverage = tape.coverage(key).expect("coverage").expect("covered");
        assert_eq!(coverage.first, coverage.last);
        assert_eq!(coverage.first.ts_ms, 1_767_225_900_000);
        assert_eq!(coverage.bars, 1);
    })
    .await;
}

#[tokio::test]
async fn coverage_last_is_the_max_stamp() {
    with_memory_caps(async {
        let (tape, key) = seeded("SPY", &grid(1_767_225_900_000, 10)).await;
        let coverage = tape.coverage(key).expect("coverage").expect("covered");
        assert_eq!(coverage.first.ts_ms, 1_767_225_900_000);
        assert_eq!(coverage.last.ts_ms, 1_767_225_900_000 + 9 * 60_000);
        assert_eq!(coverage.bars, 10);
    })
    .await;
}

#[tokio::test]
async fn broken_store_answers_err_never_none() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let tape = with_memory_caps(async {
        let tape = SqliteTape::open(&path).expect("file tape opens");
        let permit = crate::capability::StoreStorage::new(path.clone());
        let caps = fluent_wvr::CapabilitySet::new().with(permit);
        fluent_wvr::CURRENT_CAPS
            .scope(caps, async {
                tape.register(
                    "SPY",
                    "test-venue",
                    crate::catalog::AssetClass::Stock,
                    crate::catalog::PriceScale::of(2).expect("valid scale"),
                    "UTC",
                    protopyne_types::Timeframe::M1,
                )
                .expect("register");
                tape
            })
            .await
    })
    .await;
    // Break the store out from under the open handle: the table the
    // coverage query reads is gone.
    rusqlite::Connection::open(&path)
        .expect("second connection")
        .execute_batch("DROP TABLE candle;")
        .expect("drop");
    let key = with_memory_caps(async {
        tape.resolve("SPY", "test-venue")
            .expect("resolve")
            .expect("known")
    })
    .await;
    let key = crate::key::TapeKey {
        symbol_id: key,
        timeframe: protopyne_types::Timeframe::M1,
    };
    let err = with_memory_caps(async { tape.coverage(key) })
        .await
        .expect_err("broken store errs");
    assert!(
        matches!(err, StoreError::Sqlite(_)),
        "a store that cannot answer errors; it never reads as empty: {err:?}"
    );
}

#[tokio::test]
async fn garbage_file_refuses_at_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    std::fs::write(&path, b"this is not a database").expect("write garbage");
    let err = SqliteTape::open(&path).expect_err("garbage refuses");
    assert!(
        matches!(err, StoreError::Sqlite(_)),
        "a corrupt file fails at open, never as a silent empty store: {err:?}"
    );
}
