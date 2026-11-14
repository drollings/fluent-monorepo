//! Lemma-route ablation harness tests: corpus loading, determinism,
//! exact-match controls, and the inflection-group measurement.
//! Included via `#[path]` forwarder, per repo convention. Hermetic:
//! rule pipeline, throwaway in-memory index, no model, no endpoint.

use super::*;

fn rule_pipeline() -> spacy_rs::pipeline::NlpPipeline {
    spacy_rs::pipeline::NlpPipeline::en_default().expect("rule pipeline builds")
}

fn seed_corpus_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("corpora/lemma_ablation_seed.json")
}

#[test]
fn ablation_corpus_loads_checked_in_goldens() {
    let corpus = load_lemma_ablation_corpus(&seed_corpus_path()).expect("seed corpus loads");
    assert_eq!(corpus.fragments.len(), 12, "{corpus:?}");
    assert_eq!(corpus.cases.len(), 6, "{corpus:?}");
    assert_eq!(corpus.cases.iter().filter(|c| c.control).count(), 3);
    assert_eq!(corpus.cases.iter().filter(|c| !c.control).count(), 3);
}

#[test]
fn ablation_corpus_refuses_bad_shapes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("missing.json");
    assert!(load_lemma_ablation_corpus(&missing).is_err());
    let bad = dir.path().join("bad.json");
    std::fs::write(&bad, r#"{"fragments": []}"#).expect("write");
    assert!(load_lemma_ablation_corpus(&bad).is_err());
}

#[test]
fn ablation_measurement_is_deterministic() {
    let nlp = rule_pipeline();
    let corpus = load_lemma_ablation_corpus(&seed_corpus_path()).expect("corpus");
    let first = measure_lemma_route(&corpus, Some(&nlp), 5).expect("measure");
    let second = measure_lemma_route(&corpus, Some(&nlp), 5).expect("measure");
    assert_eq!(first, second, "ablation is deterministic");
    assert_eq!(first.support, 3, "{first:?}");
    assert_eq!(first.control_total, 3, "{first:?}");
    eprintln!(
        "lemma ablation: on={:.3} off={:.3} delta={:+.3} support={} control_moved={}/{}",
        first.on_recall_at_k,
        first.off_recall_at_k,
        first.delta_recall_at_k,
        first.support,
        first.control_moved,
        first.control_total
    );
    for case in &first.cases {
        eprintln!(
            "  query {:?}: on {}/{} off {}/{} control={}",
            case.query, case.on_hits, case.support, case.off_hits, case.support, case.control
        );
    }
}

#[test]
fn ablation_exact_match_controls_do_not_move() {
    // Control group: verbatim queries keep identical recall in both
    // arms — the route must not disturb what lexical match owns.
    let nlp = rule_pipeline();
    let corpus = load_lemma_ablation_corpus(&seed_corpus_path()).expect("corpus");
    let report = measure_lemma_route(&corpus, Some(&nlp), 5).expect("measure");
    for case in report.cases.iter().filter(|c| {
        corpus
            .cases
            .iter()
            .any(|gold| gold.query == c.query && gold.control && !gold.relevant.is_empty())
    }) {
        assert!(
            !case.on_topk.is_empty(),
            "control must fire: {}",
            case.query
        );
        assert_eq!(
            case.on_hits, case.support,
            "verbatim control fully recalled ON: {}",
            case.query
        );
        assert_eq!(
            case.off_hits, case.support,
            "verbatim control fully recalled OFF: {}",
            case.query
        );
        assert_eq!(
            case.on_hits, case.off_hits,
            "verbatim control recall identical: {}",
            case.query
        );
    }
}

#[test]
fn ablation_no_fire_control_stays_empty() {
    let nlp = rule_pipeline();
    let corpus = load_lemma_ablation_corpus(&seed_corpus_path()).expect("corpus");
    let report = measure_lemma_route(&corpus, Some(&nlp), 5).expect("measure");
    let empty = report
        .cases
        .iter()
        .find(|c| c.support == 0)
        .expect("no-fire control present");
    assert!(empty.on_topk.is_empty(), "{empty:?}");
    assert!(empty.off_topk.is_empty(), "{empty:?}");
}
