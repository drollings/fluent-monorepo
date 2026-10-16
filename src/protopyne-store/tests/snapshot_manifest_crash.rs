//! Snapshot crash recovery: a missing file is loud, an orphan is reported.
//!
//! Two failure shapes from an interrupted seal, each handled distinctly. A
//! manifest row whose file is gone is a **loud** error — a promise the store
//! cannot keep must never read as an empty series. A file with no manifest
//! row is an orphan from a seal that renamed its artifact but never committed
//! its row; [`reconcile_orphans`] reports and removes it, and nothing else
//! changes.

use super::*;
use crate::error::{SnapshotError, StoreError};
use crate::snapshot::test_snapshot::{SEAL_NOW_MS, varied_bars, with_seeded_tape};

#[tokio::test]
async fn orphan_file_is_reported_and_removed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bars = varied_bars(8);
    let path = dir.path().to_path_buf();
    with_seeded_tape("ORPHAN", 2, &bars, |registry, key| {
        let sealed = seal(
            registry,
            key,
            &bars,
            &path,
            SealOptions::new().build(),
            SEAL_NOW_MS,
        )
        .expect("seal succeeds");
        // A file with no manifest row: a byte-identical copy under a new name.
        let orphan = path.join("orphan.parquet");
        std::fs::copy(&sealed.path, &orphan).expect("copy orphan");
        let report = reconcile_orphans(registry, &path).expect("reconcile");
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
async fn manifest_row_with_missing_file_is_loud() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bars = varied_bars(8);
    let path = dir.path().to_path_buf();
    with_seeded_tape("CRASH", 2, &bars, |registry, key| {
        let sealed = seal(
            registry,
            key,
            &bars,
            &path,
            SealOptions::new().build(),
            SEAL_NOW_MS,
        )
        .expect("seal succeeds");
        std::fs::remove_file(&sealed.path).expect("remove artifact");
        match read_snapshot(registry, key, &sealed.path) {
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
    let bars = varied_bars(8);
    let path = dir.path().to_path_buf();
    with_seeded_tape("NOMAN", 2, &bars, |registry, key| {
        let sealed = seal(
            registry,
            key,
            &bars,
            &path,
            SealOptions::new().build(),
            SEAL_NOW_MS,
        )
        .expect("seal succeeds");
        let stray = path.join("stray.parquet");
        std::fs::copy(&sealed.path, &stray).expect("copy stray");
        match read_snapshot(registry, key, &stray) {
            Err(StoreError::Snapshot(SnapshotError::Unmanifested { path })) => {
                assert_eq!(path, stray.to_string_lossy().into_owned());
            }
            other => panic!("expected a loud unmanifested error, got {other:?}"),
        }
    })
    .await;
}
