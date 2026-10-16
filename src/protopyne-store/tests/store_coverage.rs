//! Coverage answers, including the answer a broken store gives.
//!
//! `Ok(None)` means the store answered "no rows". It must never mean
//! "the store could not be reached" — that confusion is what triggers a
//! multi-year backfill on the next repair. This suite pins both sides: an
//! empty tape answers `None`, a one-bar tape answers `Some` with
//! `first == last`, and a store whose catalog is corrupt refuses at open,
//! loudly, instead of laundering the failure as emptiness. (An unreachable
//! store answers `Denied` through the capability gate, pinned in
//! `capability_gate.rs`.)

use super::*;

use crate::commit::test_tapes::at_ms;
use crate::tape::test_setup::{seeded, with_registry_caps};

fn grid(first_ms: i64, count: i64) -> Vec<Bar> {
    (0..count)
        .map(|step| at_ms(first_ms + step * 60_000))
        .collect()
}

#[tokio::test]
async fn empty_table_answers_none() {
    with_registry_caps(async {
        let (tape, key) = seeded("SPY", &[]).await;
        assert_eq!(tape.coverage(key).expect("coverage"), None);
    })
    .await;
}

#[tokio::test]
async fn one_bar_table_covers_exactly_that_bar() {
    with_registry_caps(async {
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
    with_registry_caps(async {
        let (tape, key) = seeded("SPY", &grid(1_767_225_900_000, 10)).await;
        let coverage = tape.coverage(key).expect("coverage").expect("covered");
        assert_eq!(coverage.first.ts_ms, 1_767_225_900_000);
        assert_eq!(coverage.last.ts_ms, 1_767_225_900_000 + 9 * 60_000);
        assert_eq!(coverage.bars, 10);
    })
    .await;
}

#[tokio::test]
async fn garbage_catalog_refuses_at_open() {
    use crate::tape::MarketTape;
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("store");
    std::fs::create_dir_all(&root).expect("root creates");
    std::fs::write(root.join("catalog.json"), b"this is not a catalog")
        .expect("write garbage");
    let err = MarketTape::open(&root).expect_err("garbage refuses");
    assert!(
        matches!(err, StoreError::InvalidBar(_)),
        "a corrupt catalog fails at open, never as a silent empty store: {err:?}"
    );
}
