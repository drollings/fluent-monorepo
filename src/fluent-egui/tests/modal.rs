//! Searchable, categorized catalog modal: deterministic ordering over the
//! shared keyed registry, category/favorite/query filtering over one lowered
//! haystack, fail-closed registration, and headless rendering.

use super::*;

/// Domain-blind stand-in for a catalog's category tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Class {
    Stocks,
    Funds,
    Futures,
    Crypto,
}

fn catalog() -> CatalogModal<Class> {
    let mut modal = CatalogModal::new("Symbols");
    modal
        .register("gld", "SPDR Gold Shares", Class::Funds)
        .expect("registers");
    modal
        .register("spy", "SPDR S&P 500", Class::Stocks)
        .expect("registers");
    modal
        .register("es", "E-mini S&P", Class::Futures)
        .expect("registers");
    modal
        .register("btc", "Bitcoin", Class::Crypto)
        .expect("registers");
    modal
}

fn ids(modal: &CatalogModal<Class>) -> Vec<String> {
    modal
        .filtered()
        .iter()
        .map(|entry| entry.id().to_owned())
        .collect()
}

#[test]
fn catalog_modal_orders_entries_deterministically_via_keyed_registry() {
    let mut modal = CatalogModal::new("Symbols");
    modal
        .register("zeta", "Zeta Fund", Class::Funds)
        .expect("registers");
    modal
        .register("alpha", "Alpha Corp", Class::Stocks)
        .expect("registers");
    modal
        .register("mid", "Mid Futures", Class::Futures)
        .expect("registers");
    assert_eq!(
        modal.ids(),
        vec!["alpha", "mid", "zeta"],
        "keys come back sorted"
    );
    let entries: Vec<&CatalogEntry<Class>> = modal.filtered();
    let ordered: Vec<&str> = entries.iter().map(|entry| entry.id()).collect();
    assert_eq!(
        ordered,
        vec!["alpha", "mid", "zeta"],
        "filtered follows registry order"
    );
    assert_eq!(modal.len(), 3);
    assert!(modal.contains("alpha"), "registered keys are present");
    assert_eq!(
        modal.get("mid").map(CatalogEntry::title),
        Some("Mid Futures")
    );
    // Re-registering an existing key replaces it in place: the registry does
    // not grow and the sorted order holds.
    modal
        .register("mid", "Midpoint", Class::Futures)
        .expect("replaces");
    assert_eq!(modal.len(), 3, "a replace does not grow the registry");
    assert_eq!(modal.get("mid").map(CatalogEntry::title), Some("Midpoint"));
}

#[test]
fn catalog_modal_filters_by_category_favorites_and_case_insensitive_query() {
    let mut modal = catalog();

    // Case-insensitive substring query, over both id and title.
    modal.set_query("spdr");
    assert_eq!(
        ids(&modal),
        vec!["gld", "spy"],
        "substring match ignores case"
    );
    modal.set_query("SPDR");
    assert_eq!(
        ids(&modal),
        vec!["gld", "spy"],
        "query casing never matters"
    );
    modal.set_query("btc");
    assert_eq!(ids(&modal), vec!["btc"], "the id is searchable");
    modal.set_query("zzzz_no_match");
    assert!(
        modal.filtered().is_empty(),
        "a disjoint query matches nothing"
    );
    modal.set_query("");
    assert_eq!(
        modal.filtered().len(),
        4,
        "an empty query matches every entry"
    );

    // Category tab filtering; `None` clears the tab back to all.
    modal.set_category(Some(Class::Stocks));
    assert_eq!(modal.category(), Some(&Class::Stocks), "the tab holds");
    assert_eq!(ids(&modal), vec!["spy"], "only the selected class shows");
    modal.set_category(None);
    assert_eq!(modal.filtered().len(), 4, "clearing the category shows all");

    // Favorite toggling plus the favorites-only filter.
    assert_eq!(
        modal.is_favorite("gld"),
        Some(false),
        "entries start unfavorited"
    );
    assert!(
        modal.toggle_favorite("gld").expect("toggles"),
        "first toggle favorites"
    );
    assert_eq!(modal.is_favorite("gld"), Some(true));
    modal.set_favorites_only(true);
    assert_eq!(
        ids(&modal),
        vec!["gld"],
        "favorites-only shows just the starred entry"
    );
    assert!(
        !modal.toggle_favorite("gld").expect("toggles"),
        "second toggle unfavorites"
    );
    modal.set_favorites_only(false);

    // Category, favorite, and query compose.
    modal.set_category(Some(Class::Funds));
    modal.set_query("gold");
    assert_eq!(ids(&modal), vec!["gld"], "category and query compose");
    assert!(
        modal.toggle_favorite("missing").is_err(),
        "unknown ids refuse the star"
    );
    assert!(
        modal.is_favorite("missing").is_none(),
        "unknown reads answer nothing"
    );
}

#[test]
fn catalog_modal_refuses_empty_ids_and_renders_headlessly() {
    let mut modal = CatalogModal::<Class>::new("Symbols");
    let blank_id = modal
        .register("", "Title", Class::Stocks)
        .expect_err("blank id refuses");
    assert!(
        blank_id.to_string().contains("id"),
        "the refusal names the id: {blank_id}"
    );
    assert!(
        modal.register("ok", "   ", Class::Stocks).is_err(),
        "blank title refuses"
    );
    assert!(modal.is_empty(), "a refusal leaves the catalog untouched");
    modal
        .register("gld", "Gold", Class::Funds)
        .expect("registers");

    let theme = ThemeConfig::default();
    egui::__run_test_ui(|ui| {
        let actions: Vec<CatalogAction> =
            paint_catalog_modal(ui, &mut modal, &[(Class::Stocks, "Stocks")], &theme)
                .expect("the modal paints");
        assert!(
            actions.is_empty(),
            "an untouched frame raises nothing: {actions:?}"
        );
    });
    // A theme that cannot resolve the accent key refuses with the key named
    // instead of painting a silent default.
    let broken = ThemeConfig {
        fast: "not-a-color".to_owned(),
        ..Default::default()
    };
    egui::__run_test_ui(|ui| {
        assert!(
            paint_catalog_modal(ui, &mut modal, &[], &broken).is_err(),
            "a broken theme refuses rather than painting gray"
        );
    });
}

/// Calibration (task value: search retrieval fitness): positive queries return
/// exactly their labeled matches at full precision and recall, and the
/// negative controls (disjoint, whitespace-only, and category-excluded
/// queries) return nothing — zero false positives.
#[test]
fn catalog_search_precision_and_recall_controls() {
    let mut modal = catalog();
    let evaluate = |modal: &CatalogModal<Class>, expected: &[&str]| -> (f64, f64) {
        let returned = ids(modal);
        let hits = returned
            .iter()
            .filter(|id| expected.contains(&id.as_str()))
            .count();
        let precision = if returned.is_empty() {
            if expected.is_empty() {
                1.0
            } else {
                0.0
            }
        } else {
            hits as f64 / returned.len() as f64
        };
        let recall = if expected.is_empty() {
            1.0
        } else {
            hits as f64 / expected.len() as f64
        };
        (precision, recall)
    };
    // Positive queries: every returned entry is a true positive, and every
    // labeled entry is returned.
    for (query, expected) in [
        ("spdr", &["gld", "spy"][..]),
        ("SPDR", &["gld", "spy"][..]),
        ("gold", &["gld"][..]),
        ("btc", &["btc"][..]),
        ("e-mini", &["es"][..]),
    ] {
        modal.set_query(query);
        let (precision, recall) = evaluate(&modal, expected);
        assert!(
            (precision - 1.0).abs() < 1e-12 && (recall - 1.0).abs() < 1e-12,
            "query {query:?} scores fully: {precision}/{recall}"
        );
    }
    // Negative controls: disjoint and whitespace-only queries match nothing.
    for query in ["zzzz_no_match", "   ", "\t\t"] {
        modal.set_query(query);
        assert!(
            modal.filtered().is_empty(),
            "query {query:?} yields no false positives"
        );
        let (precision, recall) = evaluate(&modal, &[]);
        assert!(
            (precision - 1.0).abs() < 1e-12 && (recall - 1.0).abs() < 1e-12,
            "an empty result scores clean: {precision}/{recall}"
        );
    }
    // Category-excluded items: a query matching an entry outside the active
    // tab returns nothing, so the excluded match is never a false positive.
    modal.set_category(Some(Class::Stocks));
    modal.set_query("gold");
    assert!(
        modal.filtered().is_empty(),
        "category exclusion drops the match"
    );
    let (precision, recall) = evaluate(&modal, &[]);
    assert!(
        (precision - 1.0).abs() < 1e-12 && (recall - 1.0).abs() < 1e-12,
        "the excluded match is not returned: {precision}/{recall}"
    );
    // The favorites filter composes with the query the same way.
    modal.set_category(None);
    modal.set_query("spdr");
    assert!(
        modal.toggle_favorite("spy").expect("toggles"),
        "spy becomes a favorite"
    );
    modal.set_favorites_only(true);
    let (precision, recall) = evaluate(&modal, &["spy"]);
    assert!(
        (precision - 1.0).abs() < 1e-12 && (recall - 1.0).abs() < 1e-12,
        "favorites-only narrows cleanly: {precision}/{recall}"
    );
}

#[test]
fn catalog_modal_tag_filter_shows_only_its_namespace() {
    let mut modal = catalog();
    modal
        .register("pine:rsi", "🌲 RSI", Class::Stocks)
        .expect("registers");
    modal.set_tag_filter("Pine", "pine:");
    assert!(!modal.tag_only(), "installing starts unchecked");
    assert_eq!(ids(&modal).len(), 5, "unchecked shows everything");
    modal.tag_filter.as_mut().expect("installed").only = true;
    assert!(modal.tag_only(), "checking narrows the view");
    assert_eq!(ids(&modal), vec!["pine:rsi"], "only the namespace shows");
    // The tag composes with the other filters instead of replacing them.
    modal.query = "rsi".to_owned();
    assert_eq!(ids(&modal), vec!["pine:rsi"]);
    modal.query = "zzz-no-match".to_owned();
    assert!(ids(&modal).is_empty(), "query still narrows the tag view");
    modal.query.clear();
    modal.set_tag_filter("Pine", "other:");
    assert_eq!(ids(&modal).len(), 5, "replacing starts unchecked");
    modal.tag_filter.as_mut().expect("installed").only = true;
    assert!(ids(&modal).is_empty(), "the new namespace matches nothing");
}

#[test]
fn tag_checkbox_paints_headlessly_when_installed() {
    use super::CatalogAction;

    let mut modal = CatalogModal::new("Symbols");
    modal
        .register("pine:rsi", "RSI", Class::Stocks)
        .expect("registers");
    modal
        .register("spy", "SPDR S&P 500", Class::Stocks)
        .expect("registers");
    modal.set_tag_filter("Pine", "pine:");
    let theme = ThemeConfig::default();
    egui::__run_test_ui(|ui| {
        let actions: Vec<CatalogAction> =
            paint_catalog_modal(ui, &mut modal, &[(Class::Stocks, "Stocks")], &theme)
                .expect("the modal paints with a tag filter");
        assert!(
            actions.is_empty(),
            "an untouched frame raises nothing: {actions:?}"
        );
    });
    assert!(!modal.tag_only(), "painting does not check the box");
}
