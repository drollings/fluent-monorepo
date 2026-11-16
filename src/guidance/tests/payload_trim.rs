//! Payload trim: scoped queries render the scoped project map while
//! unscoped queries and `--full-map` keep the whole member dump. The
//! map frame (workspace card) survives every trim.
use super::*;
use crate::read::planes::orientation::CardFinding;
use crate::tests::common::db_project_card;
use search_vector::db::GuidanceDb;

fn card(kind: &str, title: &str, code_paths: &[&str]) -> CardFinding {
    CardFinding {
        id: format!("{kind}:{title}"),
        kind: kind.to_string(),
        title: title.to_string(),
        body: format!("{title} body."),
        source: "Cargo.toml".to_string(),
        code_paths: code_paths.iter().map(|path| (*path).to_string()).collect(),
    }
}

#[test]
fn scoped_cards_keep_the_frame_and_members_in_scope() {
    let cards = vec![
        card("workspace", "Workspace", &[]),
        card("crate", "guidance-core", &[]),
        card("crate", "coral-router", &[]),
        card("domain", "router", &["src/router/src/lib.rs"]),
    ];
    let kept = scoped_project_cards(cards.clone(), &["fluent-router".to_string()]);
    let titles: Vec<&str> = kept.iter().map(|card| card.title.as_str()).collect();
    assert_eq!(titles, vec!["Workspace", "router"]);
    let frame_only = scoped_project_cards(cards, &["no-such-member".to_string()]);
    assert_eq!(frame_only.len(), 1, "an unmatched scope keeps the frame");
    assert_eq!(frame_only[0].kind, "workspace");
}

fn map_workspace() -> (tempfile::TempDir, GuidanceDb) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").expect("write");
    std::fs::create_dir_all(dir.path().join("src/guidance/src")).expect("mkdir");
    std::fs::create_dir_all(dir.path().join("src/router/src")).expect("mkdir");
    std::fs::write(
        dir.path().join("src/guidance/src/sync.rs"),
        "//! Guidance sync converges the derived state.\npub fn sync() {}\n",
    )
    .expect("write");
    std::fs::write(
        dir.path().join("src/router/src/lib.rs"),
        "//! Coral router routes requests.\npub fn route() {}\n",
    )
    .expect("write");
    let db = GuidanceDb::open_in_memory().expect("db");
    db.replace_project_cards(&[
        db_project_card(
            "workspace:ws",
            "workspace",
            "Workspace",
            "Ships guidance and coral-router.",
            "Cargo.toml",
            "",
        ),
        db_project_card(
            "crate:guidance-core",
            "crate",
            "guidance-core",
            "Sync, freshness, and the read plane.",
            "Cargo.toml",
            "",
        ),
        db_project_card(
            "crate:coral-router",
            "crate",
            "coral-router",
            "Routes requests to local models.",
            "Cargo.toml",
            "",
        ),
    ])
    .expect("cards seed");
    (dir, db)
}

fn project_map(query: &str, full_map: bool) -> String {
    let (dir, db) = map_workspace();
    let engine = if full_map {
        ReadEngine::new().with_full_map()
    } else {
        ReadEngine::new()
    };
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    let answer = engine.explain(
        &ExplainRequest {
            query: query.to_string(),
            limit: 5,
            budget_override: None,
            no_llm: true,
            include_fixtures: false,
            answer_mode: AnswerMode::Auto,
        },
        &ctx,
        None,
    );
    answer
        .sections
        .iter()
        .find(|section| section.title == "From the project map")
        .map(|section| section.body.clone())
        .unwrap_or_default()
}

#[test]
fn scoped_queries_trim_the_member_dump() {
    let scoped = project_map("how does guidance-core sync work", false);
    assert!(scoped.contains("#### guidance-core"), "{scoped}");
    assert!(
        !scoped.contains("#### coral-router"),
        "unscoped members stay out: {scoped}"
    );
}

#[test]
fn unscoped_queries_keep_the_whole_dump() {
    let map = project_map("What are the major components of this codebase?", false);
    assert!(map.contains("#### guidance-core"), "{map}");
    assert!(map.contains("#### coral-router"), "{map}");
}

#[test]
fn full_map_restores_the_dump_for_scoped_queries() {
    let map = project_map("how does guidance-core sync work", true);
    assert!(map.contains("#### guidance-core"), "{map}");
    assert!(map.contains("#### coral-router"), "{map}");
}
