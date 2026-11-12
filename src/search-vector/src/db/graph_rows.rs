//! Graph recall rows: persisted import edges and symbol call sites.
//!
//! Moved verbatim from `db.rs` (M6 split): row shapes and `from_row`
//! constructors (crate-visible for the `GuidanceDb` recall queries). No SQL
//! text changed.

/// One persisted import edge: the importing file, the raw specifier text,
/// and the resolved target (NULL until hydration resolves it against the
/// workspace file set).
#[derive(Debug, Clone, PartialEq, Eq)]
/// Graph recall rows: persisted import edges and symbol call sites.
/// 
/// Moved verbatim from `db.rs` (M6 split): row shapes and `from_row`
/// constructors (crate-visible for the `GuidanceDb` recall queries). No SQL
/// text changed.
/// One persisted import edge: the importing file, the raw specifier text,
/// and the resolved target (NULL until hydration resolves it against the
/// workspace file set).
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
/// One persisted symbol site: the defined name, its file, and the raw
/// names it calls. The empty name marks an unnamed scope's calls (kept
/// for call edges, never a definition).
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