//! Exact symbol plane: definitions by name through the committed
//! `symbols` table. No fuzzy matching, no guessing: homonyms stay
//! listed, unknown names yield empty.

use crate::query::search_db::{GuidanceSearchDb, SymbolRecord};

/// One exact definition site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolSite {
    /// Declared name.
    pub name: String,
    /// Owning file (id as stored; absolute path).
    pub file: String,
    /// Lexical scope breadcrumb, when recorded.
    pub scope: Option<String>,
    /// Stable symbol id (`file#scope::name`).
    pub symbol_id: String,
}

impl From<SymbolRecord> for SymbolSite {
    fn from(row: SymbolRecord) -> Self {
        Self {
            name: row.name.unwrap_or_default(),
            file: row.file_id,
            scope: row.scope,
            symbol_id: row.symbol_id,
        }
    }
}

/// Candidate query forms for one symbol reference: the full dotted
/// form plus the terminal identifier (`engine.gen_scoped` matches
/// both spellings). Single names yield one form. Resolution tiers
/// travel on the matched rows/edges, never on this list.
#[must_use]
pub fn normalize_symbol_query(name: &str) -> Vec<String> {
    let trimmed = name.trim().trim_matches('"').trim_matches('\'').trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let mut forms = vec![trimmed.to_string()];
    if let Some((_, terminal)) = trimmed
        .rsplit_once("::")
        .or_else(|| trimmed.rsplit_once('.'))
    {
        let terminal = terminal.trim();
        if !terminal.is_empty() && terminal != trimmed {
            forms.push(terminal.to_string());
        }
    }
    forms
}

/// Exact-symbol port: definitions by name.
pub trait StructureStore {
    /// Every declaration of the name in any query form. Empty for
    /// unknown names.
    fn exact_symbols(&self, name: &str) -> Vec<SymbolSite>;
}

/// `StructureStore` over a committed index.
pub struct GuidanceStructureStore<'a> {
    db: &'a GuidanceSearchDb,
}

impl<'a> GuidanceStructureStore<'a> {
    /// Borrow the committed index (no copy; rows read per query).
    #[must_use]
    pub fn new(db: &'a GuidanceSearchDb) -> Self {
        Self { db }
    }
}

impl StructureStore for GuidanceStructureStore<'_> {
    fn exact_symbols(&self, name: &str) -> Vec<SymbolSite> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for form in normalize_symbol_query(name) {
            if let Ok(rows) = self.db.symbols_by_name(&form) {
                for site in rows.into_iter().map(SymbolSite::from) {
                    if seen.insert(site.symbol_id.clone()) {
                        out.push(site);
                    }
                }
            }
        }
        out.sort_by(|a, b| a.symbol_id.cmp(&b.symbol_id));
        out
    }
}
