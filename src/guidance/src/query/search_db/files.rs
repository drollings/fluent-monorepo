//! File-row and file-status operations over the guidance tables.
//!
//! Moved verbatim from `search-vector`: same SQL text, same behavior.

use std::collections::HashMap;

use rusqlite::params;

use super::fragments::{FileRecord, FragmentLemma, FragmentRecord};
use super::VectorDbError;

impl super::GuidanceSearchDb {
    /// One indexed file by id.
    pub fn get_file(&self, file_id: &str) -> Result<Option<FileRecord>, VectorDbError> {
        self.docs
            .sqlite_store()
            .query_row(
                "SELECT id, absolute_path, relative_path, root_path, size_bytes,
                        last_modified_time, kind, format, content_hash,
                        index_status, fail_count, last_error
                 FROM files WHERE id = ?1",
                params![file_id],
                FileRecord::from_row,
            )
            .map_err(VectorDbError::from)
    }

    /// All indexed files (filter resolution input).
    pub fn list_files(&self) -> Result<Vec<FileRecord>, VectorDbError> {
        self.docs
            .sqlite_store()
            .query_rows(
                "SELECT id, absolute_path, relative_path, root_path, size_bytes,
                        last_modified_time, kind, format, content_hash,
                        index_status, fail_count, last_error
                 FROM files ORDER BY id",
                &[],
                FileRecord::from_row,
            )
            .map_err(VectorDbError::from)
    }

    /// Stored source-content hashes (`files.content_hash`) for the
    /// member-JSON clock gate: absolute path → sha256 of the source bytes
    /// at ingest time. Empty when no row carries a hash — the gate then
    /// falls back to mtime behavior, never to a stale skip.
    pub fn source_content_hashes(&self) -> Result<HashMap<String, String>, VectorDbError> {
        let rows: Vec<(String, Option<String>)> = self
            .docs
            .sqlite_store()
            .query_rows(
                "SELECT absolute_path, content_hash FROM files WHERE content_hash IS NOT NULL",
                &[],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(VectorDbError::from)?;
        Ok(rows
            .into_iter()
            .filter_map(|(path, hash)| hash.map(|hash| (path, hash)))
            .collect())
    }

    /// Delete-by-file plus upsert of fragments/lemmas in one transaction
    /// (P2 commit primitive; failure is stored data via `mark_file_failed`).
    pub fn replace_file(
        &self,
        file: &FileRecord,
        fragments: &[FragmentRecord],
        lemmas: &[FragmentLemma],
    ) -> Result<usize, VectorDbError> {
        self.upsert_fragments(file, fragments, lemmas)
    }

    /// Record a per-file failure (stored data, never an aborted run).
    /// Upserts the file row so files that fail before their first commit
    /// (embed/prepare) are still tracked for the failed-retry pass.
    pub fn mark_file_failed(&self, file: &FileRecord, reason: &str) -> Result<(), VectorDbError> {
        self.docs
            .sqlite_store()
            .execute(
                "INSERT INTO files
                 (id, absolute_path, relative_path, root_path, size_bytes, last_modified_time,
                  kind, format, content_hash, index_status, fail_count, last_error)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'failed', 1, ?10)
                 ON CONFLICT(id) DO UPDATE SET
                   index_status = 'failed', last_error = ?10, fail_count = fail_count + 1",
                &[
                    &file.id as &dyn rusqlite::ToSql,
                    &file.absolute_path as &dyn rusqlite::ToSql,
                    &file.relative_path as &dyn rusqlite::ToSql,
                    &file.root_path as &dyn rusqlite::ToSql,
                    &(file.size_bytes as i64) as &dyn rusqlite::ToSql,
                    &file.last_modified_time as &dyn rusqlite::ToSql,
                    &file.kind as &dyn rusqlite::ToSql,
                    &file.format as &dyn rusqlite::ToSql,
                    &file.content_hash as &dyn rusqlite::ToSql,
                    &reason as &dyn rusqlite::ToSql,
                ],
            )
            .map(|_| ())
            .map_err(VectorDbError::from)
    }

    /// Drop the whole fragment index (workspace-index drop; the shared
    /// `nodes` document store is untouched).
    pub fn drop_index(&self) -> Result<(), VectorDbError> {
        self.docs
            .sqlite_store()
            .transaction(|tx| {
                for table in [
                    "fragment_lemmas",
                    "fragments",
                    "files",
                    "graph_edges",
                    "graph_symbols",
                    "symbols",
                ] {
                    fluent_db::query::execute(
                        tx,
                        &format!("DELETE FROM {table}"),
                        rusqlite::params![],
                    )?;
                }
                Ok(())
            })
            .map_err(VectorDbError::from)
    }

    /// Delete a file record and all its fragments/lemmas (P2 diff `deleted`),
    /// plus its persisted graph inputs (edges it imports, edges resolved
    /// to it, symbols it defines).
    pub fn delete_file(&self, file_id: &str) -> Result<(), VectorDbError> {
        let path: Option<String> = self
            .docs
            .sqlite_store()
            .query_row(
                "SELECT absolute_path FROM files WHERE id = ?1",
                params![file_id],
                |row| row.get(0),
            )
            .map_err(VectorDbError::from)?;
        self.docs.sqlite_store()
            .transaction(|tx| {
                fluent_db::query::execute(
                    tx,
                    "DELETE FROM fragment_lemmas WHERE fragment_rowid IN (SELECT rowid FROM fragments WHERE file_id = ?1)",
                    rusqlite::params![file_id],
                )?;
                fluent_db::query::execute(
                    tx,
                    "DELETE FROM fragments WHERE file_id = ?1",
                    rusqlite::params![file_id],
                )?;
                fluent_db::query::execute(
                    tx,
                    "DELETE FROM files WHERE id = ?1",
                    rusqlite::params![file_id],
                )?;
                if let Some(path) = &path {
                    fluent_db::query::execute(
                        tx,
                        "DELETE FROM graph_edges WHERE importer = ?1 OR resolved = ?1",
                        rusqlite::params![path],
                    )?;
                    fluent_db::query::execute(
                        tx,
                        "DELETE FROM graph_symbols WHERE file = ?1",
                        rusqlite::params![path],
                    )?;
                }
                fluent_db::query::execute(
                    tx,
                    "DELETE FROM symbols WHERE file_id = ?1",
                    rusqlite::params![file_id],
                )?;
                Ok(())
            })
            .map_err(VectorDbError::from)
    }

    /// Ids of files in `failed` status (failed-retry-once input).
    pub fn failed_file_ids(&self) -> Result<Vec<String>, VectorDbError> {
        self.docs
            .sqlite_store()
            .query_rows(
                "SELECT id FROM files WHERE index_status = 'failed' ORDER BY id",
                &[],
                |row| row.get(0),
            )
            .map_err(VectorDbError::from)
    }
}
