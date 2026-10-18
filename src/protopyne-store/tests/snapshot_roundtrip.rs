//! Snapshot round-trip: the control group is identity.
//!
//! Sealing a seeded tape and reading it back must change not one value — the
//! same order, the same `Decimal` spellings, the same corporate-action
//! adjustment. This is the control group that makes every other snapshot
//! claim meaningful: a seal that alters its input cannot be a reproducible
//! run input.

use super::*;
use crate::snapshot::test_snapshot::{varied_bars, with_seeded_tape, SEAL_NOW_MS};

/// The canonical stored spelling of every leg, in order — what the tape
/// keeps and what a sealed artifact must keep identically.
fn spellings(bars: &[protopyne_types::Bar]) -> Vec<String> {
    bars.iter()
        .flat_map(|bar| {
            [
                bar.ts.timestamp_millis().to_string(),
                bar.o.normalize().to_string(),
                bar.h.normalize().to_string(),
                bar.l.normalize().to_string(),
                bar.c.normalize().to_string(),
                bar.volume.to_string(),
            ]
        })
        .collect()
}

#[tokio::test]
async fn sealed_then_read_changes_not_one_value() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bars = varied_bars(64);
    let path = dir.path().to_path_buf();
    let source = bars.clone();
    with_seeded_tape("ROUND", 2, &bars, |registry, key| {
        let sealed = seal(
            registry,
            key,
            &bars,
            &path,
            SealOptions::new().build(),
            SEAL_NOW_MS,
        )
        .expect("seal succeeds");
        assert_eq!(sealed.row_count, source.len() as u64);
        assert_eq!(
            sealed.first_ts_ms,
            source.first().unwrap().ts.timestamp_millis()
        );
        assert_eq!(
            sealed.last_ts_ms,
            source.last().unwrap().ts.timestamp_millis()
        );
        let read = read_snapshot(registry, key, &sealed.path).expect("read back");
        assert_eq!(read, source, "the round-trip is the identity");
        assert_eq!(
            spellings(&read),
            spellings(&source),
            "byte-for-byte the same spellings, in the same order"
        );
    })
    .await;
}

#[tokio::test]
async fn corporate_action_adjustment_survives_the_round_trip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut bars = varied_bars(3);
    bars[1].adj = Some(
        protopyne_types::Adjustment::new()
            .split_factor("2".parse().expect("valid decimal"))
            .build(),
    );
    let path = dir.path().to_path_buf();
    let source = bars.clone();
    with_seeded_tape("ADJ", 2, &bars, |registry, key| {
        let sealed = seal(
            registry,
            key,
            &bars,
            &path,
            SealOptions::new().build(),
            SEAL_NOW_MS,
        )
        .expect("seal succeeds");
        let read = read_snapshot(registry, key, &sealed.path).expect("read back");
        assert_eq!(read, source, "the adjustment is carried, not dropped");
    })
    .await;
}
