//! Generic searchable, categorized catalog modal.
//!
//! One engine serves every "pick a thing from a filtered, categorized list"
//! surface — symbol selection, indicator libraries — over the workspace's
//! shared primitives: [`common_core::registry::KeyedRegistry`] keeps entries
//! in deterministic sorted-key order (so a listing is reproducible, not
//! hash-ordered), and each entry lowers its searchable text once into a
//! [`common_core::string::LoweredHaystack`], so a scan never re-lowers the
//! haystack. Query, category tab, and favorites filter compose; the modal
//! owns that state and the caller reads [`CatalogModal::filtered`] or paints
//! through [`paint_catalog_modal`].
//!
//! The category type is fully generic: this crate knows nothing about asset
//! classes or indicator families. Category is an entry field and a filter
//! value; the caller supplies the tab labels it wants rendered.

use common_core::registry::KeyedRegistry;
use common_core::string::LoweredHaystack;

use crate::config::ThemeConfig;
use crate::error::EguiError;
use crate::paint::resolve_color;

/// One catalog row: a stable id, a display title, its category, and whether
/// the operator has starred it.
///
/// The searchable text (id and title, lowered once) is private bookkeeping;
/// every public field is display data the caller may render directly.
pub struct CatalogEntry<C> {
    /// Stable key; also a search token.
    pub id: String,
    /// Display title shown on the row.
    pub title: String,
    /// Category this entry belongs to.
    pub category: C,
    /// Whether the operator has starred this entry.
    pub favorite: bool,
    /// Search text (id plus title) lowered once at registration.
    search: LoweredHaystack,
}

impl<C> CatalogEntry<C> {
    /// Stable key.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Display title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Category this entry belongs to.
    #[must_use]
    pub fn category(&self) -> &C {
        &self.category
    }

    /// Whether the operator has starred this entry.
    #[must_use]
    pub fn is_favorite(&self) -> bool {
        self.favorite
    }

    /// Whether the entry's lowered search text contains `query` (case
    /// insensitive). An empty query matches every entry.
    #[must_use]
    pub fn matches(&self, query: &str) -> bool {
        self.search.contains(query)
    }
}

/// What one frame of the modal raised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogAction {
    /// An entry row was clicked; the payload is its id.
    Picked(String),
    /// An entry's favorite marker was toggled; the payload is its id.
    ToggledFavorite(String),
}

/// A categorized, searchable catalog over a deterministic keyed registry.
///
/// Query, active category (`None` means every category), and the
/// favorites-only flag are owned here; [`filtered`](Self::filtered) applies
/// all three. Entries iterate in sorted key order.
pub struct CatalogModal<C> {
    title: String,
    entries: KeyedRegistry<String, CatalogEntry<C>>,
    query: String,
    category: Option<C>,
    favorites_only: bool,
}

impl<C: Ord + Clone> CatalogModal<C> {
    /// Create an empty catalog with a display `title`.
    #[must_use]
    pub fn new(title: &str) -> Self {
        Self {
            title: title.to_owned(),
            entries: KeyedRegistry::new(),
            query: String::new(),
            category: None,
            favorites_only: false,
        }
    }

    /// Display title rendered above the search bar.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Register (or replace) one entry under `id`.
    ///
    /// Refuses blank ids and titles with the offending key named; a refusal
    /// leaves the catalog untouched. The entry's searchable text is lowered
    /// once here, so later scans never re-lower the haystack.
    pub fn register(&mut self, id: &str, title: &str, category: C) -> Result<(), EguiError> {
        if id.trim().is_empty() {
            return Err(EguiError::InvalidParam {
                what: "id".to_owned(),
                reason: "catalog entry id must not be empty".to_owned(),
            });
        }
        if title.trim().is_empty() {
            return Err(EguiError::InvalidParam {
                what: "title".to_owned(),
                reason: "catalog entry title must not be empty".to_owned(),
            });
        }
        let search = LoweredHaystack::new(&format!("{id} {title}"));
        self.entries.insert(
            id.to_owned(),
            CatalogEntry {
                id: id.to_owned(),
                title: title.to_owned(),
                category,
                favorite: false,
                search,
            },
        );
        Ok(())
    }

    /// Entry count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the catalog holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether `id` is registered.
    #[must_use]
    pub fn contains(&self, id: &str) -> bool {
        self.entries.contains(id)
    }

    /// Borrow one entry by id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&CatalogEntry<C>> {
        self.entries.get(id)
    }

    /// Every registered id, in sorted order.
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    /// Current query string.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Replace the query string.
    pub fn set_query(&mut self, query: &str) {
        query.clone_into(&mut self.query);
    }

    /// Active category tab (`None` means every category).
    #[must_use]
    pub fn category(&self) -> Option<&C> {
        self.category.as_ref()
    }

    /// Select a category tab, or clear it with `None`.
    pub fn set_category(&mut self, category: Option<C>) {
        self.category = category;
    }

    /// Whether the favorites-only filter is active.
    #[must_use]
    pub fn favorites_only(&self) -> bool {
        self.favorites_only
    }

    /// Turn the favorites-only filter on or off.
    pub fn set_favorites_only(&mut self, favorites_only: bool) {
        self.favorites_only = favorites_only;
    }

    /// Whether `id` is starred (`None` for unknown ids).
    #[must_use]
    pub fn is_favorite(&self, id: &str) -> Option<bool> {
        self.entries.get(id).map(|entry| entry.favorite)
    }

    /// Flip an entry's favorite marker, returning the new state. Unknown ids
    /// refuse.
    pub fn toggle_favorite(&mut self, id: &str) -> Result<bool, EguiError> {
        let entry = self.entries.get_mut(id).ok_or_else(|| unknown_entry(id))?;
        entry.favorite = !entry.favorite;
        Ok(entry.favorite)
    }

    /// Entries passing the category tab, favorites filter, and query, in
    /// sorted key order.
    ///
    /// The query is lowered once for the whole scan; each entry compares
    /// against its already-lowered haystack.
    #[must_use]
    pub fn filtered(&self) -> Vec<&CatalogEntry<C>> {
        let lowered_query = self.query.to_lowercase();
        self.entries
            .values()
            .filter(|entry| self.category.as_ref().is_none_or(|c| entry.category == *c))
            .filter(|entry| !self.favorites_only || entry.favorite)
            .filter(|entry| entry.search.lowered().contains(&lowered_query))
            .collect()
    }
}

/// Unknown-entry refusal naming the id.
fn unknown_entry(id: &str) -> EguiError {
    EguiError::InvalidParam { what: id.to_owned(), reason: "unknown catalog entry".to_owned() }
}

/// Paint the catalog: title, a search field, a favorites toggle, optional
/// category tabs, and the filtered rows (each with a favorite marker).
///
/// The modal owns its query, category, and favorites state, so painting
/// mutates them directly. Clicking a row raises [`CatalogAction::Picked`];
/// clicking a row's marker raises [`CatalogAction::ToggledFavorite`]. An
/// untouched frame raises nothing. A theme that cannot resolve the accent
/// key refuses with the key named.
pub fn paint_catalog_modal<C: Ord + Clone>(
    ui: &mut egui::Ui,
    modal: &mut CatalogModal<C>,
    categories: &[(C, &'static str)],
    theme: &ThemeConfig,
) -> Result<Vec<CatalogAction>, EguiError> {
    let accent = resolve_color(theme, "fast")?;
    let mut actions = Vec::new();
    ui.colored_label(accent, &modal.title);
    ui.horizontal(|bar| {
        bar.label("Search");
        bar.add(egui::TextEdit::singleline(&mut modal.query).hint_text("filter"));
        // One-click reset for every filter (query, category tab, favorites):
        // a catalog never strands the operator in a narrowed view with no
        // obvious way back to the full list.
        if bar.button("Clear").clicked() {
            modal.query.clear();
            modal.category = None;
            modal.favorites_only = false;
        }
        bar.checkbox(&mut modal.favorites_only, "Favorites");
    });
    if !categories.is_empty() {
        ui.horizontal(|tabs| {
            tabs.selectable_value(&mut modal.category, None, "All");
            for (category, label) in categories {
                tabs.selectable_value(&mut modal.category, Some(category.clone()), *label);
            }
        });
    }
    ui.separator();
    // Collect the rows first: rendering mutates `actions`, never the modal.
    let rows: Vec<(String, String, bool)> = modal
        .filtered()
        .into_iter()
        .map(|entry| (entry.id.clone(), entry.title.clone(), entry.favorite))
        .collect();
    for (id, title, favorite) in rows {
        ui.horizontal(|row| {
            let marker = if favorite { "[x]" } else { "[ ]" };
            if row.button(marker).clicked() {
                actions.push(CatalogAction::ToggledFavorite(id.clone()));
            }
            if row.selectable_label(false, title).clicked() {
                actions.push(CatalogAction::Picked(id));
            }
        });
    }
    Ok(actions)
}

#[cfg(test)]
#[path = "../tests/modal.rs"]
mod tests;
