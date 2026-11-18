//! Sealed-snapshot manifest: one durable row per sealed Parquet artifact.
//!
//! A sealed snapshot is a byte-stable, content-addressed run input — the
//! property CSV cannot provide (no schema, no guaranteed ordering, no stable
//! digest). The manifest records what was sealed, where it lives, and the
//! digest that proves it: `(symbol_id, timeframe, path)` is the key, and the
//! coverage columns let a read planner reason about a snapshot without
//! opening it.
//!
//! This is deliberately **not** a tier system. There is one snapshot format,
//! one digest, one coverage row per artifact; the manifest exists so
//! [`ReadPlan`](crate::plan::ReadPlan) can stay pure, not so storage can be
//! layered.
//!
//! The manifest is one JSON document beside the artifacts
//! (`<seals>/manifest.json`), written atomically with the pattern from
//! [`common_core::io::write_atomic`]: readers always observe the old or the
//! new row set, never a mix. A seal writes its artifact (tmp, fsync, rename)
//! and only then upserts and persists its row. A crash can therefore produce
//! an orphan file (no row), which
//! [`reconcile_orphans`](crate::snapshot::reconcile_orphans) removes loudly,
//! but never a manifest row pointing at a missing or partial file. The
//! digest-verification contract of [`read_snapshot`](crate::snapshot::read_snapshot)
//! is unchanged: manifest row and file must both exist and agree.

use std::path::Path;

use protopyne_types::Timeframe;

use crate::error::StoreError;
use crate::key::SymbolId;

/// File name of the persisted manifest inside a seals directory.
pub const MANIFEST_FILE: &str = "manifest.json";

/// One sealed artifact's manifest row. The shape is the SQL-era row field
/// for field, so an export reads identically.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SnapshotRecord {
    /// The instrument the sealed tape belongs to.
    pub symbol_id: SymbolId,
    /// The bar spacing of the sealed tape.
    pub timeframe: Timeframe,
    /// The artifact path, as sealed.
    pub path: String,
    /// The first bar stamp in the artifact.
    pub first_ts_ms: i64,
    /// The last bar stamp in the artifact.
    pub last_ts_ms: i64,
    /// Rows in the artifact.
    pub row_count: u64,
    /// Artifact size in bytes.
    pub byte_len: u64,
    /// The content digest (BLAKE3 hex) of the artifact.
    pub digest: String,
    /// The wall-clock instant the seal committed, epoch milliseconds.
    pub written_at_ms: i64,
}

/// The file-backed manifest: every sealed artifact's row, keyed by
/// `(symbol_id, timeframe, path)`.
///
/// Loaded once per seal operation from `<seals>/manifest.json` (or empty
/// when no seal has committed yet); [`upsert`](Self::upsert) replaces any
/// row for the same key, and [`save`](Self::save) persists atomically.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SealManifest {
    /// Every manifest row, in key order.
    #[serde(default)]
    records: Vec<SnapshotRecord>,
}

impl SealManifest {
    /// The manifest file inside `seals_dir`.
    #[must_use]
    pub fn path_for(seals_dir: &Path) -> std::path::PathBuf {
        seals_dir.join(MANIFEST_FILE)
    }

    /// Load the manifest beside the artifacts, or an empty one when no seal
    /// has committed yet. A file that exists but does not parse refuses
    /// loudly — falling back to empty would silently unmanifest every sealed
    /// artifact.
    pub fn load(seals_dir: &Path) -> Result<Self, StoreError> {
        let path = Self::path_for(seals_dir);
        match std::fs::read_to_string(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(StoreError::Io(common_core::error::IoError::from(e))),
            Ok(text) => serde_json::from_str(&text).map_err(|e| {
                StoreError::InvalidBar(format!("seal manifest {} is corrupt: {e}", path.display()))
            }),
        }
    }

    /// Persist the manifest atomically beside the artifacts.
    pub fn save(&self, seals_dir: &Path) -> Result<(), StoreError> {
        std::fs::create_dir_all(seals_dir).map_err(common_core::error::IoError::from)?;
        let mut ordered = self.records.clone();
        ordered.sort_by(|a, b| {
            (a.symbol_id, a.timeframe.code(), &a.path).cmp(&(
                b.symbol_id,
                b.timeframe.code(),
                &b.path,
            ))
        });
        let doc = Self { records: ordered };
        let text = serde_json::to_string_pretty(&doc).map_err(|e| {
            StoreError::InvalidBar(format!("seal manifest does not serialize: {e}"))
        })?;
        common_core::io::write_atomic(&Self::path_for(seals_dir), text.as_bytes())
            .map_err(|e| StoreError::Io(e.into()))?;
        Ok(())
    }

    /// Write the manifest row for a freshly sealed artifact, replacing any
    /// row for the same `(symbol_id, timeframe, path)`.
    pub fn upsert(&mut self, record: SnapshotRecord) {
        if let Some(held) = self.records.iter_mut().find(|held| {
            held.symbol_id == record.symbol_id
                && held.timeframe == record.timeframe
                && held.path == record.path
        }) {
            *held = record;
        } else {
            self.records.push(record);
        }
    }

    /// One manifest row for `(symbol_id, timeframe, path)`, or `None`.
    #[must_use]
    pub fn record_for(
        &self,
        symbol_id: SymbolId,
        timeframe: Timeframe,
        path: &str,
    ) -> Option<&SnapshotRecord> {
        self.records.iter().find(|record| {
            record.symbol_id == symbol_id && record.timeframe == timeframe && record.path == path
        })
    }

    /// Every manifest row, in key order.
    #[must_use]
    pub fn records(&self) -> &[SnapshotRecord] {
        &self.records
    }
}

#[cfg(all(test, feature = "parquet"))]
#[path = "../tests/manifest_json.rs"]
mod tests;
