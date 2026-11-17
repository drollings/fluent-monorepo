//! Symbol-row operations over the guidance tables.
//!
//! Moved verbatim from `search-vector`: same SQL text, same behavior.

use rusqlite::params;

use super::fragments::SymbolRecord;
use super::VectorDbError;

impl super::GuidanceSearchDb {
    /// Replace one file's identity rows (delete + insert in one
    /// transaction). One row per (file, symbol, scope); window chunks
    /// of one entity collapse to the shared stable id.
    pub fn replace_symbols(&self, rows: &[SymbolRecord]) -> Result<usize, VectorDbError> {
        if rows.is_empty() {
            return Ok(0);
        }
        let file_id = rows[0].file_id.clone();
        self.docs
            .sqlite_store()
            .transaction(|tx| {
                fluent_db::query::execute(
                    tx,
                    "DELETE FROM symbols WHERE file_id = ?1",
                    rusqlite::params![file_id],
                )?;
                for row in rows {
                    fluent_db::query::execute(
                        tx,
                        "INSERT OR REPLACE INTO symbols
                         (symbol_id, file_id, name, scope, interface_hash)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        rusqlite::params![
                            row.symbol_id,
                            row.file_id,
                            row.name,
                            row.scope,
                            row.interface_hash,
                        ],
                    )?;
                }
                Ok(rows.len())
            })
            .map_err(VectorDbError::from)
    }

    /// Identity rows for one file, ordered by symbol id.
    pub fn symbols_for_file(&self, file_id: &str) -> Result<Vec<SymbolRecord>, VectorDbError> {
        self.docs
            .sqlite_store()
            .query_rows(
                "SELECT symbol_id, file_id, name, scope, interface_hash
                 FROM symbols WHERE file_id = ?1 ORDER BY symbol_id",
                params![file_id],
                SymbolRecord::from_row,
            )
            .map_err(VectorDbError::from)
    }

    /// Every symbol's interface hash, for the tree-level propagation
    /// fingerprint. Module-header rows are filtered by the caller.
    pub fn symbol_interface_hashes(&self) -> Result<Vec<(String, String, String)>, VectorDbError> {
        self.docs
            .sqlite_store()
            .query_rows(
                "SELECT file_id, symbol_id, interface_hash FROM symbols",
                &[],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(VectorDbError::from)
    }

    /// Every declaration of one name across files: the exact-symbol
    /// input. Homonyms stay listed (every candidate reachable);
    /// unknown names yield empty (honest empty, never a guess).
    pub fn symbols_by_name(&self, name: &str) -> Result<Vec<SymbolRecord>, VectorDbError> {
        self.docs
            .sqlite_store()
            .query_rows(
                "SELECT symbol_id, file_id, name, scope, interface_hash
                 FROM symbols WHERE name = ?1 ORDER BY symbol_id",
                params![name],
                SymbolRecord::from_row,
            )
            .map_err(VectorDbError::from)
    }

    /// Owning fragment for one symbol: id plus stored range JSON, first
    /// in harvest order. `scope = None` matches any scope (the callers
    /// route, where scope is unknown); `Some` matches exactly (the
    /// summary join). `None` when no fragment declares the symbol.
    pub fn symbol_fragment(
        &self,
        file_id: &str,
        name: &str,
        scope: Option<&str>,
    ) -> Result<Option<(String, String)>, VectorDbError> {
        self.docs
            .sqlite_store()
            .query_row(
                "SELECT id, range_json FROM fragments
                 WHERE file_id = ?1 AND symbol_name = ?2
                 AND (?3 IS NULL OR COALESCE(scope, '') = ?3)
                 ORDER BY rowid LIMIT 1",
                rusqlite::params![file_id, name, scope],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(VectorDbError::from)
    }
}
