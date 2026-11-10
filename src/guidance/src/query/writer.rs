//! Unified fragment writer seam: live (whole-file, vectors optional) and
//! batch (chunked, vectors required) over one `upsert_fragments` family.
//!
//! Readers dual-read both row shapes: recall serves file-level agreement
//! regardless of which writer committed the file. Rebuild converts v1 rows
//! by re-running the batch path from source, never by rewriting ids.

use search_vector::db::GuidanceDb;

use crate::query::db_storage::symbol_record;
use crate::query::ingest::{ingest_text_file, IngestStats};
use crate::search_types::{EntityFragment, FileInfo};

/// Writer schema version: v2 rows carry identity rows alongside fragments.
pub const WRITER_SCHEMA_VERSION: u32 = 2;

/// Stored row shape for one entity id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowShape {
    /// Whole-file v1 row (live writer, no chunk metadata required).
    WholeFile,
    /// Chunked v2 row (batch writer).
    Chunked,
    /// No row stored.
    Missing,
}

/// One seam for both writers; no second ingestion path.
pub struct FragmentWriter;

impl FragmentWriter {
    /// Live write: whole-file range, vectors optional, recall works with
    /// no embedding backend.
    pub fn write_live(
        db: &GuidanceDb,
        file: &FileInfo,
        text: &str,
        embedder: Option<&dyn fluent_llm::embeddings::EmbeddingProvider>,
        nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
    ) -> Result<IngestStats, crate::query::ingest::IngestError> {
        ingest_text_file(db, file, text, embedder, nlp)
    }

    /// Shape of one stored entity: whole-file, chunked, or missing.
    /// Dual-read input for migration decisions.
    #[must_use]
    pub fn row_shape(db: &GuidanceDb, entity_id: &str) -> RowShape {
        let Ok(rows) = db.fragments_for_entity(entity_id) else {
            return RowShape::Missing;
        };
        let Some(row) = rows.iter().find(|row| row.id == entity_id).or_else(|| rows.first()) else {
            return RowShape::Missing;
        };
        if row.symbol_name.is_some() || row.heading.is_some() {
            RowShape::Chunked
        } else {
            RowShape::WholeFile
        }
    }

    /// Whether a file committed by the live writer still needs the batch
    /// pass: fragments exist but no identity rows were stored.
    #[must_use]
    pub fn needs_rebuild(db: &GuidanceDb, file_id: &str, entity_id: &str) -> bool {
        if Self::row_shape(db, entity_id) != RowShape::WholeFile {
            return false;
        }
        db.symbols_for_file(file_id).is_ok_and(|rows| rows.is_empty())
    }

    /// Record identity rows for fragments already committed from the same
    /// harvest pass. Reads still serve both shapes afterwards.
    pub fn backfill_symbols(db: &GuidanceDb, fragments: &[EntityFragment]) -> Result<usize, String> {
        let rows: Vec<search_vector::db::SymbolRecord> =
            fragments.iter().filter_map(symbol_record).collect();
        db.replace_symbols(&rows).map_err(|e| format!("symbols: {e}"))
    }
}

#[cfg(test)]
#[path = "../../tests/writer_parity.rs"]
mod writer_parity_tests;
