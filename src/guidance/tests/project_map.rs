//! Deterministic project cards: README member lists, Cargo workspace
//! members, per-crate ownership, doc/code links, and ordering.
//! Included via `#[path]` forwarder from `read/project_map.rs`, per
//! repo convention. Hermetic: temp workspaces, hand-written manifests.

use super::*;
use search_vector::db::ProjectCard;

fn map_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"src/guidance\", \"src/router\"]\n",
    )
    .expect("write root manifest");
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
    dir
}

#[test]
fn members_come_from_the_root_manifest() {
    let dir = map_fixture();
    assert_eq!(
        workspace_members(dir.path()),
        vec!["src/guidance".to_string(), "src/router".to_string()]
    );
    let empty = tempfile::tempdir().expect("tempdir");
    assert!(workspace_members(empty.path()).is_empty());
}

#[test]
fn manifest_package_name_parses() {
    assert_eq!(
        crate::read::doc_cards::manifest_package_name("[package]\nname = \"guidance-core\"\n"),
        Some("guidance-core".to_string())
    );
    assert_eq!(
        crate::read::doc_cards::manifest_package_name("[workspace]\nmembers = []\n"),
        None
    );
}

#[test]
fn cards_carry_workspace_crates_and_domains_in_key_order() {
    let dir = map_fixture();
    let cards = build_project_cards(dir.path(), "fixture", None);
    assert!(!cards.is_empty(), "{cards:?}");
    let keys: Vec<&str> = cards.iter().map(|card| card.key.as_str()).collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(keys, sorted, "card order is deterministic: {keys:?}");
    let workspace = cards
        .iter()
        .find(|card| card.key == "workspace:fixture")
        .expect("workspace card");
    assert!(
        workspace.body.contains("src/guidance"),
        "{}",
        workspace.body
    );
    assert!(
        workspace.body.contains("guidance-core"),
        "{}",
        workspace.body
    );
    let guidance = cards
        .iter()
        .find(|card| card.key == "crate:guidance-core")
        .expect("crate card");
    assert_eq!(guidance.kind, "crate");
    assert!(
        guidance.body.contains("Local code index for recall."),
        "{}",
        guidance.body
    );
    assert_eq!(
        guidance.source_path, "src/guidance/README.md",
        "{guidance:?}"
    );
    assert_eq!(guidance.anchor, "guidance-core", "{guidance:?}");
    assert_eq!(
        guidance.code_paths_json, r#"["src/guidance"]"#,
        "{guidance:?}"
    );
    let router = cards
        .iter()
        .find(|card| card.key == "crate:fluent-router")
        .expect("router card");
    assert_eq!(router.code_paths_json, r#"["src/router"]"#, "{router:?}");
    assert!(cards.iter().any(|card| card.kind == "domain"), "{keys:?}");
    let again = build_project_cards(dir.path(), "fixture", None);
    let timeless = |cards: &Vec<ProjectCard>| {
        cards
            .iter()
            .map(|card| {
                let mut card = card.clone();
                card.generated_at = 0;
                card
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        timeless(&cards),
        timeless(&again),
        "card derivation is deterministic"
    );
    for card in &cards {
        assert!(card.generated_at > 0, "{}", card.key);
    }
}

#[test]
fn cards_rederive_on_manifest_change_only() {
    let dir = map_fixture();
    let before = build_project_cards(dir.path(), "fixture", None);
    std::fs::write(
        dir.path().join("src/router/Cargo.toml"),
        "[package]\nname = \"fluent-router\"\ndescription = \"Routes requests edited.\"\n",
    )
    .expect("edit manifest");
    let after = build_project_cards(dir.path(), "fixture", None);
    assert_eq!(before.len(), after.len());
    for card in &before {
        let next = after
            .iter()
            .find(|c| c.key == card.key)
            .expect("stable key");
        if card.key == "crate:fluent-router" {
            assert_ne!(card.content_hash, next.content_hash);
            assert_ne!(card.body, next.body);
        } else {
            assert_eq!(card.content_hash, next.content_hash, "{}", card.key);
        }
    }
}

#[test]
fn unknown_crate_falls_back_without_dropping() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"src/mystery\"]\n",
    )
    .expect("write");
    std::fs::create_dir_all(root.join("src/mystery")).expect("mkdir");
    std::fs::write(
        root.join("src/mystery/Cargo.toml"),
        "[package]\nname = \"mystery\"\ndescription = \"Mystery crate.\"\n",
    )
    .expect("write");
    let cards = build_project_cards(root, "fixture", None);
    let mystery = cards
        .iter()
        .find(|card| card.key == "crate:mystery")
        .expect("mystery card");
    assert!(mystery.body.contains("unknown"), "{}", mystery.body);
}

#[test]
fn import_stores_incrementally_and_skips_unchanged() {
    let dir = map_fixture();
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    let first = import_project_cards(dir.path(), "fixture", &db, None).expect("import");
    assert!(first.cards > 0, "{first:?}");
    assert_eq!(first.skipped_unchanged, 0, "{first:?}");
    assert_eq!(
        db.project_card_count().expect("count") as usize,
        first.cards,
        "{first:?}"
    );
    let second = import_project_cards(dir.path(), "fixture", &db, None).expect("re-import");
    assert_eq!(second.cards, 0, "{second:?}");
    assert_eq!(second.skipped_unchanged, first.cards, "{second:?}");
    std::fs::write(
        dir.path().join("src/router/Cargo.toml"),
        "[package]\nname = \"fluent-router\"\ndescription = \"Routes requests edited.\"\n",
    )
    .expect("edit manifest");
    let third = import_project_cards(dir.path(), "fixture", &db, None).expect("re-import");
    assert_eq!(third.cards, 1, "{third:?}");
    assert_eq!(third.skipped_unchanged, first.cards - 1, "{third:?}");
}

#[test]
fn readme_builds_verbatim_doc_cards() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("README.md"),
        "# Root\n\nIntro prose.\n\n## Sync\n\nSync reprocesses stale files.\n",
    )
    .expect("write");
    let cfg = crate::config::ProjectConfig::default();
    let cards = crate::read::project_map::build_doc_cards(dir.path(), &cfg, None);
    assert!(cards.iter().any(|card| card.kind == "doc"), "{cards:?}");
    let sync = cards
        .iter()
        .find(|card| card.title.contains("Sync"))
        .expect("sync section");
    assert!(
        sync.body.contains("Sync reprocesses stale files."),
        "{sync:?}"
    );
    let (chunks, _) = crate::read::doc_cards::chunk_markdown_sections(
        &sync.source_path,
        &std::fs::read_to_string(dir.path().join("README.md")).expect("read"),
    );
    assert_eq!(
        sync.content_hash,
        chunks
            .iter()
            .find(|chunk| chunk.anchor == sync.anchor)
            .expect("chunk")
            .content_hash
    );
}
