//! Searchable, categorized catalog modal: deterministic ordering over the
//! shared keyed registry, category/favorite/query filtering over one lowered
//! haystack, fail-closed registration, and headless rendering.

use super::*;

/// Domain-blind stand-in for a catalog's category tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

fn big_catalog() -> CatalogModal<Class> {
    let words = [
        "Alpha", "Beta", "Gamma", "Delta", "Epsilon", "Zeta", "Eta", "Theta",
    ];
    let mut modal = CatalogModal::new("Symbols");
    let mut state: u64 = 0x9E3779B97F4A7C15;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for index in 0..48 {
        let id = format!("sym-{index:03}");
        let title = format!("{} {}", words[index % words.len()], index);
        let category = match next() % 4 {
            0 => Class::Stocks,
            1 => Class::Funds,
            2 => Class::Futures,
            _ => Class::Crypto,
        };
        modal.register(&id, &title, category).expect("registers");
        if index % 3 == 0 {
            modal.toggle_favorite(&id).expect("stars");
        }
        if index % 4 == 0 {
            modal.set_subtitle(&id, "second line").expect("subtitles");
        }
    }
    modal
}

fn count_for(counts: &[(Class, usize)], class: Class) -> usize {
    counts
        .iter()
        .find_map(|(candidate, count)| (*candidate == class).then_some(*count))
        .unwrap_or(0)
}

#[test]
fn tab_counts_match_filtered_lengths_per_tab() {
    for query in ["", "sym", "alpha", "zzz", "e"] {
        for favorites in [false, true] {
            let mut modal = big_catalog();
            modal.set_query(query);
            modal.set_favorites_only(favorites);
            let counts = modal.tab_counts();
            let total: usize = counts.iter().map(|(_, count)| count).sum();
            modal.set_category(None);
            assert_eq!(
                modal.filtered().len(),
                total,
                "the All tab holds every counted row (query={query:?}, favorites={favorites})"
            );
            for class in [Class::Stocks, Class::Funds, Class::Futures, Class::Crypto] {
                modal.set_category(Some(class));
                assert_eq!(
                    modal.filtered().len(),
                    count_for(&counts, class),
                    "the {class:?} tab holds its counted rows (query={query:?}, favorites={favorites})"
                );
            }
        }
    }
}

#[test]
fn selection_never_alters_filtered_membership() {
    let mut first = big_catalog();
    first.set_selected(Some("sym-003".to_owned()));
    first.set_query("sym");
    first.set_category(Some(Class::Stocks));
    let mut second = big_catalog();
    second.set_query("sym");
    second.set_category(Some(Class::Stocks));
    second.set_selected(Some("sym-003".to_owned()));
    assert_eq!(
        ids(&first),
        ids(&second),
        "select-then-filter equals filter-then-select"
    );
    assert_eq!(
        first.selected(),
        Some(&"sym-003".to_owned()),
        "the selection sticks"
    );
    let mut cleared = big_catalog();
    cleared.set_selected(Some("sym-003".to_owned()));
    cleared.set_selected(None);
    assert_eq!(cleared.selected(), None, "clearing drops the selection");
    assert_eq!(
        ids(&cleared),
        ids(&big_catalog()),
        "clearing restores the unselected listing"
    );
}

fn modal_frame(
    ctx: &egui::Context,
    input: egui::RawInput,
    modal: &mut CatalogModal<Class>,
    categories: &[(Class, &'static str)],
    theme: &ThemeConfig,
) -> (Vec<CatalogAction>, egui::FullOutput) {
    let actions = std::cell::Cell::new(Vec::new());
    let mut output = ctx.run_ui(input, |ui| {
        let got = paint_catalog_modal(ui, modal, categories, theme).expect("the modal paints");
        actions.set(got);
    });
    output.textures_delta.clear();
    (actions.take(), output)
}

fn frame_screen() -> egui::Rect {
    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))
}

fn frame_input(point: egui::RawInput) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(frame_screen()),
        ..point
    }
}

fn click_input(pos: egui::Pos2, pressed: bool) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(frame_screen()),
        events: vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            },
        ],
        ..Default::default()
    }
}

fn text_center(output: &egui::FullOutput, needle: &str) -> Option<egui::Pos2> {
    output
        .shapes
        .iter()
        .find_map(|clipped| match &clipped.shape {
            egui::Shape::Text(text) if text.galley.text() == needle => {
                Some(text.galley.rect.translate(text.pos.to_vec2()).center())
            }
            _ => None,
        })
}

fn shape_texts(output: &egui::FullOutput) -> Vec<String> {
    output
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Text(text) => Some(text.galley.text().to_owned()),
            _ => None,
        })
        .collect()
}

const ALL_CATEGORIES: [(Class, &str); 4] = [
    (Class::Stocks, "Stocks"),
    (Class::Funds, "Funds"),
    (Class::Futures, "Futures"),
    (Class::Crypto, "Crypto"),
];

#[test]
fn tabs_show_counts_and_selection_marks_the_row() {
    let theme = ThemeConfig::default();
    let mut modal = catalog();
    modal.set_subtitle("gld", "Gold ETF").expect("subtitles");
    modal.set_selected(Some("spy".to_owned()));
    let ctx = egui::Context::default();
    let (actions, output) = modal_frame(
        &ctx,
        frame_input(egui::RawInput::default()),
        &mut modal,
        &ALL_CATEGORIES,
        &theme,
    );
    assert!(actions.is_empty(), "an untouched frame raises nothing");
    let texts = shape_texts(&output);
    assert!(
        texts.iter().any(|text| text == "All (4)"),
        "the All tab counts its rows: {texts:?}"
    );
    assert!(
        texts.iter().any(|text| text == "Stocks (1)"),
        "each tab counts its rows: {texts:?}"
    );
    assert!(
        texts.iter().any(|text| text == "Gold ETF"),
        "the subtitle renders: {texts:?}"
    );
}

#[test]
fn subtitle_leaves_pick_and_favorite_hits_alone() {
    let theme = ThemeConfig::default();
    let mut modal = catalog();
    modal.set_subtitle("spy", "Big index").expect("subtitles");
    let ctx = egui::Context::default();
    let (_, output) = modal_frame(
        &ctx,
        frame_input(egui::RawInput::default()),
        &mut modal,
        &ALL_CATEGORIES,
        &theme,
    );
    let title = text_center(&output, "SPDR S&P 500").expect("the title paints");
    let (no_press, _) = modal_frame(
        &ctx,
        click_input(title, true),
        &mut modal,
        &ALL_CATEGORIES,
        &theme,
    );
    assert!(no_press.is_empty(), "press alone picks nothing");
    let (picked, _) = modal_frame(
        &ctx,
        click_input(title, false),
        &mut modal,
        &ALL_CATEGORIES,
        &theme,
    );
    assert!(
        picked.contains(&CatalogAction::Picked("spy".to_owned())),
        "release on the title picks it, subtitle and all: {picked:?}"
    );
    let mut starred = catalog();
    starred.set_subtitle("btc", "Coin").expect("subtitles");
    let (_, star_output) = modal_frame(
        &ctx,
        frame_input(egui::RawInput::default()),
        &mut starred,
        &ALL_CATEGORIES,
        &theme,
    );
    let first_star = text_center(&star_output, "☆").expect("a star paints");
    let (toggled, _) = modal_frame(
        &ctx,
        click_input(first_star, true),
        &mut starred,
        &ALL_CATEGORIES,
        &theme,
    );
    assert!(toggled.is_empty(), "press alone toggles nothing");
    let (toggled, _) = modal_frame(
        &ctx,
        click_input(first_star, false),
        &mut starred,
        &ALL_CATEGORIES,
        &theme,
    );
    assert!(
        toggled
            .iter()
            .any(|action| matches!(action, CatalogAction::ToggledFavorite(_))),
        "release on the star toggles it: {toggled:?}"
    );
}

#[test]
fn collapsed_categories_hide_members_without_removing_them() {
    let mut modal = catalog();
    assert!(
        modal.is_expanded(&Class::Stocks),
        "categories start expanded"
    );
    modal.set_expanded(Class::Stocks, false);
    assert!(!modal.is_expanded(&Class::Stocks), "collapse sticks");
    assert_eq!(modal.len(), 4, "collapse removes nothing");
    assert_eq!(modal.ids().len(), 4, "the registry still holds every entry");
    let shown = ids(&modal);
    assert!(
        !shown.contains(&"spy".to_owned()),
        "collapsed members hide from the listing: {shown:?}"
    );
    assert_eq!(shown.len(), 3, "only the collapsed members hide");
    modal.set_expanded(Class::Stocks, true);
    assert_eq!(ids(&modal).len(), 4, "re-expanding restores the listing");
}

#[test]
fn default_state_reproduces_the_flat_listing() {
    let modal = catalog();
    let mut explicit = catalog();
    for class in [Class::Stocks, Class::Funds, Class::Futures, Class::Crypto] {
        explicit.set_expanded(class, true);
    }
    assert_eq!(
        ids(&explicit),
        ids(&modal),
        "explicit all-expanded equals the default"
    );
    let mut tabbed = catalog();
    tabbed.set_category(Some(Class::Funds));
    tabbed.set_expanded(Class::Funds, false);
    assert!(
        tabbed.filtered().is_empty(),
        "collapsing the active tab empties it"
    );
    assert_eq!(tabbed.len(), 4, "the entries are hidden, not removed");
    tabbed.set_expanded(Class::Funds, true);
    assert_eq!(ids(&tabbed), vec!["gld"], "the tab listing returns");
}
