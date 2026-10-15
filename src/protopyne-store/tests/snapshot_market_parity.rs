//! Sealed-artifact parity: the snapshot reader and the market reader agree
//! on every OHLCV field of a real sealed artifact.
//!
//! The fixture is sealed by the real [`SnapshotWriter`](super::SnapshotWriter)
//! path, not hand-built, so the market reader proves itself against the exact
//! bytes the store addresses by digest. The market reader drops `adj` by
//! contract — the snapshot reader carries it — which the test asserts
//! explicitly rather than letting the comparison hide it.

use super::test_snapshot::{SEAL_NOW_MS, varied_bars, with_seeded_tape};
use super::{SealOptions, read_parquet, seal};
use crate::market_parquet::read_ohlcv_parquet;

#[tokio::test]
async fn sealed_artifact_reads_identically_through_both_readers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut bars = varied_bars(16);
    bars[5].adj = Some(
        protopyne_types::Adjustment::new()
            .split_factor("2".parse().expect("valid decimal"))
            .build(),
    );
    let source = bars.clone();
    with_seeded_tape("PARITY", 2, &bars, |tape, key| {
        let sealed = seal(
            tape.catalog(),
            key,
            dir.path(),
            SealOptions::new().build(),
            SEAL_NOW_MS,
        )
        .expect("seal succeeds");
        let via_snapshot = read_parquet(&sealed.path).expect("snapshot reader reads");
        assert_eq!(via_snapshot, source, "the sealed bars keep every field");
        assert!(
            via_snapshot.iter().any(|bar| bar.adj.is_some()),
            "the fixture must carry an adjustment for the drop assertion"
        );

        let via_market =
            read_ohlcv_parquet(&sealed.path, None).expect("market reader reads the sealed shape");
        assert_eq!(via_market.restatements_merged, 0);
        assert_eq!(via_market.rows_filtered, 0);
        let stripped: Vec<protopyne_types::Bar> = via_snapshot
            .iter()
            .map(|bar| {
                let mut plain = bar.clone();
                plain.adj = None;
                plain
            })
            .collect();
        assert_eq!(
            via_market.bars, stripped,
            "every OHLCV field agrees, in order"
        );
        assert!(
            via_market.bars.iter().all(|bar| bar.adj.is_none()),
            "the market reader drops `adj` by contract"
        );
    })
    .await;
}
