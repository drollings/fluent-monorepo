use crate::query::search_db::capabilities::{
    build_capability_fts_match, CapabilityLemma, CapabilityRecord,
};
use crate::query::search_db::GuidanceSearchDb;

fn row(id: &str, purpose: &str) -> CapabilityRecord {
    CapabilityRecord {
        id: id.to_string(),
        project: "monorepo".to_string(),
        crate_name: "guidance-core".to_string(),
        domain: "guidance".to_string(),
        title: "Index".to_string(),
        purpose: purpose.to_string(),
        predicate: "index".to_string(),
        roles_json: r#"["fragment"]"#.to_string(),
        keywords_json: r#"["recall"]"#.to_string(),
        code_paths_json: r#"["src/guidance/src/query/recall.rs"]"#.to_string(),
        symbols_json: "[]".to_string(),
        source_path: "doc/guidance/index.md".to_string(),
        anchor: "index".to_string(),
        content_hash: "hash-a".to_string(),
        model: "test-model".to_string(),
        prompt_version: 1,
        origin: "test".to_string(),
        corpus_hash: String::new(),
        status: "ready".to_string(),
        gap_reason: None,
        generated_at: 1700000000000,
    }
}

#[test]
fn capability_rows_round_trip_with_provenance() {
    let db = GuidanceSearchDb::open_in_memory().expect("db");
    let rows = vec![row(
        "doc/guidance/index.md#index",
        "Indexes fragments for recall.",
    )];
    assert_eq!(db.replace_capabilities(&rows).expect("replace"), 1);
    let stored = db
        .capabilities_for_source("doc/guidance/index.md")
        .expect("read");
    assert_eq!(stored.len(), 1);
    let stored_row = &stored[0];
    assert_eq!(stored_row.purpose, "Indexes fragments for recall.");
    assert_eq!(stored_row.project, "monorepo");
    assert_eq!(stored_row.crate_name, "guidance-core");
    assert_eq!(stored_row.domain, "guidance");
    assert_eq!(stored_row.predicate, "index");
    assert_eq!(stored_row.roles_json, r#"["fragment"]"#);
    assert_eq!(stored_row.anchor, "index");
    assert_eq!(stored_row.content_hash, "hash-a");
    assert_eq!(stored_row.model, "test-model");
    assert_eq!(stored_row.prompt_version, 1);
    assert_eq!(stored_row.status, "ready");
    assert_eq!(stored_row.generated_at, 1700000000000);
}

#[test]
fn capability_ready_count_covers_only_ready_rows() {
    // The tier-honesty read: `held` and `gap` rows persist for retry but
    // never count toward the available tier.
    let db = GuidanceSearchDb::open_in_memory().expect("db");
    assert_eq!(db.capability_ready_count().expect("count"), 0);
    let mut held = row("b#held", "Brief the model could not serve.");
    held.status = "held".to_string();
    let mut gap = row("c#gap", "Brief that failed validation.");
    gap.status = "gap".to_string();
    db.replace_capabilities(&[row("a#index", "Indexes fragments for recall."), held, gap])
        .expect("replace");
    assert_eq!(db.capability_ready_count().expect("count"), 1);
}

#[test]
fn capability_fts_finds_brief_terms_only() {
    // Brief text and the SVO predicate are lexically reachable;
    // source-file terms outside briefs must not match this route.
    let db = GuidanceSearchDb::open_in_memory().expect("db");
    db.replace_capabilities(&[row("a#index", "Indexes fragments for recall.")])
        .expect("replace");
    let hits = db.search_capabilities("fragments", 10).expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "a#index");
    let hits = db.search_capabilities("index", 10).expect("search");
    assert_eq!(hits.len(), 1, "predicate column participates: {hits:?}");
    assert!(db
        .search_capabilities("zzzz", 10)
        .expect("search")
        .is_empty());
}

#[test]
fn capability_fts_never_matches_everything() {
    let db = GuidanceSearchDb::open_in_memory().expect("db");
    db.replace_capabilities(&[row("a#index", "Indexes fragments for recall.")])
        .expect("replace");
    assert!(build_capability_fts_match("").is_none());
    assert!(build_capability_fts_match("   ").is_none());
    assert!(build_capability_fts_match("!!!").is_none());
    for query in ["", "   ", "!!!"] {
        assert!(
            db.search_capabilities(query, 10)
                .expect("search")
                .is_empty(),
            "query {query:?} must match nothing"
        );
    }
}

#[test]
fn capability_replace_updates_fts_rows() {
    let db = GuidanceSearchDb::open_in_memory().expect("db");
    db.replace_capabilities(&[row("a#index", "Indexes fragments for recall.")])
        .expect("replace");
    let mut second = row("a#index", "Summarizes comments for retrieval.");
    second.content_hash = "hash-b".to_string();
    db.replace_capabilities(&[second]).expect("replace");
    assert!(
        db.search_capabilities("fragments", 10)
            .expect("search")
            .is_empty(),
        "stale brief text must leave the index"
    );
    let hits = db.search_capabilities("summarizes", 10).expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "a#index");
}

#[test]
fn capability_title_and_keywords_are_lexically_reachable() {
    let db = GuidanceSearchDb::open_in_memory().expect("db");
    let mut card = row("a#card", "Indexes fragments for recall.");
    card.title = "Zebra routing".to_string();
    card.keywords_json = r#"["quuxalpha"]"#.to_string();
    db.replace_capabilities(&[card]).expect("replace");
    let hits = db.search_capabilities("zebra", 10).expect("search");
    assert_eq!(hits.len(), 1, "title participates: {hits:?}");
    let hits = db.search_capabilities("quuxalpha", 10).expect("search");
    assert_eq!(hits.len(), 1, "keywords participate: {hits:?}");
}

#[test]
fn project_cards_round_trip_in_key_order() {
    use crate::query::search_db::ProjectCard;
    let db = GuidanceSearchDb::open_in_memory().expect("db");
    assert_eq!(db.project_card_count().expect("count"), 0);
    let card = |key: &str, kind: &str| ProjectCard {
        key: key.to_string(),
        kind: kind.to_string(),
        title: format!("{kind} {key}"),
        body: format!("facts about {key}"),
        source_path: "README.md".to_string(),
        anchor: String::new(),
        aliases_json: "[]".to_string(),
        code_paths_json: "[]".to_string(),
        symbols_json: "[]".to_string(),
        content_hash: format!("hash-{key}"),
        generated_at: 1700000000000,
    };
    db.replace_project_cards(&[card("crate:b", "crate"), card("crate:a", "crate")])
        .expect("replace");
    assert_eq!(db.project_card_count().expect("count"), 2);
    let stored = db.project_cards_all().expect("read");
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[0].key, "crate:a");
    assert_eq!(stored[1].key, "crate:b");
    assert_eq!(stored[0].content_hash, "hash-crate:a");
}

#[test]
fn capability_ids_fetch_ready_rows_only() {
    let db = GuidanceSearchDb::open_in_memory().expect("db");
    let mut held = row("a#held", "Held brief text.");
    held.status = "held".to_string();
    db.replace_capabilities(&[row("a#one", "First brief."), held])
        .expect("replace");
    let rows = db
        .capabilities_for_ids(&[
            "a#one".to_string(),
            "a#held".to_string(),
            "a#missing".to_string(),
        ])
        .expect("read");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].id, "a#one");
    assert!(db.capabilities_for_ids(&[]).expect("read").is_empty());
}

#[test]
fn capability_lemmas_post_and_recall() {
    // Lemma postings mirror the fragment route: most matches first,
    // empty input recalls nothing.
    let db = GuidanceSearchDb::open_in_memory().expect("db");
    db.replace_capabilities(&[
        row("a#one", "Indexes fragments for recall."),
        row("a#two", "Indexes summaries for fusion."),
    ])
    .expect("replace");
    let postings = vec![
        CapabilityLemma {
            capability_id: "a#one".to_string(),
            lemma: "fragment".to_string(),
            confidence: 1.0,
        },
        CapabilityLemma {
            capability_id: "a#two".to_string(),
            lemma: "fragment".to_string(),
            confidence: 1.0,
        },
        CapabilityLemma {
            capability_id: "a#two".to_string(),
            lemma: "summary".to_string(),
            confidence: 1.0,
        },
    ];
    assert_eq!(db.insert_capability_lemmas(&postings).expect("insert"), 3);
    let ids = db
        .capabilities_for_lemmas(&["fragment".to_string(), "summary".to_string()], 10)
        .expect("recall");
    assert_eq!(ids, vec!["a#two".to_string(), "a#one".to_string()]);
    assert!(db
        .capabilities_for_lemmas(&[], 10)
        .expect("recall")
        .is_empty());
}

#[test]
fn capability_lemmas_ignore_unknown_ids() {
    // Postings for rows that do not exist are skipped, never stored.
    let db = GuidanceSearchDb::open_in_memory().expect("db");
    db.replace_capabilities(&[row("a#one", "Indexes fragments.")])
        .expect("replace");
    let postings = vec![CapabilityLemma {
        capability_id: "missing#id".to_string(),
        lemma: "fragment".to_string(),
        confidence: 1.0,
    }];
    assert_eq!(db.insert_capability_lemmas(&postings).expect("insert"), 1);
    assert!(db
        .capabilities_for_lemmas(&["fragment".to_string()], 10)
        .expect("recall")
        .is_empty());
}
