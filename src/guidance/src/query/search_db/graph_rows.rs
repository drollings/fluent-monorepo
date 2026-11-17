//! Graph recall rows: persisted import edges and symbol call sites.
//!
//! Moved verbatim from `search-vector`: row shapes and `from_row` constructors
//! (crate-visible for the `GuidanceSearchDb` recall queries). No SQL text changed.

use std::collections::HashMap;

use super::VectorDbError;

/// One persisted import edge: the importing file, the raw specifier text,
/// and the resolved target (NULL until hydration resolves it against the
/// workspace file set).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphEdgeRow {
    /// Importing file (absolute path; the graph node key).
    pub importer: String,
    /// Raw specifier text.
    pub specifier: String,
    /// Resolved target file, if any.
    pub resolved: Option<String>,
}

impl GraphEdgeRow {
    pub(crate) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            importer: row.get(0)?,
            specifier: row.get(1)?,
            resolved: row.get(2)?,
        })
    }
}

/// One persisted symbol site: the defined name, its file, and the raw
/// names it calls. The empty name marks an unnamed scope's calls (kept
/// for call edges, never a definition).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphSymbolRow {
    /// Defined symbol name ("" = unnamed scope).
    pub name: String,
    /// Defining file (absolute path).
    pub file: String,
    /// Raw callee names.
    pub calls: Vec<String>,
}

impl GraphSymbolRow {
    pub(crate) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let raw: String = row.get(2)?;
        Ok(Self {
            name: row.get(0)?,
            file: row.get(1)?,
            calls: serde_json::from_str(&raw).unwrap_or_default(),
        })
    }
}

impl super::GuidanceSearchDb {
    /// Replace one file's persisted graph inputs (delete + insert in one
    /// transaction). `specifiers` are raw import texts; `resolved` stays
    /// NULL here — specifiers resolve at hydration time against the
    /// workspace file set, never per file. Same-name definitions
    /// (overloads) share one row with unioned calls.
    pub fn replace_file_graph(
        &self,
        importer: &str,
        specifiers: &[String],
        symbols: &[(String, Vec<String>)],
    ) -> Result<(), VectorDbError> {
        let mut merged_names: Vec<String> = Vec::new();
        let mut merged_calls: HashMap<String, Vec<String>> = HashMap::new();
        for (name, calls) in symbols {
            let entry = merged_calls.entry(name.clone()).or_insert_with(|| {
                merged_names.push(name.clone());
                Vec::new()
            });
            for call in calls {
                if !entry.contains(call) {
                    entry.push(call.clone());
                }
            }
        }
        let mut calls_json: HashMap<String, String> = HashMap::new();
        for name in &merged_names {
            let json = serde_json::to_string(&merged_calls[name])
                .map_err(|e| VectorDbError::Serialization(e.to_string()))?;
            calls_json.insert(name.clone(), json);
        }
        self.docs
            .sqlite_store()
            .transaction(|tx| {
                fluent_db::query::execute(
                    tx,
                    "DELETE FROM graph_edges WHERE importer = ?1",
                    rusqlite::params![importer],
                )?;
                fluent_db::query::execute(
                    tx,
                    "DELETE FROM graph_symbols WHERE file = ?1",
                    rusqlite::params![importer],
                )?;
                for specifier in specifiers {
                    fluent_db::query::execute(
                        tx,
                        "INSERT INTO graph_edges (importer, specifier, resolved)
                         VALUES (?1, ?2, NULL)",
                        rusqlite::params![importer, specifier],
                    )?;
                }
                for name in &merged_names {
                    fluent_db::query::execute(
                        tx,
                        "INSERT INTO graph_symbols (name, file, calls_json)
                         VALUES (?1, ?2, ?3)",
                        rusqlite::params![name, importer, calls_json[name]],
                    )?;
                }
                Ok(())
            })
            .map_err(VectorDbError::from)
    }

    /// All persisted import edges, ordered for deterministic hydration.
    pub fn graph_edge_rows(&self) -> Result<Vec<GraphEdgeRow>, VectorDbError> {
        self.docs
            .sqlite_store()
            .query_rows(
                "SELECT importer, specifier, resolved FROM graph_edges
                 ORDER BY importer, specifier",
                &[],
                GraphEdgeRow::from_row,
            )
            .map_err(VectorDbError::from)
    }

    /// All persisted symbol rows, ordered for deterministic hydration.
    pub fn graph_symbol_rows(&self) -> Result<Vec<GraphSymbolRow>, VectorDbError> {
        self.docs
            .sqlite_store()
            .query_rows(
                "SELECT name, file, calls_json FROM graph_symbols ORDER BY file, name",
                &[],
                GraphSymbolRow::from_row,
            )
            .map_err(VectorDbError::from)
    }
}
