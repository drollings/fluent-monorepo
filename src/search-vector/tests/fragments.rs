use super::*;

// P1 fragment tables: FTS5 + group collapse + filter pushdown +
// `fragment_lemmas` + brute-force KNN over `fragments`. The legacy
// `guidance_nodes` behavior is untouched (P5 regression gate).

fn file_a() -> FileRecord {
    FileRecord {
        id: "file-a".to_string(),
        absolute_path: "/repo/src/a.ts".to_string(),
        relative_path: "src/a.ts".to_string(),
        root_path: "/repo".to_string(),
        size_bytes: 100,
        last_modified_time: 100,
        kind: Some("code".to_string()),
        format: "typescript".to_string(),
        content_hash: None,
        index_status: Some("indexed".to_string()),
        fail_count: 0,
        last_error: None,
    }
}

fn frag(id: &str, group: Option<&str>, symbol: &str, text: &str) -> FragmentRecord {
    FragmentRecord {
        id: id.to_string(),
        group: group.map(str::to_string),
        file_id: "file-a".to_string(),
        range_json: "{\"kind\":\"file\"}".to_string(),
        content_kind: "text".to_string(),
        content_text: Some(text.to_string()),
        cjk_text: String::new(),
        symbol_type: Some("function".to_string()),
        symbol_name: Some(symbol.to_string()),
        scope: None,
        signature: Some(format!("function {symbol}()")),
        doc: None,
        modifiers: "exported".to_string(),
        heading: None,
        heading_level: None,
        embedding: None,
    }
}

fn lemma(fragment_id: &str, lemma: &str, confidence: f64) -> FragmentLemma {
    FragmentLemma {
        fragment_id: fragment_id.to_string(),
        lemma: lemma.to_string(),
        confidence,
    }
}

fn frag_with_doc(id: &str, symbol: &str, text: &str, doc: &str) -> FragmentRecord {
    let mut record = frag(id, None, symbol, text);
    record.doc = Some(doc.to_string());
    record
}

#[test]
fn fts_finds_comment_only_terms_in_doc() {
    // Comment-only terms live in `doc`, not in the body: the query must
    // still find the row after the `doc` column joins the FTS index.
    let db = GuidanceDb::open_in_memory().expect("db");
    db.upsert_fragments(
        &file_a(),
        &[frag_with_doc(
            "entity-doc",
            "Widget",
            "pub struct Widget {}",
            "Adds one to the total",
        )],
        &[],
    )
    .expect("upsert");
    let filter = FragmentFilter::default();
    let hits = db
        .search_fts("total", 10, &filter)
        .expect("search");
    assert!(
        hits.iter().any(|h| h.id == "entity-doc"),
        "comment-only term must find the row"
    );
    // Body and symbol queries are unchanged.
    let hits = db
        .search_fts("Widget", 10, &filter)
        .expect("search");
    assert!(hits.iter().any(|h| h.id == "entity-doc"));
}

#[test]
fn fts_doc_terms_do_not_leak_across_fragments() {
    // A term present only in one fragment's `doc` must not match siblings.
    let db = GuidanceDb::open_in_memory().expect("db");
    db.upsert_fragments(
        &file_a(),
        &[
            frag_with_doc("entity-doc", "Widget", "pub struct Widget {}", "ZebraStripe note"),
            frag("entity-plain", None, "Gadget", "pub struct Gadget {}"),
        ],
        &[],
    )
    .expect("upsert");
    let hits = db
        .search_fts("ZebraStripe", 10, &FragmentFilter::default())
        .expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "entity-doc");
}

#[test]
fn fts_cjk_content_match_survives_doc_column() {
    // The CJK bigram route is unchanged by the `doc` column.
    let db = GuidanceDb::open_in_memory().expect("db");
    let mut f = frag_with_doc("entity-cjk", "Segment", "日本語の検索テスト", "Chinese comment");
    f.cjk_text = "日本 本語 語の の検 検索 索テ テス スト".to_string();
    db.upsert_fragments(&file_a(), &[f], &[]).expect("upsert");
    let hits = db
        .search_fts("日本語", 10, &FragmentFilter::default())
        .expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "entity-cjk");
}

fn seed_alpha(db: &GuidanceDb) {
    let fragments = vec![
        frag(
            "entity-a",
            Some("entity-a"),
            "AlphaSymbol",
            "export function AlphaSymbol() {}",
        ),
        frag(
            "entity-a#1",
            Some("entity-a"),
            "AlphaSymbol",
            "AlphaSymbol helper window",
        ),
    ];
    db.upsert_fragments(&file_a(), &fragments, &[])
        .expect("upsert");
}

#[test]
fn fts_finds_symbol_text() {
    let db = GuidanceDb::open_in_memory().expect("db");
    seed_alpha(&db);
    let hits = db
        .search_fts("AlphaSymbol", 10, &FragmentFilter::default())
        .expect("search");
    assert!(!hits.is_empty());
    assert!(hits.iter().any(|h| h.id == "entity-a"));
}

#[test]
fn fts_empty_query_never_matches_all() {
    let db = GuidanceDb::open_in_memory().expect("db");
    seed_alpha(&db);
    for query in ["", "   ", "!!!"] {
        let hits = db
            .search_fts(query, 10, &FragmentFilter::default())
            .expect("search");
        assert!(hits.is_empty(), "query {query:?} must match nothing");
    }
}

#[test]
fn fts_matches_cjk_substring_via_bigram_column() {
    let db = GuidanceDb::open_in_memory().expect("db");
    let mut f = frag("entity-cjk", None, "Segment", "日本語の検索テスト");
    f.cjk_text = "日本 本語 語の の検 検索 索テ テス スト".to_string();
    db.upsert_fragments(&file_a(), &[f], &[]).expect("upsert");
    let hits = db
        .search_fts("日本語", 10, &FragmentFilter::default())
        .expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "entity-cjk");
}

#[test]
fn fts_respects_filter_pushdown() {
    let db = GuidanceDb::open_in_memory().expect("db");
    seed_alpha(&db);
    // Wrong file allow-list → nothing.
    let filter = FragmentFilter {
        file_ids: Some(vec!["file-missing".to_string()]),
        ..Default::default()
    };
    assert!(db
        .search_fts("AlphaSymbol", 10, &filter)
        .expect("search")
        .is_empty());
    // Right file → hits.
    let filter = FragmentFilter {
        file_ids: Some(vec!["file-a".to_string()]),
        ..Default::default()
    };
    assert!(!db
        .search_fts("AlphaSymbol", 10, &filter)
        .expect("search")
        .is_empty());
    // Symbol-name allow-list.
    let filter = FragmentFilter {
        symbol_names: vec!["Nope".to_string()],
        ..Default::default()
    };
    assert!(db
        .search_fts("AlphaSymbol", 10, &filter)
        .expect("search")
        .is_empty());
    // Symbol-type allow-list.
    let filter = FragmentFilter {
        symbol_types: vec!["class".to_string()],
        ..Default::default()
    };
    assert!(db
        .search_fts("AlphaSymbol", 10, &filter)
        .expect("search")
        .is_empty());
    // mtime window excludes file-a (mtime 100).
    let filter = FragmentFilter {
        modified_after: Some(200),
        ..Default::default()
    };
    assert!(db
        .search_fts("AlphaSymbol", 10, &filter)
        .expect("search")
        .is_empty());
    let filter = FragmentFilter {
        modified_after: Some(50),
        modified_before: Some(250),
        ..Default::default()
    };
    assert!(!db
        .search_fts("AlphaSymbol", 10, &filter)
        .expect("search")
        .is_empty());
}

#[test]
fn upsert_replaces_file_fragments_and_fts_rows() {
    let db = GuidanceDb::open_in_memory().expect("db");
    seed_alpha(&db);
    db.upsert_fragments(
        &file_a(),
        &[frag(
            "entity-b",
            None,
            "BetaSymbol",
            "export function BetaSymbol() {}",
        )],
        &[],
    )
    .expect("re-upsert");
    let filter = FragmentFilter::default();
    assert!(db
        .search_fts("AlphaSymbol", 10, &filter)
        .expect("search")
        .is_empty());
    let hits = db.search_fts("BetaSymbol", 10, &filter).expect("search");
    assert_eq!(hits.len(), 1);
}

#[test]
fn fragments_for_entity_returns_major_and_minors() {
    let db = GuidanceDb::open_in_memory().expect("db");
    seed_alpha(&db);
    let rows = db.fragments_for_entity("entity-a").expect("fetch");
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|r| r.id == "entity-a"));
}

#[test]
fn lemmas_round_trip_per_fragment() {
    let db = GuidanceDb::open_in_memory().expect("db");
    seed_alpha(&db);
    db.insert_fragment_lemmas(&[
        lemma("entity-a", "alpha", 0.9),
        lemma("entity-a#1", "help", 0.8),
    ])
    .expect("lemmas");
    let ids = db
        .fragments_for_lemmas(&["alpha".to_string()], 10)
        .expect("lookup");
    assert_eq!(ids, vec!["entity-a".to_string()]);
    // Re-upsert replaces lemma rows with the file.
    db.upsert_fragments(&file_a(), &[frag("entity-b", None, "BetaSymbol", "x")], &[])
        .expect("re-upsert");
    let ids = db
        .fragments_for_lemmas(&["alpha".to_string()], 10)
        .expect("lookup");
    assert!(ids.is_empty());
}

#[test]
fn lemma_lookup_orders_full_matches_before_partial() {
    let db = GuidanceDb::open_in_memory().expect("db");
    // Partial matches inserted FIRST, so pure rowid order would bury the
    // exact match (the S1 `scale_needle_042` failure: insertion-ordered
    // lemma membership let a non-discriminative route outvote FTS rank 1
    // in RRF fusion).
    db.upsert_fragments(
        &file_a(),
        &[
            frag("partial-b", None, "B", "b"),
            frag("partial-c", None, "C", "c"),
            frag("exact-a", None, "A", "a"),
        ],
        &[],
    )
    .expect("upsert");
    db.insert_fragment_lemmas(&[
        lemma("partial-b", "scale", 1.0),
        lemma("partial-b", "needle", 1.0),
        lemma("partial-c", "scale", 1.0),
        lemma("partial-c", "needle", 1.0),
        lemma("exact-a", "scale", 1.0),
        lemma("exact-a", "needle", 1.0),
        lemma("exact-a", "042", 1.0),
    ])
    .expect("lemmas");
    let ids = db
        .fragments_for_lemmas(
            &["scale".to_string(), "needle".to_string(), "042".to_string()],
            10,
        )
        .expect("lookup");
    assert_eq!(ids.len(), 3);
    assert_eq!(
        ids[0], "exact-a",
        "fragment matching all query lemmas must outrank partial matches"
    );
}

#[test]
fn lemma_lookup_keeps_insertion_order_on_count_ties() {
    // Must-NOT-fire control: uniform match counts preserve today's rowid
    // order exactly — the promotion must only fire on count differences.
    let db = GuidanceDb::open_in_memory().expect("db");
    db.upsert_fragments(
        &file_a(),
        &[
            frag("partial-b", None, "B", "b"),
            frag("partial-c", None, "C", "c"),
            frag("exact-a", None, "A", "a"),
        ],
        &[],
    )
    .expect("upsert");
    db.insert_fragment_lemmas(&[
        lemma("partial-b", "scale", 1.0),
        lemma("partial-c", "scale", 1.0),
        lemma("exact-a", "scale", 1.0),
    ])
    .expect("lemmas");
    let ids = db
        .fragments_for_lemmas(&["scale".to_string()], 10)
        .expect("lookup");
    assert_eq!(ids, vec!["partial-b", "partial-c", "exact-a"]);
}

#[test]
fn vector_search_orders_by_similarity_and_skips_dim_mismatch() {
    let db = GuidanceDb::open_in_memory().expect("db");
    let mut near = frag("near", None, "Near", "near code");
    near.embedding = Some(vec![1.0, 0.0]);
    let mut far = frag("far", None, "Far", "far code");
    far.embedding = Some(vec![0.0, 1.0]);
    let mut wrong_dim = frag("wrong", None, "Wrong", "wrong dims");
    wrong_dim.embedding = Some(vec![1.0, 0.0, 0.0]);
    db.upsert_fragments(&file_a(), &[near, far, wrong_dim], &[])
        .expect("upsert");
    let hits = db
        .search_vector_fragments(&[1.0, 0.0], 10, &FragmentFilter::default())
        .expect("search");
    let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(ids, vec!["near", "far"]);
    assert!(hits[0].score >= hits[1].score);
}

#[test]
fn match_expression_quotes_specials_and_splits_axes() {
    // ASCII tokens hit the text/symbol/doc columns; CJK becomes bigram ORs.
    let expr = build_fts_match("find AlphaSymbol").expect("expr");
    assert!(expr.contains("alphasymbol"), "got {expr}");
    assert!(expr.contains("doc"), "got {expr}");
    let expr = build_fts_match("日本語").expect("expr");
    assert!(expr.contains("cjk_text"), "got {expr}");
    assert!(expr.contains("日本"), "got {expr}");
    // Nothing indexable → None (never match-all).
    assert!(build_fts_match("!!!").is_none());
    assert!(build_fts_match("").is_none());
}

#[test]
fn files_and_group_diagnostics_round_trip() {
    let db = GuidanceDb::open_in_memory().expect("db");
    seed_alpha(&db);
    let files = db.list_files().expect("files");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].relative_path, "src/a.ts");
    let groups = db
        .group_ids_for_files(&["file-a".to_string()])
        .expect("groups");
    assert_eq!(groups, vec!["entity-a".to_string()]);
}

#[test]
fn vector_search_prefers_q8_and_matches_fp32_order() {
    // Same near/far layout as the fp32 test: q8 recall must agree.
    let db = GuidanceDb::open_in_memory().expect("db");
    let mut near = frag("near", None, "Near", "near code");
    near.embedding = Some(vec![1.0, 0.0]);
    let mut far = frag("far", None, "Far", "far code");
    far.embedding = Some(vec![0.0, 1.0]);
    db.upsert_fragments(&file_a(), &[near, far], &[])
        .expect("upsert");
    let hits = db
        .search_vector_fragments(&[1.0, 0.0], 10, &FragmentFilter::default())
        .expect("search");
    let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(ids, vec!["near", "far"]);
}

#[test]
fn upsert_stores_q8_only_and_saves_bytes() {
    let db = GuidanceDb::open_in_memory().expect("db");
    let dims = 64;
    let vec: Vec<f32> = (0..dims)
        .map(|i| (i as f32) / (dims as f32) - 0.5)
        .collect();
    let mut with_vec = frag("vec-row", None, "VecRow", "vector row");
    with_vec.embedding = Some(vec.clone());
    db.upsert_fragments(&file_a(), &[with_vec], &[])
        .expect("upsert");
    let (fp32_blob, q8_blob): (Option<Vec<u8>>, Option<Vec<u8>>) = db
        .store
        .query_row(
            "SELECT embedding, embedding_q8 FROM fragments WHERE id = 'vec-row'",
            &[],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("row")
        .unwrap();
    assert!(fp32_blob.is_none(), "new rows store no fp32 blob");
    let q8_blob = q8_blob.expect("q8 blob");
    assert_eq!(q8_blob.len(), (dims as usize) + 8);
    assert!(q8_blob.len() * 3 < dims as usize * 4, "q8 < 1/3 of fp32");
}

#[test]
fn vector_search_reads_legacy_fp32_rows() {
    // A pre-P6 row (fp32 only, no q8) stays searchable via the fallback.
    let db = GuidanceDb::open_in_memory().expect("db");
    let mut legacy = frag("legacy", None, "Legacy", "legacy code");
    legacy.embedding = Some(vec![1.0, 0.0]);
    db.upsert_fragments(&file_a(), &[legacy], &[])
        .expect("upsert");
    let fp32_blob = fluent_db::vector::vec_to_bytes(&[1.0f32, 0.0]);
    db.store
        .execute(
            "UPDATE fragments SET embedding = ?1, embedding_q8 = NULL WHERE id = 'legacy'",
            &[&fp32_blob],
        )
        .expect("legacy fp32 row");
    let hits = db
        .search_vector_fragments(&[1.0, 0.0], 10, &FragmentFilter::default())
        .expect("search");
    assert!(
        hits.iter().any(|h| h.id == "legacy"),
        "legacy fp32 row found"
    );
}

#[test]
fn fragment_freshness_none_for_unknown_file() {
    // No committed row → no fingerprint (caller must ingest).
    let db = GuidanceDb::open_in_memory().expect("db");
    assert_eq!(
        db.fragment_freshness("never-ingested").expect("query"),
        None
    );
}

#[test]
fn fragment_freshness_matches_committed_fingerprint() {
    // The fingerprint mirrors exactly what `upsert_fragments` stores.
    let db = GuidanceDb::open_in_memory().expect("db");
    db.upsert_fragments(&file_a(), &[frag("f1", None, "F1", "body")], &[])
        .expect("upsert");
    assert_eq!(
        db.fragment_freshness("file-a").expect("query"),
        Some(FragmentFreshness {
            size_bytes: 100,
            last_modified_time: 100,
            embedded: false,
        })
    );
}

#[test]
fn fragment_freshness_reports_embedded_flag() {
    // A caller with a newly configured embedder must see that vectors
    // still need backfilling, even when size+mtime match.
    let db = GuidanceDb::open_in_memory().expect("db");
    let mut plain = frag("plain-row", None, "Plain", "plain body");
    plain.embedding = None;
    db.upsert_fragments(&file_a(), &[plain], &[])
        .expect("upsert");
    assert!(
        !db.fragment_freshness("file-a")
            .expect("query")
            .expect("row")
            .embedded
    );
    let mut with_vec = frag("vec-row", None, "VecRow", "vector body");
    with_vec.embedding = Some(vec![0.5, -0.5]);
    db.upsert_fragments(&file_a(), &[with_vec], &[])
        .expect("upsert");
    assert!(
        db.fragment_freshness("file-a")
            .expect("query")
            .expect("row")
            .embedded
    );
}

#[test]
fn embedding_status_round_trips_per_file() {
    use super::super::db::embedding_status::EmbeddingStatusRecord;
    let db = GuidanceDb::open_in_memory().expect("db");
    seed_alpha(&db);
    let rows = vec![
        EmbeddingStatusRecord {
            fragment_id: "entity-a".to_string(),
            input_hash: "aaa".to_string(),
            embedder: "model-x:4".to_string(),
            status: "ready".to_string(),
            updated_at: 1,
        },
        EmbeddingStatusRecord {
            fragment_id: "entity-a#1".to_string(),
            input_hash: "bbb".to_string(),
            embedder: "model-x:4".to_string(),
            status: "failed".to_string(),
            updated_at: 1,
        },
    ];
    assert_eq!(db.replace_embedding_status(&rows).expect("replace"), 2);
    let stored = db.embedding_status_for_file("file-a").expect("read");
    assert_eq!(stored.len(), 2);
    let failed: Vec<_> = stored.iter().filter(|row| row.status == "failed").collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].fragment_id, "entity-a#1");
    assert_eq!(failed[0].embedder, "model-x:4");
    assert!(db
        .files_needing_embed_backfill("model-x:4")
        .expect("backfill")
        .is_empty(), "same embedder never backfills");
    let uuid = file_b();
    let mut other = frag("entity-b", None, "Beta", "x");
    other.file_id = "file-b".to_string();
    db.upsert_fragments(&uuid, &[other], &[]).expect("upsert");
    db.replace_embedding_status(&[EmbeddingStatusRecord {
        fragment_id: "entity-b".to_string(),
        input_hash: "ccc".to_string(),
        embedder: "model-y:4".to_string(),
        status: "ready".to_string(),
        updated_at: 1,
    }])
    .expect("replace");
    assert_eq!(
        db.files_needing_embed_backfill("model-x:4").expect("backfill"),
        vec!["file-b".to_string()],
        "rotated embedders backfill even when ready"
    );
}

fn file_b() -> FileRecord {
    FileRecord {
        id: "file-b".to_string(),
        ..file_a()
    }
}

#[test]
fn fragment_counts_cover_embedded_and_plain_rows() {
    let db = GuidanceDb::open_in_memory().expect("db");
    assert_eq!(db.fragment_count().expect("count"), 0);
    let mut with_vec = frag("vec-row", None, "VecRow", "vector row");
    with_vec.embedding = Some(vec![0.5, -0.5]);
    db.upsert_fragments(&file_a(), &[frag("plain-row", None, "Plain", "plain"), with_vec], &[])
        .expect("upsert");
    assert_eq!(db.fragment_count().expect("count"), 2);
    assert_eq!(db.embedded_fragment_count().expect("embedded"), 1);
}

#[test]
fn old_lemma_table_migrates_to_dictionary_on_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("mig.db");
    let db = GuidanceDb::open(&path).expect("open");
    db.upsert_fragments(&file_a(), &[frag("entity-a", None, "Alpha", "alpha body")], &[]).expect("upsert");
    db.store
        .with_conn(|conn| {
            conn.execute_batch(
                "DROP TABLE IF EXISTS fragment_lemmas;
                 DROP TABLE IF EXISTS lemma_terms;
                 CREATE TABLE fragment_lemmas (
                     fragment_id TEXT NOT NULL,
                     lemma TEXT NOT NULL,
                     confidence REAL NOT NULL DEFAULT 1.0,
                     PRIMARY KEY (fragment_id, lemma)
                 ) WITHOUT ROWID;
                 CREATE INDEX idx_lemmas_lemma ON fragment_lemmas(lemma);
                 INSERT INTO fragment_lemmas(fragment_id, lemma, confidence)
                 VALUES ('entity-a', 'alpha', 1.0), ('entity-a', 'body', 0.5);",
            )
            .expect("downgrade");
            Ok(())
        })
        .expect("with_conn");
    drop(db);
    let db = GuidanceDb::open(&path).expect("reopen");
    let ids = db
        .fragments_for_lemmas(&["alpha".to_string()], 10)
        .expect("lookup");
    assert_eq!(ids, vec!["entity-a".to_string()]);
    let terms: Vec<String> = db
        .store
        .query_rows("SELECT lemma FROM lemma_terms ORDER BY lemma", &[], |row| {
            row.get(0)
        })
        .expect("terms");
    assert_eq!(terms, vec!["alpha".to_string(), "body".to_string()]);
    let distinct: i64 = db
        .store
        .query_row(
            "SELECT COUNT(DISTINCT lemma_id) FROM fragment_lemmas",
            &[],
            |row| row.get(0),
        )
        .expect("distinct")
        .unwrap_or(0);
    assert_eq!(distinct, 2);
}

#[test]
fn bulk_freshness_matches_single_probes() {
    // One scan must agree with per-file probes on size, mtime, and the
    // embedded flag — including unknown files, which stay absent.
    let db = GuidanceDb::open_in_memory().expect("db");
    seed_alpha(&db);
    let mut with_vec = frag("entity-vec", None, "VecRow", "vector row");
    with_vec.embedding = Some(vec![0.5, -0.5]);
    db.upsert_fragments(&file_a(), &[with_vec], &[])
        .expect("upsert");
    let map = db.all_fragment_freshness().expect("bulk");
    for file in db.list_files().expect("files") {
        let key = |fresh: &super::super::db::FragmentFreshness| {
            (fresh.size_bytes, fresh.last_modified_time, fresh.embedded)
        };
        let single = db
            .fragment_freshness(&file.id)
            .expect("single")
            .map(|f| key(&f));
        let bulk = map.get(&file.id).map(key);
        assert_eq!(single, bulk, "bulk must match single probe for {}", file.id);
    }
    assert!(map.get("file-a").expect("row").embedded);
    assert!(map.get("never-ingested").is_none());
}
