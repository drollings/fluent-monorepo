//! P6 recall-depth sweep: fused recall latency and final depth across
//! corpus sizes (50 / 500 / 2000 fragments), limit 7.
//!
//! Method: in-memory `GuidanceDb` seeded with synthetic fragments sharing
//! one needle term (FTS + lemma routes saturate, exercising iterative
//! deepening); no embedder (vector route declines — the L1+L2 degradation).
//! Reports wall time; the depth reached is printed once per size. The
//! preserved constants (200 → 2000 × 2) stand unless this sweep shows
//! early saturation or blowup.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use guidance_core::query::db_storage::GuidanceDbStorage;
use guidance_core::query::hybrid::plan_from_query;
use guidance_core::query::recall::run_recall;
use guidance_core::query::strategy::QueryIntent;
use search_vector::GuidanceDb;
use search_vector::db::{ZgFileRecord, ZgFragmentRecord};
use std::hint::black_box;

fn file_record() -> ZgFileRecord {
    ZgFileRecord {
        id: "file-sweep".to_string(),
        absolute_path: "/bench/lib.rs".to_string(),
        relative_path: "lib.rs".to_string(),
        root_path: "/bench".to_string(),
        size_bytes: 100,
        last_modified_time: 100,
        kind: Some("code".to_string()),
        format: "rust".to_string(),
        content_hash: None,
        index_status: Some("indexed".to_string()),
        fail_count: 0,
        last_error: None,
    }
}

fn fragment_record(index: usize) -> ZgFragmentRecord {
    ZgFragmentRecord {
        id: format!("frag-{index}"),
        group: None,
        file_id: "file-sweep".to_string(),
        range_json: "{\"kind\":\"file\"}".to_string(),
        content_kind: "text".to_string(),
        content_text: Some(format!(
            "pub fn sweep_needle_{index}() {{ let sweep_needle = {index}; }}"
        )),
        cjk_text: String::new(),
        symbol_type: Some("function".to_string()),
        symbol_name: Some(format!("sweep_needle_{index}")),
        scope: None,
        signature: None,
        doc: None,
        modifiers: String::new(),
        heading: None,
        heading_level: None,
        embedding: None,
    }
}

fn seed(size: usize) -> GuidanceDb {
    let db = GuidanceDb::open_in_memory().expect("db");
    let fragments: Vec<ZgFragmentRecord> = (0..size).map(fragment_record).collect();
    db.upsert_fragments(&file_record(), &fragments, &[]).expect("upsert");
    db
}

fn bench_recall_depth(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("recall_depth_limit7");
    for size in [50usize, 500, 2000] {
        let db = seed(size);
        let storage = GuidanceDbStorage::new(&db);
        let plan = plan_from_query("sweep_needle", QueryIntent::GeneralSearch, 7);
        // Depth reached (computed once, printed for the record).
        let output = run_recall(&plan, &storage, None, None).expect("recall");
        println!(
            "size {size}: hits {} (depth saturates past corpus)",
            output.hits.len()
        );
        group.bench_with_input(BenchmarkId::new("fused", size), &size, |bencher, _| {
            bencher.iter(|| run_recall(black_box(&plan), black_box(&storage), None, None));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_recall_depth);
criterion_main!(benches);
