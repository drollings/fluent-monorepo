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
    /// `ready` | `skipped` | `failed` | `unavailable`.
    pub status: String,
    /// Why the fragment is in this state (empty for `ready`).
    pub reason: String,
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
            reason: row.get(4)?,
            updated_at: row.get(5)?,
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
                         (fragment_id, input_hash, embedder, status, reason, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        rusqlite::params![
                            row.fragment_id,
                            row.input_hash,
                            row.embedder,
                            row.status,
                            row.reason,
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
                "SELECT s.fragment_id, s.input_hash, s.embedder, s.status, s.reason, s.updated_at
                 FROM fragment_embedding_status s
                 JOIN fragments f ON f.id = s.fragment_id
                 WHERE f.file_id = ?1
                 ORDER BY s.fragment_id",
                &[&file_id],
                EmbeddingStatusRecord::from_row,
            )
            .map_err(super::VectorDbError::from)
    }

    /// Status rows for explicit fragment ids (the backfill unchanged
    /// gate reads these, never the fragment bytes).
    pub fn embedding_status_for_ids(
        &self,
        ids: &[String],
    ) -> Result<Vec<EmbeddingStatusRecord>, super::VectorDbError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        let sql = format!(
            "SELECT fragment_id, input_hash, embedder, status, reason, updated_at
             FROM fragment_embedding_status WHERE fragment_id IN ({placeholders})"
        );
        let values: Vec<rusqlite::types::Value> = ids
            .iter()
            .map(|id| rusqlite::types::Value::Text(id.clone()))
            .collect();
        self.store
            .with_conn(|conn| {
                fluent_db::query::query_rows_from_iter(
                    conn,
                    &sql,
                    values,
                    EmbeddingStatusRecord::from_row,
                )
            })
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
