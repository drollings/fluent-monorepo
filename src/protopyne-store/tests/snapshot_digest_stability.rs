//! Snapshot digest stability: same bytes, same digest; different bytes,
//! different digest.
//!
//! A sealed artifact is content-addressed: its digest is what a rerun is
//! asserted against. Sealing the same series twice must produce the same
//! digest — the write is deterministic — and sealing a different series must
//! produce a different one. The manifest records that digest, so a rerun
//! comparing digests compares the data, not a wall clock.

use super::*;
use crate::manifest;
use crate::snapshot::test_snapshot::{SEAL_NOW_MS, varied_bars, with_seeded_tape};

#[tokio::test]
async fn same_series_seals_to_the_same_digest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bars = varied_bars(32);
    let path = dir.path().to_path_buf();
    with_seeded_tape("STABLE", 2, &bars, |tape, key| {
        let first = seal(
            tape.catalog(),
            key,
            &path,
            SealOptions::new().build(),
            SEAL_NOW_MS,
        )
        .expect("first seal");
        let second = seal(
            tape.catalog(),
            key,
            &path,
            SealOptions::new().build(),
            SEAL_NOW_MS,
        )
        .expect("second seal");
        assert_eq!(first.digest, second.digest, "the write is deterministic");
        assert_eq!(first.byte_len, second.byte_len, "and byte-stable");
        let recorded = {
            let conn = common_core::sync::lock(tape.catalog().connection());
            manifest::record_for(&conn, key.symbol_id, key.timeframe, &first.path.to_string_lossy())
                .expect("manifest read")
                .expect("a row was committed")
        };
        assert_eq!(recorded.digest, first.digest, "the digest is recorded");
        assert_eq!(recorded.row_count, bars.len() as u64);
    })
    .await;
}

#[tokio::test]
async fn different_series_seals_to_a_different_digest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().to_path_buf();
    let left = {
        let bars = varied_bars(16);
        let path = path.join("left");
        with_seeded_tape("LEFT", 2, &bars, |tape, key| {
            seal(
                tape.catalog(),
                key,
                &path,
                SealOptions::new().build(),
                SEAL_NOW_MS,
            )
            .expect("left seal")
            .digest
        })
        .await
    };
    let right = {
        let mut bars = varied_bars(16);
        for bar in &mut bars {
            bar.c += rust_decimal::Decimal::new(1, 2);
        }
        let path = path.join("right");
        with_seeded_tape("RIGHT", 2, &bars, |tape, key| {
            seal(
                tape.catalog(),
                key,
                &path,
                SealOptions::new().build(),
                SEAL_NOW_MS,
            )
            .expect("right seal")
            .digest
        })
        .await
    };
    assert_ne!(left, right, "different content, different digest");
}
