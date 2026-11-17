//! Semantic-unit embed backfill: selector matrix, status rows,
//! unchanged skip, no embedder, and partial failure. Included via
//! `#[path]` forwarder from `index_pipeline.rs`, per repo convention.
//! Hermetic: in-memory index, deterministic stub embedder, no endpoint.

use super::*;
use crate::query::search_db::{FileRecord, FragmentRecord, FragmentRow, GuidanceSearchDb};
use fluent_llm::embeddings::{BatchEmbedding, EmbeddingError, EmbeddingProvider};

struct StubEmbedder {
    dims: u32,
    fail_on: std::sync::Mutex<Vec<String>>,
    unavailable_on: std::sync::Mutex<Vec<String>>,
    calls: std::sync::Mutex<usize>,
}

impl StubEmbedder {
    fn new() -> Self {
        Self {
            dims: 4,
            fail_on: std::sync::Mutex::new(Vec::new()),
            unavailable_on: std::sync::Mutex::new(Vec::new()),
            calls: std::sync::Mutex::new(0),
        }
    }

    fn calls(&self) -> usize {
        *common_core::sync::lock(&self.calls)
    }
}

impl EmbeddingProvider for StubEmbedder {
    fn name(&self) -> &'static str {
        "stub"
    }

    fn dimensions(&self) -> u32 {
        self.dims
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        Ok(vec![text.len() as f32; self.dims as usize])
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<BatchEmbedding, EmbeddingError> {
        *common_core::sync::lock(&self.calls) += 1;
        let fail_on = common_core::sync::lock(&self.fail_on);
        let unavailable_on = common_core::sync::lock(&self.unavailable_on);
        for text in texts {
            if unavailable_on.iter().any(|marker| text.contains(marker)) {
                return Err(EmbeddingError::RequestFailed("transport down".to_string()));
            }
            if fail_on.iter().any(|marker| text.contains(marker)) {
                return Err(EmbeddingError::ParseError("poison input".to_string()));
            }
        }
        let flat: Vec<f32> = texts
            .iter()
            .flat_map(|text| vec![text.len() as f32; self.dims as usize])
            .collect();
        Ok(BatchEmbedding {
            flat,
            count: texts.len(),
            dims: self.dims as usize,
        })
    }
}

fn file_a() -> FileRecord {
    FileRecord {
        id: "file-a".to_string(),
        absolute_path: "/repo/src/a.rs".to_string(),
        relative_path: "src/a.rs".to_string(),
        root_path: "/repo".to_string(),
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

fn frow(id: &str) -> FragmentRow {
    FragmentRow {
        id: id.to_string(),
        group: None,
        file_id: "file-a".to_string(),
        range_json: "{\"kind\":\"item\"}".to_string(),
        content_kind: "text".to_string(),
        content_text: Some("fn alpha() {}".to_string()),
        symbol_type: Some("function".to_string()),
        symbol_name: Some("alpha".to_string()),
        scope: None,
        signature: Some("fn alpha()".to_string()),
        doc: Some("Adds one.".to_string()),
        modifiers: String::new(),
        heading: None,
        heading_level: None,
    }
}

fn row(id: &str) -> FragmentRecord {
    FragmentRecord {
        id: id.to_string(),
        group: None,
        file_id: "file-a".to_string(),
        range_json: "{\"kind\":\"item\"}".to_string(),
        content_kind: "text".to_string(),
        content_text: Some("fn alpha() {}".to_string()),
        cjk_text: String::new(),
        symbol_type: Some("function".to_string()),
        symbol_name: Some("alpha".to_string()),
        scope: None,
        signature: Some("fn alpha()".to_string()),
        doc: Some("Adds one.".to_string()),
        modifiers: String::new(),
        heading: None,
        heading_level: None,
        embedding: None,
    }
}

fn seed(db: &GuidanceSearchDb, fragments: &[FragmentRecord]) {
    db.upsert_fragments(&file_a(), fragments, &[])
        .expect("seed");
    // upsert clears vectors by convention; backfill candidates must
    // actually lack them.
    assert_eq!(db.embedded_fragment_count().expect("count"), 0);
}

#[test]
fn selector_matrix_pins_units_and_skip_reasons() {
    let function = frow("fn");
    let mut value = frow("value");
    value.symbol_type = Some("value".to_string());
    let mut module = frow("module");
    module.symbol_type = Some("module".to_string());
    module.symbol_name = Some("freshness".to_string());
    let mut whole = frow("whole");
    whole.symbol_type = None;
    whole.symbol_name = None;
    let mut markdown = frow("md");
    markdown.symbol_type = None;
    markdown.symbol_name = None;
    markdown.heading = Some("Guide".to_string());
    markdown.content_text = Some("Body text.".to_string());
    let mut image = frow("img");
    image.content_kind = "image".to_string();
    let mut eval = frow("eval");
    eval.file_id = "/repo/doc/guidance/eval/questions.jsonl".to_string();
    let mut mirror = frow("mirror");
    mirror.file_id = "/repo/.guidance/src/a.json".to_string();
    let mut empty = frow("empty");
    empty.symbol_type = None;
    empty.symbol_name = None;
    empty.heading = Some(String::new());
    empty.content_text = None;
    empty.doc = None;
    for (kind, fragment) in [
        ("module", "m"),
        ("class", "c"),
        ("interface", "i"),
        ("function", "f"),
        ("alias", "a"),
    ] {
        let mut unit = frow(kind);
        unit.symbol_type = Some(kind.to_string());
        unit.symbol_name = Some(kind.to_string());
        assert!(
            classify_embed_fragment(&unit).is_ok(),
            "{kind} is a unit: {unit:?}"
        );
        let _ = (fragment, unit);
    }
    assert!(classify_embed_fragment(&function).is_ok());
    assert!(
        classify_embed_fragment(&module).is_ok(),
        "module headers are units: {module:?}"
    );
    assert!(classify_embed_fragment(&markdown).is_ok());
    assert_eq!(
        classify_embed_fragment(&value)
            .expect_err("value skips")
            .reason,
        "non-unit-symbol"
    );
    assert_eq!(
        classify_embed_fragment(&whole)
            .expect_err("whole-file skips")
            .reason,
        "whole-file"
    );
    assert_eq!(
        classify_embed_fragment(&image)
            .expect_err("image skips")
            .reason,
        "image"
    );
    assert_eq!(
        classify_embed_fragment(&eval)
            .expect_err("eval skips")
            .reason,
        "eval-fixture"
    );
    assert_eq!(
        classify_embed_fragment(&mirror)
            .expect_err("mirror skips")
            .reason,
        "derived-mirror"
    );
    assert_eq!(
        classify_embed_fragment(&empty)
            .expect_err("empty skips")
            .reason,
        "empty-input"
    );
}

#[test]
fn unit_input_carries_symbol_and_doc_shape() {
    let unit = classify_embed_fragment(&frow("fn")).expect("unit");
    assert!(
        unit.input.contains("symbol: function alpha"),
        "{}",
        unit.input
    );
    assert!(unit.input.contains("doc: Adds one."), "{}", unit.input);
    assert!(unit.input.contains("fn alpha() {}"), "{}", unit.input);
    assert_eq!(unit.input_hash.len(), 64);
}

#[test]
fn backfill_embeds_units_and_records_skips_with_reasons() {
    let db = GuidanceSearchDb::open_in_memory().expect("db");
    let mut whole = row("whole");
    whole.symbol_type = None;
    whole.symbol_name = None;
    seed(&db, &[row("fn"), whole]);
    let provider = StubEmbedder::new();
    let report = backfill_fragment_embeddings(&db, Some(&provider), &IndexOptions::default())
        .expect("backfill");
    assert_eq!(report.candidates, 2, "{report:?}");
    assert_eq!(report.embedded, 1, "{report:?}");
    assert_eq!(report.skipped_filtered, 1, "{report:?}");
    assert_eq!(db.embedded_fragment_count().expect("count"), 1);
    let statuses = db
        .embedding_status_for_ids(&["fn".to_string(), "whole".to_string()])
        .expect("status");
    assert_eq!(statuses.len(), 2, "{statuses:?}");
    let ready = statuses
        .iter()
        .find(|row| row.fragment_id == "fn")
        .expect("fn status");
    assert_eq!(ready.status, "ready");
    assert_eq!(ready.embedder, "stub:4");
    assert!(ready.reason.is_empty());
    let skipped = statuses
        .iter()
        .find(|row| row.fragment_id == "whole")
        .expect("skip status");
    assert_eq!(skipped.status, "skipped");
    assert_eq!(skipped.reason, "whole-file");
}

#[test]
fn unchanged_inputs_skip_without_new_calls() {
    let db = GuidanceSearchDb::open_in_memory().expect("db");
    seed(&db, &[row("fn")]);
    let provider = StubEmbedder::new();
    let options = IndexOptions::default();
    let first = backfill_fragment_embeddings(&db, Some(&provider), &options).expect("first");
    assert_eq!(first.embedded, 1, "{first:?}");
    let calls = provider.calls();
    assert!(calls > 0);
    let second = backfill_fragment_embeddings(&db, Some(&provider), &options).expect("second");
    assert_eq!(second.candidates, 0, "{second:?}");
    assert_eq!(second.embedded, 0, "{second:?}");
    assert_eq!(provider.calls(), calls, "no new embed calls");
}

#[test]
fn changed_inputs_reembed_and_terminal_failures_wait() {
    let db = GuidanceSearchDb::open_in_memory().expect("db");
    seed(&db, &[row("fn")]);
    let provider = StubEmbedder::new();
    let options = IndexOptions::default();
    backfill_fragment_embeddings(&db, Some(&provider), &options).expect("first");
    // New bytes, same id: the status hash disagrees, so the row re-embeds.
    let mut edited = row("fn");
    edited.content_text = Some("fn alpha_edited() {}".to_string());
    db.upsert_fragments(&file_a(), &[edited], &[])
        .expect("edit");
    let calls = provider.calls();
    let report = backfill_fragment_embeddings(&db, Some(&provider), &options).expect("re-embed");
    assert_eq!(report.embedded, 1, "{report:?}");
    assert!(provider.calls() > calls);
    // Terminal failure with the same input and embedder waits: the next
    // run skips without calling the backend again.
    common_core::sync::lock(&provider.fail_on).push("alpha_edited".to_string());
    let mut edited_again = row("fn");
    edited_again.content_text = Some("fn alpha_edited_again() {}".to_string());
    db.upsert_fragments(&file_a(), &[edited_again], &[])
        .expect("edit");
    let report = backfill_fragment_embeddings(&db, Some(&provider), &options).expect("fail");
    assert_eq!(report.failed, 1, "{report:?}");
    let failed_status = db
        .embedding_status_for_ids(&["fn".to_string()])
        .expect("status");
    assert_eq!(failed_status[0].status, "failed");
    assert!(!failed_status[0].reason.is_empty());
    let calls = provider.calls();
    let report = backfill_fragment_embeddings(&db, Some(&provider), &options).expect("wait");
    assert_eq!(report.failed, 0, "{report:?}");
    assert_eq!(report.skipped_unchanged, 1, "{report:?}");
    assert_eq!(
        provider.calls(),
        calls,
        "terminal failure waits for new input"
    );
}

#[test]
fn transport_failures_retry_next_run() {
    let db = GuidanceSearchDb::open_in_memory().expect("db");
    seed(&db, &[row("fn")]);
    let provider = StubEmbedder::new();
    common_core::sync::lock(&provider.unavailable_on).push("alpha".to_string());
    let options = IndexOptions::default();
    let report = backfill_fragment_embeddings(&db, Some(&provider), &options).expect("backfill");
    assert_eq!(report.unavailable, 1, "{report:?}");
    assert_eq!(report.embedded, 0, "{report:?}");
    assert_eq!(db.embedded_fragment_count().expect("count"), 0);
    common_core::sync::lock(&provider.unavailable_on).clear();
    let report = backfill_fragment_embeddings(&db, Some(&provider), &options).expect("retry");
    assert_eq!(report.embedded, 1, "{report:?}");
    assert_eq!(report.unavailable, 0, "{report:?}");
}

#[test]
fn no_embedder_records_nothing() {
    let db = GuidanceSearchDb::open_in_memory().expect("db");
    seed(&db, &[row("fn")]);
    let report =
        backfill_fragment_embeddings(&db, None, &IndexOptions::default()).expect("no backend");
    assert!(report.backend_missing, "{report:?}");
    assert_eq!(report.embedded, 0, "{report:?}");
    assert_eq!(db.embedded_fragment_count().expect("count"), 0);
    assert!(db
        .embedding_status_for_ids(&["fn".to_string()])
        .expect("status")
        .is_empty());
}

/// M1.4 characterization: `embed_input_for_row` collapses
/// multiline signatures and docs to one line (C2, fourth `one_line`
/// call site — found after the initial inventory). Expected strings
/// were verified against the removed `one_line` before migration.
#[test]
fn embed_input_collapses_multiline_signature_and_doc() {
    let mut fragment = frow("sig");
    fragment.signature = Some("pub fn alpha(\n    x: u32,\n) -> u64".to_string());
    fragment.doc = Some("Adds\none.".to_string());
    let input = super::embed_input_for_row(&fragment).expect("embed input");
    assert!(
        input.contains("signature: pub fn alpha( x: u32, ) -> u64"),
        "{input:?}"
    );
    assert!(input.contains("doc: Adds one."), "{input:?}");
}

/// M5.1 characterization: wave retry contract (C11) — fail-fast
/// aborts at once, retryable errors retry to the attempt budget,
/// success short-circuits, exhaustion maps transport vs input.
/// Backoff delays use a 1ms base so the table stays fast while
/// asserting the attempt counts exactly.
#[test]
fn embed_batch_retry_attempt_counts() {
    struct ScriptedEmbedder {
        script: std::sync::Mutex<Vec<Result<BatchEmbedding, EmbeddingError>>>,
        calls: std::sync::Mutex<usize>,
    }
    impl EmbeddingProvider for ScriptedEmbedder {
        fn name(&self) -> &'static str {
            "scripted"
        }
        fn dimensions(&self) -> u32 {
            2
        }
        fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
            Ok(vec![text.len() as f32; 2])
        }
        fn embed_batch(&self, _texts: &[&str]) -> Result<BatchEmbedding, EmbeddingError> {
            *common_core::sync::lock(&self.calls) += 1;
            common_core::sync::lock(&self.script)
                .pop()
                .expect("script exhausted")
        }
    }
    fn ok_batch() -> BatchEmbedding {
        BatchEmbedding {
            flat: vec![0.0; 2],
            count: 1,
            dims: 2,
        }
    }
    fn run(
        script: Vec<Result<BatchEmbedding, EmbeddingError>>,
        max_attempts: u32,
    ) -> (Result<BatchEmbedding, super::EmbedFailure>, usize) {
        let embedder = ScriptedEmbedder {
            script: std::sync::Mutex::new(script.into_iter().rev().collect()),
            calls: std::sync::Mutex::new(0),
        };
        let options = super::IndexOptions {
            retry_max_attempts: max_attempts,
            retry_base_ms: 1,
            ..Default::default()
        };
        let out = super::embed_batch_with_retry(&embedder, &["a"], &options);
        let calls = *common_core::sync::lock(&embedder.calls);
        (out, calls)
    }
    let transport = || EmbeddingError::RequestFailed("down".to_string());
    let input_err = || EmbeddingError::ParseError("bad input".to_string());

    let (out, calls) = run(vec![Ok(ok_batch())], 3);
    assert!(out.is_ok());
    assert_eq!(calls, 1);

    let (out, calls) = run(vec![Err(EmbeddingError::InvalidApiUrl)], 3);
    assert!(matches!(out, Err(super::EmbedFailure::FailFast(_))));
    assert_eq!(calls, 1);

    let (out, calls) = run(vec![Err(transport()), Ok(ok_batch())], 3);
    assert!(out.is_ok());
    assert_eq!(calls, 2);

    let (out, calls) = run(
        vec![Err(transport()), Err(transport()), Err(transport())],
        3,
    );
    match out {
        Err(super::EmbedFailure::Retryable(f)) => assert!(f.unavailable, "{f:?}"),
        other => panic!("expected transport Retryable, got {other:?}"),
    }
    assert_eq!(calls, 3);

    let (out, calls) = run(
        vec![Err(input_err()), Err(input_err()), Err(input_err())],
        3,
    );
    match out {
        Err(super::EmbedFailure::Retryable(f)) => assert!(!f.unavailable, "{f:?}"),
        other => panic!("expected input Retryable, got {other:?}"),
    }
    assert_eq!(calls, 3);

    let (out, calls) = run(vec![Err(transport())], 1);
    assert!(matches!(out, Err(super::EmbedFailure::Retryable(_))));
    assert_eq!(calls, 1);
}
