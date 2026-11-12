//! Per-fragment embedding status: which input the stored vector (or the
//! failure) came from, and which embedder produced it. The sync gate
//! consults it without re-ingesting: unchanged files skip even when
//! unembedded, failures retry only on input or embedder change, and an
//! embedder rotation backfills exactly the files whose recorded embedder
//! differs.

/// One fragment's embedding provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Per-fragment embedding status: which input the stored vector (or the
/// failure) came from, and which embedder produced it. The sync gate
/// consults it without re-ingesting: unchanged files skip even when
/// unembedded, failures retry only on input or embedder change, and an
/// embedder rotation backfills exactly the files whose recorded embedder
/// differs.
/// One fragment's embedding provenance.
pub struct EmbeddingStatusRecord {
    /// Owning fragment id.
    pub fragment_id: String,
    /// Blake3 of the embedded input text.
    pub input_hash: String,
    /// Embedder identity (`name:dimensions`).
    pub embedder: String,
    /// `ready` | `failed` | `unavailable`.
    pub status: String,
    /// Last update time (ms epoch).
    pub updated_at: i64,
}

impl EmbeddingStatusRecord {
    pub(crate) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            fragment_id: row.get(0)?,
            input_hash: row.get(1)?,
            embedder: row.get(2)?,
            status: row.get(3)?,
            updated_at: row.get(4)?,
        })
    }
}

impl super::GuidanceDb {
    /// Insert or replace embedding status rows.
    pub fn replace_embedding_status(
        &self,
        rows: &[EmbeddingStatusRecord],
    ) -> Result<usize, super::VectorDbError> {
        self.store
            .transaction(|tx| {
                for row in rows {
                    fluent_db::query::execute(
                        tx,
                        "INSERT OR REPLACE INTO fragment_embedding_status
                         (fragment_id, input_hash, embedder, status, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        rusqlite::params![
                            row.fragment_id,
                            row.input_hash,
                            row.embedder,
                            row.status,
                            row.updated_at,
                        ],
                    )?;
                }
                Ok(rows.len())
            })
            .map_err(super::VectorDbError::from)
    }

    /// Status rows for one file's fragments.
    pub fn embedding_status_for_file(
        &self,
        file_id: &str,
    ) -> Result<Vec<EmbeddingStatusRecord>, super::VectorDbError> {
        self.store
            .query_rows(
                "SELECT s.fragment_id, s.input_hash, s.embedder, s.status, s.updated_at
                 FROM fragment_embedding_status s
                 JOIN fragments f ON f.id = s.fragment_id
                 WHERE f.file_id = ?1
                 ORDER BY s.fragment_id",
                &[&file_id],
                EmbeddingStatusRecord::from_row,
            )
            .map_err(super::VectorDbError::from)
    }

    /// Files with a recorded embedder different from the current one:
    /// rotation backfill seeds. Files without rows (never attempted, live
    /// only, legacy) are left alone — unknown is not a backfill trigger.
    pub fn files_needing_embed_backfill(
        &self,
        embedder: &str,
    ) -> Result<Vec<String>, super::VectorDbError> {
        self.store
            .query_rows(
                "SELECT DISTINCT f.file_id
                 FROM fragment_embedding_status s
                 JOIN fragments f ON f.id = s.fragment_id
                 WHERE s.embedder != ?1
                 ORDER BY f.file_id",
                &[&embedder],
                |row| row.get::<_, String>(0),
            )
            .map_err(super::VectorDbError::from)
    }
}