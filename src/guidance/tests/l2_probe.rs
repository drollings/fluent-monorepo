//! P6 target (4) probe: inflected-query recall, fused (L1+L2) vs FTS-only.
//!
//! Method (mirrors zvec `benchmarks/`): a morphology-heavy micro-corpus is
//! ingested once with the hermetic rule-lemmatizer pipeline; each probe
//! query names an inflected form ABSENT from the corpus text. P@3 is
//! measured for the fused plan (FTS + lemma routes, no embedder) and for
//! an FTS-only plan — the latter plays the zvec-lexical role (zvec has no
//! lemma layer; its lexical behavior is token-exact like FTS MATCH).
//! Timings are printed (scoreboard style, never floors).

use guidance_core::query::db_storage::GuidanceDbStorage;
use guidance_core::query::hybrid::plan_from_query;
use guidance_core::query::ingest::{default_en_pipeline, ingest_text_file};
use guidance_core::query::recall::{RecallStorage, run_recall};
use guidance_core::query::strategy::QueryIntent;
use guidance_core::search_types::{FileInfo, SearchPlan, StorageFilter};
use search_vector::GuidanceDb;

const PROBES: &[(&str, &str, &str)] = &[
    // (query, relevant doc id, note)
    ("runners", "doc-run", "plural absent; text has runner/Running"),
    ("functions", "doc-fn", "plural absent; text has function"),
    ("child", "doc-child", "singular absent; text has children"),
    ("box", "doc-box", "singular absent; text has boxes"),
    ("needle", "doc-control", "control: literal present for both routes"),
];

const CORPUS: &[(&str, &str)] = &[
    ("doc-run", "the runner finished early\nRunning daily builds the runner\n"),
    ("doc-fn", "every function composes well\nfunction composition scales\n"),
    ("doc-child", "the children play outside\nyoung children learn fast\n"),
    ("doc-box", "the boxes sit there\nopen boxes carefully\n"),
    ("doc-control", "a needle hides here\nnothing else matters\n"),
];

fn file_info(id: &str) -> FileInfo {
    FileInfo {
        id: id.to_string(),
        absolute_path: format!("/probe/{id}.md"),
        relative_path: format!("{id}.md"),
        root_path: "/probe".to_string(),
        size_bytes: 64,
        last_modified_time: 0,
        content_hash: None,
        kind: None,
        format: "markdown".to_string(),
        index_status: None,
    }
}

fn seed() -> GuidanceDb {
    let db = GuidanceDb::open_in_memory().expect("db");
    let nlp = default_en_pipeline().expect("hermetic pipeline");
    for (id, text) in CORPUS {
        ingest_text_file(&db, &file_info(id), text, None, Some(&nlp)).expect("ingest");
    }
    db
}

fn fused_plan(query: &str) -> SearchPlan {
    plan_from_query(query, QueryIntent::GeneralSearch, 3)
}

/// Pure lexical baseline (the zvec-lexical role): storage FTS directly,
/// bypassing recall's always-on lemma route. zvec has no lemma layer; its
/// lexical behavior is token-exact like FTS MATCH.
fn lexical_only_hits(db: &GuidanceDb, query: &str, relevant: &str) -> f64 {
    let storage = GuidanceDbStorage::new(db);
    let hits = storage
        .search_fts(query, 3, &StorageFilter::default())
        .unwrap_or_default();
    if hits.iter().take(3).any(|hit| hit.file.id == relevant) {
        1.0
    } else {
        0.0
    }
}

/// P@3: 1.0 when the relevant doc is in the top 3, else 0.0.
fn precision_at_3(db: &GuidanceDb, plan: &SearchPlan, relevant: &str) -> f64 {
    let storage = GuidanceDbStorage::new(db);
    let nlp = default_en_pipeline().expect("hermetic pipeline");
    let output = run_recall(plan, &storage, None, Some(&nlp)).expect("recall");
    if output.hits.iter().take(3).any(|hit| hit.file.id == relevant) {
        1.0
    } else {
        0.0
    }
}

#[test]
fn l2_inflected_recall_beats_lexical_only() {
    let db = seed();
    let mut fused_sum = 0.0;
    let mut fts_sum = 0.0;
    println!("query\trelevant\tfused-P@3\tfts-P@3\tnote");
    for (query, relevant, note) in PROBES {
        let start = std::time::Instant::now();
        let fused = precision_at_3(&db, &fused_plan(query), relevant);
        let fused_ms = start.elapsed().as_secs_f64() * 1000.0;
        let fts = lexical_only_hits(&db, query, relevant);
        println!("{query}\t{relevant}\t{fused}\t{fts}\t{note} ({fused_ms:.1}ms)");
        fused_sum += fused;
        fts_sum += fts;
    }
    let fused_mean = fused_sum / PROBES.len() as f64;
    let fts_mean = fts_sum / PROBES.len() as f64;
    println!("mean fused-P@3 = {fused_mean:.3}, mean fts-P@3 = {fts_mean:.3}");
    assert!(fused_mean > fts_mean, "L2 must beat lexical-only on inflections");
    assert_eq!(fused_mean, 1.0, "every probe (incl. control) must hit top-3");
}
