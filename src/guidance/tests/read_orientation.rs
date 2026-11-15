//! Orientation plane: workspace/crate/domain cards first, ownership
//! scoping by package, absent-corpus behavior. Included via `#[path]`
//! forwarders from `read/planes`, per repo convention. Hermetic: cards
//! built by `read::project_map` over temp workspaces, no endpoint.

use super::orientation::OrientationPlane;

fn fixture() -> (tempfile::TempDir, search_vector::db::GuidanceDb) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"src/guidance\", \"src/router\"]\n",
    )
    .expect("manifest");
    for (prefix, name, desc, readme) in [
        (
            "src/guidance",
            "guidance-core",
            "Local code index for recall.",
            "# Guidance Core\n\nIndex facts here.\n",
        ),
        (
            "src/router",
            "fluent-router",
            "Routes requests onward through the table.",
            "# Fluent Router\n\nRoute facts here.\n",
        ),
    ] {
        let dir_path = root.join(prefix);
        std::fs::create_dir_all(&dir_path).expect("mkdir");
        std::fs::write(
            dir_path.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\ndescription = \"{desc}\"\n"),
        )
        .expect("write manifest");
        std::fs::write(dir_path.join("README.md"), readme).expect("write readme");
    }
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    crate::read::project_map::import_project_cards(root, "fixture", &db, None).expect("import");
    (dir, db)
}

#[test]
fn orientation_starts_from_workspace_and_crate_cards() {
    let (_dir, db) = fixture();
    let plane = OrientationPlane::load(&db).expect("load");
    assert!(!plane.is_empty());
    let cards = plane.orientation_cards();
    assert!(
        !cards.is_empty(),
        "orientation answers from cards: {cards:?}"
    );
    assert_eq!(cards[0].kind, "workspace");
    // Every card links a doc anchor and code paths.
    for card in &cards {
        assert!(!card.source.is_empty(), "{card:?}");
        assert!(!card.title.is_empty(), "{card:?}");
    }
    assert!(cards.iter().any(|card| card.kind == "crate"), "{cards:?}");
    let guidance = cards
        .iter()
        .find(|card| card.id == "crate:guidance-core")
        .expect("guidance card");
    assert!(
        guidance.code_paths.contains(&"src/guidance".to_string()),
        "{guidance:?}"
    );
    assert!(guidance.source.contains("README.md"), "{guidance:?}");
}

#[test]
fn ownership_scopes_to_package_and_domain() {
    let (_dir, db) = fixture();
    let plane = OrientationPlane::load(&db).expect("load");
    let cards = plane.ownership_cards("guidance-core");
    assert_eq!(cards.len(), 2, "{cards:?}");
    assert_eq!(cards[0].kind, "crate");
    assert_eq!(cards[1].kind, "domain");
    assert!(plane.ownership_cards("no-such-crate").is_empty());
}

#[test]
fn absent_corpus_is_an_empty_plane_not_an_error() {
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    let plane = OrientationPlane::load(&db).expect("load");
    assert!(plane.is_empty());
    assert!(plane.orientation_cards().is_empty());
    assert!(plane.ownership_cards("guidance-core").is_empty());
}
