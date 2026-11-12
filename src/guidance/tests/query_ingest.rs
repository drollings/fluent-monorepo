//! Lazy lemma-pipeline boundary tests (included via `#[path]` forwarder
//! from `src/query/ingest.rs`, per repo convention). The counting
//! factory stands in for the stock constructor: production call sites
//! pass need, tests count builds.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn counting_factory(
    builds: &AtomicUsize,
) -> impl FnOnce() -> Option<spacy_rs::pipeline::NlpPipeline> + '_ {
    || {
        builds.fetch_add(1, Ordering::SeqCst);
        None
    }
}

#[test]
fn lemma_need_constructs_exactly_once() {
    // The recall side asks on first lemma need and reuses after:
    // repeated need still builds once.
    let lazy = LazyNlp::new();
    let builds = AtomicUsize::new(0);
    let first = lazy.get_with(counting_factory(&builds));
    let second = lazy.get_with(counting_factory(&builds));
    assert_eq!(builds.load(Ordering::SeqCst), 1, "construct-once");
    assert_eq!(first.is_some(), second.is_some());
}

#[test]
fn failed_construction_is_cached_not_rebuilt() {
    // Absence is an answer too: a `None` build still counts once, so
    // the failure path can never turn into a rebuild storm.
    let lazy = LazyNlp::new();
    let builds = AtomicUsize::new(0);
    assert!(lazy.get_with(counting_factory(&builds)).is_none());
    assert!(lazy.get_with(counting_factory(&builds)).is_none());
    assert_eq!(builds.load(Ordering::SeqCst), 1);
}
#[test]
fn untouched_holder_builds_nothing() {
    // The holder is inert until asked: paths that never ask (rg,
    // fully-skipped syncs) can never pay construction through it.
    let lazy = LazyNlp::new();
    let builds = AtomicUsize::new(0);
    let factory = counting_factory(&builds);
    drop(factory);
    drop(lazy);
    assert_eq!(builds.load(Ordering::SeqCst), 0);
}

// --- M14.1: no-NLP fail-open characterization -------------------------------
// `query_lemmas(None, …)` is the pipeline-absent path: deterministic
// lowercase alphanumeric tokens, order-preserving dedup. Old rows carry
// exactly these tokens, so this output is the compatibility anchor every
// query must share.

#[test]
fn query_lemmas_without_pipeline_is_deterministic_fallback() {
    assert_eq!(
        query_lemmas(None, "Hello World hello!"),
        vec!["hello".to_string(), "world".to_string()]
    );
    assert_eq!(
        query_lemmas(None, "export function AlphaSymbol() { return 1; }"),
        vec![
            "export".to_string(),
            "function".to_string(),
            "alphasymbol".to_string(),
        ]
    );
}

#[test]
fn query_lemmas_drops_punctuation_whitespace_and_stop_words() {
    // FTS already owns the raw lexical row; the lemma index pays only for
    // navigable terms, so punctuation, whitespace, and function words drop.
    assert_eq!(
        query_lemmas(None, "The fragment-index is BUILT: [hello] world!\n"),
        vec![
            "fragment".to_string(),
            "index".to_string(),
            "built".to_string(),
            "hello".to_string(),
            "world".to_string(),
        ]
    );
}

#[test]
fn query_lemmas_keeps_short_code_tokens() {
    // Two-character identifiers are real navigation anchors (`id`, `db`,
    // `fs`); the floor drops only the single-character punctuation scar.
    assert_eq!(
        query_lemmas(None, "id db fs io"),
        vec![
            "id".to_string(),
            "db".to_string(),
            "fs".to_string(),
            "io".to_string()
        ]
    );
}

#[test]
fn query_lemmas_filters_structural_toml_tokens() {
    // Cargo.toml punctuation was the worst offender: `[`, `]`, `=`, and
    // newlines used to become lemma rows.
    let lemmas = query_lemmas(None, "[package]\nname = \"guidance\"\n[dependencies]\n");
    assert!(!lemmas.iter().any(|lemma| lemma.len() <= 1), "{lemmas:?}");
    assert!(
        !lemmas
            .iter()
            .any(|lemma| !lemma.chars().all(char::is_alphanumeric)),
        "{lemmas:?}"
    );
    assert!(lemmas.contains(&"package".to_string()), "{lemmas:?}");
    assert!(lemmas.contains(&"dependencies".to_string()), "{lemmas:?}");
}

#[test]
fn query_lemmas_without_pipeline_empty_in_empty_out() {
    assert!(query_lemmas(None, "").is_empty());
    assert!(query_lemmas(None, "!!!").is_empty());
}

fn comment_block(text: &str) -> crate::ast_parser::CommentBlock {
    crate::ast_parser::CommentBlock {
        node_key: "test".to_string(),
        node_kind: "function_item".to_string(),
        name: "test".to_string(),
        scope: String::new(),
        start_byte: 0,
        end_byte: text.len(),
        start_line: 1,
        end_line: 1,
        text: text.to_string(),
        comment_hash: common_core::hash::blake3_hex(text.as_bytes()),
    }
}

#[test]
fn comment_block_lemmas_come_only_from_comment_text() {
    let blocks = [comment_block("Adds one to the total")];
    let lemmas = lemmas_for_comment_blocks(&blocks, None);
    assert!(lemmas.contains(&"adds".to_string()), "{lemmas:?}");
    assert!(lemmas.contains(&"total".to_string()), "{lemmas:?}");
    assert!(!lemmas.contains(&"value".to_string()), "{lemmas:?}");
    assert!(!lemmas.contains(&"fn".to_string()), "{lemmas:?}");
}

#[test]
fn comment_block_lemmas_dedupe_across_blocks() {
    let blocks = [
        comment_block("Retry the operation"),
        comment_block("Retry later"),
    ];
    let lemmas = lemmas_for_comment_blocks(&blocks, None);
    assert_eq!(
        lemmas.iter().filter(|lemma| *lemma == "retry").count(),
        1,
        "{lemmas:?}"
    );
    assert!(lemmas.contains(&"operation".to_string()), "{lemmas:?}");
    assert!(lemmas.contains(&"later".to_string()), "{lemmas:?}");
}

#[test]
fn comment_block_lemmas_empty_in_empty_out() {
    assert!(lemmas_for_comment_blocks(&[], None).is_empty());
    assert!(lemmas_for_comment_blocks(&[comment_block("")], None).is_empty());
    assert!(lemmas_for_comment_blocks(&[comment_block("!!!")], None).is_empty());
}

#[test]
fn lemma_policy_control_set_precision_recall() {
    // Labelled controls for the token filter: short identifiers and prose
    // terms must survive; punctuation, whitespace, and syntax keywords
    // must not. Both halves must measure 1.0 or the policy is retuned
    // test-first before wider reliance on comment attribution.
    let must_keep = [
        "id",
        "db",
        "io",
        "adds",
        "total",
        "retry",
        "operation",
        "runner",
        "runs",
        "composes",
        "children",
        "分词",
    ];
    let must_drop = [
        "[", "]", "=", ";", "(", ")", ".", ",", ":", " ", "\n", "the", "pub", "fn", "struct",
        "return", "self", "use", "a",
    ];
    let mut kept_keep = 0;
    for token in must_keep {
        let kept = !query_lemmas(None, token).is_empty();
        assert!(kept, "policy dropped a control term: {token:?}");
        kept_keep += usize::from(kept);
    }
    let mut kept_drop = 0;
    for token in must_drop {
        let kept = !query_lemmas(None, token).is_empty();
        assert!(!kept, "policy kept noise: {token:?}");
        kept_drop += usize::from(kept);
    }
    let recall = kept_keep as f64 / must_keep.len() as f64;
    let precision = kept_keep as f64 / (kept_keep + kept_drop) as f64;
    eprintln!("lemma policy controls: precision={precision:.3} recall={recall:.3}");
    assert!((recall - 1.0).abs() < f64::EPSILON, "recall={recall}");
    assert!(
        (precision - 1.0).abs() < f64::EPSILON,
        "precision={precision}"
    );
}

#[test]
fn lemma_policy_inflections_collapse_with_pipeline() {
    // The rule lemmatizer folds inflections so comment prose matches
    // inflected queries; the fallback keeps the surface form.
    let nlp = spacy_rs::pipeline::NlpPipeline::en_default().expect("pipeline");
    assert!(
        query_lemmas(Some(&nlp), "the service runs").contains(&"run".to_string()),
        "pipeline must collapse runs to run"
    );
    assert!(
        query_lemmas(None, "the service runs").contains(&"runs".to_string()),
        "fallback must keep the surface form"
    );
}
