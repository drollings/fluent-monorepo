//! The JSON seal manifest contract: round-trip, digest verification, and
//! the durable file shape.
//!
//! The manifest row keeps its SQL-era shape field for field; only the
//! persistence changes (one JSON file beside the artifacts, written
//! atomically). A digest mismatch and an unmanifested artifact both refuse
//! loudly. The `manifest.json` shape is pinned so an operator can read it.

use crate::error::{SnapshotError, StoreError};
use crate::key::TapeKey;
use crate::registry::test_setup::{register_defaults, with_registry_caps, VENUE};
use crate::registry::MarketRegistry;
use crate::snapshot::test_snapshot::{varied_bars, SEAL_NOW_MS};
use crate::snapshot::{read_snapshot, reconcile_orphans, seal, SealOptions, SealedSnapshot};

fn seal_one(
    registry: &MarketRegistry,
    symbol: &str,
    bars: &[protopyne_types::Bar],
    out_dir: &std::path::Path,
) -> (TapeKey, SealedSnapshot) {
    let symbol_id = register_defaults(registry, symbol, VENUE).expect("register");
    let key = TapeKey {
        symbol_id,
        timeframe: protopyne_types::Timeframe::M1,
    };
    let sealed = seal(
        registry,
        key,
        bars,
        out_dir,
        SealOptions::new().build(),
        SEAL_NOW_MS,
    )
    .expect("seal succeeds");
    (key, sealed)
}

#[tokio::test]
async fn sealed_then_read_changes_not_one_value() {
    let dir = tempfile::tempdir().expect("tempdir");
    let seals = dir.path().join("seals");
    let bars = varied_bars(16);
    let source = bars.clone();
    with_registry_caps(async {
        let registry = MarketRegistry::open_in_memory().expect("registry opens");
        let (key, sealed) = seal_one(&registry, "ROUND", &bars, &seals);
        assert_eq!(sealed.row_count, source.len() as u64);
        let read = read_snapshot(&registry, key, &sealed.path).expect("read back");
        assert_eq!(read, source, "the round-trip is the identity");
    })
    .await;
}

#[tokio::test]
async fn manifest_row_with_missing_file_is_loud() {
    let dir = tempfile::tempdir().expect("tempdir");
    let seals = dir.path().join("seals");
    let bars = varied_bars(8);
    with_registry_caps(async {
        let registry = MarketRegistry::open_in_memory().expect("registry opens");
        let (key, sealed) = seal_one(&registry, "CRASH", &bars, &seals);
        std::fs::remove_file(&sealed.path).expect("remove artifact");
        match read_snapshot(&registry, key, &sealed.path) {
            Err(StoreError::Snapshot(SnapshotError::MissingArtifact { path })) => {
                assert_eq!(path, sealed.path.to_string_lossy().into_owned());
            }
            other => panic!("expected a loud missing-artifact error, got {other:?}"),
        }
    })
    .await;
}

#[tokio::test]
async fn file_without_a_manifest_row_is_loud() {
    let dir = tempfile::tempdir().expect("tempdir");
    let seals = dir.path().join("seals");
    let bars = varied_bars(8);
    with_registry_caps(async {
        let registry = MarketRegistry::open_in_memory().expect("registry opens");
        let (key, sealed) = seal_one(&registry, "NOMAN", &bars, &seals);
        let stray = seals.join("stray.parquet");
        std::fs::copy(&sealed.path, &stray).expect("copy stray");
        match read_snapshot(&registry, key, &stray) {
            Err(StoreError::Snapshot(SnapshotError::Unmanifested { path })) => {
                assert_eq!(path, stray.to_string_lossy().into_owned());
            }
            other => panic!("expected a loud unmanifested error, got {other:?}"),
        }
    })
    .await;
}

#[tokio::test]
async fn tampered_artifact_refuses_on_its_digest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let seals = dir.path().join("seals");
    let bars = varied_bars(8);
    with_registry_caps(async {
        let registry = MarketRegistry::open_in_memory().expect("registry opens");
        let (key, sealed) = seal_one(&registry, "TAMPER", &bars, &seals);
        let mut bytes = std::fs::read(&sealed.path).expect("artifact reads");
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        std::fs::write(&sealed.path, &bytes).expect("tamper lands");
        match read_snapshot(&registry, key, &sealed.path) {
            Err(StoreError::Snapshot(SnapshotError::Artifact { reason })) => {
                assert!(
                    reason.contains("digest mismatch"),
                    "the refusal names the digest: {reason}"
                );
            }
            other => panic!("expected a loud digest-mismatch error, got {other:?}"),
        }
    })
    .await;
}

#[tokio::test]
async fn orphan_file_is_reported_and_removed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let seals = dir.path().join("seals");
    let bars = varied_bars(8);
    with_registry_caps(async {
        let registry = MarketRegistry::open_in_memory().expect("registry opens");
        let (_, sealed) = seal_one(&registry, "ORPHAN", &bars, &seals);
        let orphan = seals.join("orphan.parquet");
        std::fs::copy(&sealed.path, &orphan).expect("copy orphan");
        let report = reconcile_orphans(&registry, &seals).expect("reconcile");
        assert_eq!(
            report.orphans_removed,
            vec![orphan.to_string_lossy().into_owned()],
            "the orphan is reported and removed"
        );
        assert!(!orphan.exists(), "the orphan file is gone");
        assert!(sealed.path.exists(), "the manifest-backed artifact remains");
        assert_eq!(report.rows_checked, 1, "one manifest row was seen");
    })
    .await;
}

#[tokio::test]
async fn manifest_round_trip_through_reopen() {
    // The manifest is file-backed: a second registry over the same seals
    // directory reads the rows the first seal committed.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("store");
    let seals = dir.path().join("seals");
    let bars = varied_bars(8);
    let artifact = with_registry_caps(async {
        let registry = MarketRegistry::open(&root).expect("registry opens");
        let (_, sealed) = seal_one(&registry, "REOPEN", &bars, &seals);
        sealed.path.to_string_lossy().into_owned()
    })
    .await;
    assert!(
        seals.join("manifest.json").is_file(),
        "the manifest persists as JSON"
    );
    with_registry_caps(async {
        let registry = MarketRegistry::open(&root).expect("registry reopens");
        let symbol_id = registry
            .resolve("REOPEN", VENUE)
            .expect("resolve")
            .expect("known");
        let key = TapeKey {
            symbol_id,
            timeframe: protopyne_types::Timeframe::M1,
        };
        let read = read_snapshot(&registry, key, std::path::Path::new(&artifact))
            .expect("the row survives the reopen");
        assert_eq!(read, bars, "the artifact still verifies after reopen");
    })
    .await;
}

#[tokio::test]
async fn manifest_crash_before_rename_keeps_the_old_rows() {
    // A leftover temp sibling never becomes the manifest: the committed
    // rows still read.
    let dir = tempfile::tempdir().expect("tempdir");
    let seals = dir.path().join("seals");
    let bars = varied_bars(8);
    with_registry_caps(async {
        let registry = MarketRegistry::open_in_memory().expect("registry opens");
        let (key, sealed) = seal_one(&registry, "OLDROW", &bars, &seals);
        std::fs::write(seals.join("manifest.json.999.0.1.tmp"), b"{corrupt!!!")
            .expect("stray temp lands");
        let read = read_snapshot(&registry, key, &sealed.path).expect("old row still reads");
        assert_eq!(read, bars);
    })
    .await;
}

#[tokio::test]
async fn the_manifest_json_shape_is_pinned() {
    let dir = tempfile::tempdir().expect("tempdir");
    let seals = dir.path().join("seals");
    let bars = varied_bars(4);
    with_registry_caps(async {
        let registry = MarketRegistry::open_in_memory().expect("registry opens");
        seal_one(&registry, "SHAPE", &bars, &seals);
    })
    .await;
    let text = std::fs::read_to_string(seals.join("manifest.json")).expect("manifest reads");
    let value: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    let records = value
        .get("records")
        .and_then(serde_json::Value::as_array)
        .expect("records array");
    assert_eq!(records.len(), 1);
    let record = &records[0];
    for field in [
        "symbol_id",
        "timeframe",
        "path",
        "first_ts_ms",
        "last_ts_ms",
        "row_count",
        "byte_len",
        "digest",
        "written_at_ms",
    ] {
        assert!(
            record.get(field).is_some(),
            "the manifest row keeps its SQL-era field {field:?}: {text}"
        );
    }
    assert_eq!(
        record.get("row_count").and_then(serde_json::Value::as_u64),
        Some(4)
    );
    assert_eq!(
        record
            .get("written_at_ms")
            .and_then(serde_json::Value::as_i64),
        Some(SEAL_NOW_MS)
    );
}
