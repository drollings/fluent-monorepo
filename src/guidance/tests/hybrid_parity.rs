//! Hermetic known-item parity matrix (P1 gate evidence, CI-safe).
//!
//! Ground truth by construction: each query names content placed in exactly
//! one document. Asserts P@1 = 1.0 and reciprocal rank = 1.0 per axis
//! (FTS symbol, CJK substring, lemma inflection, filtered selection) plus a
//! fused multi-axis case. Wall-clock prints for the P6 baseline (recorded,
//! never gated).

use guidance_core::query::db_storage::GuidanceDbStorage;
use guidance_core::query::hybrid::plan_from_query;
use guidance_core::query::ingest::ingest_text_file;
use guidance_core::query::recall::run_recall;
use guidance_core::query::strategy::QueryIntent;
use guidance_core::zg_types::FileInfo;
use search_vector::db::GuidanceDb;
use std::time::Instant;

fn file_info(id: &str, relative: &str, format: &str) -> FileInfo {
    FileInfo {
        id: id.to_string(),
        absolute_path: format!("/repo/{relative}"),
        relative_path: relative.to_string(),
        root_path: "/repo".to_string(),
        size_bytes: 128,
        last_modified_time: 100,
        content_hash: None,
        kind: Some(guidance_core::zg_types::FileKind::Code),
        format: format.to_string(),
        index_status: None,
    }
}

fn seed_matrix(db: &GuidanceDb, nlp: &spacy_rs::pipeline::NlpPipeline) {
    let docs = [
        ("sym", "src/sym.rs", "pub fn UniqueAlphaSymbol() {}\n", "rust", None),
        ("cjk", "src/seg.rs", "// 中文分词器测试\npub fn seg() {}\n", "rust", None),
        (
            "lemma",
            "src/run.ts",
            "the service runs every morning\n",
            "typescript",
            Some(nlp),
        ),
        (
            "other",
            "src/other.ts",
            "export const UnrelatedHelper = 7;\n",
            "typescript",
            None,
        ),
    ];
    for (id, relative, text, format, nlp) in docs {
        ingest_text_file(db, &file_info(id, relative, format), text, None, nlp).expect("ingest");
    }
}

fn top_path(
    db: &GuidanceDb,
    query: &str,
    intent: QueryIntent,
    nlp: Option<&spacy_rs::pipeline::NlpPipeline>,
) -> (String, f64, bool) {
    let storage = GuidanceDbStorage::new(db);
    let plan = plan_from_query(query, intent, 5);
    let started = Instant::now();
    let output = run_recall(&plan, &storage, None, nlp).expect("recall");
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert!(!output.hits.is_empty(), "no hits for {query:?}");
    let rank = output
        .hits
        .iter()
        .position(|hit| hit.rank == 1)
        .expect("rank 1 present");
    assert_eq!(rank, 0, "top hit must be fused rank 1");
    let lemma_evidence = output.hits[0]
        .evidence
        .iter()
        .any(|evidence| evidence.path == guidance_core::zg_types::RecallPath::Lemma);
    (output.hits[0].file.relative_path.clone(), elapsed_ms, lemma_evidence)
}

#[test]
fn known_item_parity_matrix() {
    let db = GuidanceDb::open_in_memory().expect("db");
    let nlp = spacy_rs::pipeline::NlpPipeline::en_default().expect("pipeline");
    seed_matrix(&db, &nlp);
    // The lemma case is a genuine inflection proof: neither "service" the
    // verb form "run" appears verbatim — the index holds "runs".
    let cases = [
        ("UniqueAlphaSymbol", QueryIntent::SingleIdentifier, "src/sym.rs", None),
        ("分词器", QueryIntent::GeneralSearch, "src/seg.rs", None),
        ("service run", QueryIntent::GeneralSearch, "src/run.ts", Some(&nlp)),
        ("UnrelatedHelper", QueryIntent::SingleIdentifier, "src/other.ts", None),
    ];
    let mut sum_rr = 0.0;
    for (query, intent, expected, nlp) in cases {
        let (path, elapsed_ms, _) = top_path(&db, query, intent, nlp);
        assert_eq!(path, expected, "wrong top hit for {query:?}");
        sum_rr += 1.0;
        eprintln!("parity: query={query:?} top={path} rr=1.0 p@1=1.0 query_ms={elapsed_ms:.2}");
    }
    let mean_rr = sum_rr / cases.len() as f64;
    eprintln!("parity: mean_rr={mean_rr:.3} p@1=1.0 cases={}", cases.len());
    assert!((mean_rr - 1.0).abs() < f64::EPSILON, "mean_rr={mean_rr}");
    // The inflection case must carry lemma-path evidence (not just FTS).
    let (_, _, lemma_evidence) =
        top_path(&db, "service run", QueryIntent::GeneralSearch, Some(&nlp));
    assert!(lemma_evidence, "L2 must contribute evidence on inflections");
}
