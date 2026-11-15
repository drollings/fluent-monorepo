//! Import-side tests for the capability corpus (chunker, decomposition,
//! manifest parsing, SVO normalization, corpus loading, and calibration).
//! Included via `#[path]` forwarder, per repo convention. Hermetic: rule
//! pipeline, hand-built fixtures, in-memory index, no endpoint and no
//! model call on any path.

use super::*;
use crate::read::doc_cards::{
    chunk_markdown_sections, decompose_chunk_source, manifest_description,
};

fn rule_pipeline() -> spacy_rs::pipeline::NlpPipeline {
    spacy_rs::pipeline::NlpPipeline::en_default().expect("rule pipeline builds")
}

const DOC: &str = "# Architecture\n\nGuides the router.\n\n## Dispatch\n\nSends requests onward.\n";

#[test]
fn chunker_splits_sections_with_anchors() {
    let (chunks, gaps) = chunk_markdown_sections("doc/guidance/index.md", DOC);
    assert!(gaps.is_empty(), "{gaps:?}");
    assert_eq!(chunks.len(), 2, "{chunks:?}");
    assert_eq!(chunks[0].anchor, "architecture");
    assert_eq!(chunks[1].anchor, "architecture-dispatch");
    assert!(chunks[0].text.contains("Guides the router."));
    assert!(chunks[1].text.contains("Sends requests onward."));
    assert_eq!(chunks[0].id, "doc/guidance/index.md#architecture");
}

#[test]
fn chunker_ignores_fenced_headings_and_pins_preamble() {
    let text = "Preamble prose.\n\n```md\n# Not a heading\n```\n\n# Real\n\nBody.\n";
    let (chunks, gaps) = chunk_markdown_sections("README.md", text);
    assert!(gaps.is_empty(), "{gaps:?}");
    assert_eq!(chunks.len(), 2, "{chunks:?}");
    assert_eq!(chunks[0].anchor, "preamble");
    assert_eq!(chunks[1].anchor, "real");
}

#[test]
fn chunker_splits_oversize_sections_and_gaps_empties() {
    let big = format!(
        "# Big\n\n{}\n\n## Empty\n\n   \n\n# Tail\n\nEnd.\n",
        "x".repeat(9000)
    );
    let (chunks, gaps) = chunk_markdown_sections("doc/big.md", &big);
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert_eq!(gaps[0].reason, "empty");
    let big_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.anchor.starts_with("big"))
        .collect();
    assert_eq!(big_chunks.len(), 2, "{chunks:?}");
    assert!(big_chunks.iter().all(|c| c.text.len() <= 8000));
    assert_eq!(chunks.last().expect("tail").anchor, "tail");
}

#[test]
fn chunker_dedups_duplicate_headings() {
    let text = "# Repeat\n\nOne.\n\n# Repeat\n\nTwo.\n";
    let (chunks, gaps) = chunk_markdown_sections("doc/dup.md", text);
    assert!(gaps.is_empty(), "{gaps:?}");
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].anchor, "repeat");
    assert_eq!(chunks[1].anchor, "repeat-2");
    assert_ne!(chunks[0].id, chunks[1].id);
}

#[test]
fn decomposition_table_pins_crate_and_domain() {
    // M12-cal golden: path → (crate, domain).
    let cases = vec![
        ("README.md", ("unknown", "meta")),
        ("AGENTS.md", ("unknown", "meta")),
        ("doc/router/dispatch.md", ("fluent-router", "router")),
        ("doc/guidance/index.md", ("guidance-core", "guidance")),
        ("doc/unknown-thing/x.md", ("unknown", "unknown-thing")),
        ("src/guidance/README.md", ("guidance-core", "guidance-core")),
        (
            "src/guidance/Cargo.toml",
            ("guidance-core", "guidance-core"),
        ),
        ("Cargo.toml", ("unknown", "meta")),
    ];
    for (path, (crate_name, domain)) in cases {
        assert_eq!(
            decompose_chunk_source(path),
            (crate_name.to_string(), domain.to_string()),
            "{path}"
        );
    }
}

#[test]
fn manifest_description_parses_package_section() {
    let manifest = "[package]\nname = \"guidance-core\"\ndescription = \"Local code index.\"\n";
    assert_eq!(
        manifest_description(manifest),
        Some("Local code index.".to_string())
    );
    assert_eq!(manifest_description("[package]\nname = \"x\"\n"), None);
    let escaped = "[package]\ndescription = \"Say \\\"hi\\\".\"\n";
    assert_eq!(
        manifest_description(escaped),
        Some("Say \"hi\".".to_string())
    );
}
#[test]
fn brief_svo_normalizes_predicate_and_fails_open() {
    let nlp = rule_pipeline();
    let (predicate, roles) = brief_svo("Indexes fragments for recall.", Some(&nlp));
    assert_eq!(predicate, "index");
    assert_eq!(roles, vec!["fragment".to_string()], "{roles:?}");
    assert_eq!(
        brief_svo("Indexes fragments.", None),
        (String::new(), Vec::new())
    );
}
fn seed_corpus_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("corpora/capability_seed.json")
}

#[test]
fn capability_corpus_loads_checked_in_goldens() {
    let corpus = load_capability_corpus(&seed_corpus_path()).expect("seed corpus loads");
    assert_eq!(corpus.briefs.len(), 6, "{corpus:?}");
    let cases = corpus.cases.as_ref().expect("seed corpus has cases");
    assert_eq!(cases.len(), 8, "{corpus:?}");
    assert_eq!(cases.iter().filter(|c| c.control).count(), 2);
    let guide = corpus
        .briefs
        .iter()
        .find(|b| b.id == "doc/router/guide.md#guide")
        .expect("guide brief");
    assert_eq!(guide.crate_name, "fluent-router");
    assert_eq!(guide.domain, "router");
}

#[test]
fn capability_corpus_refuses_bad_shapes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("missing.json");
    assert!(load_capability_corpus(&missing).is_err());
    let bad = dir.path().join("bad.json");
    std::fs::write(&bad, r#"{"briefs": []}"#).expect("write");
    assert!(load_capability_corpus(&bad).is_err());
    let no_cases = dir.path().join("no_cases.json");
    std::fs::write(
        &no_cases,
        r#"{"briefs": [{"id": "a", "purpose": "P.", "crate": "c", "domain": "d", "source_path": "s", "anchor": "a"}]}"#,
    )
    .expect("write");
    let corpus = load_capability_corpus(&no_cases).expect("missing cases is valid for import");
    assert!(corpus.cases.is_none(), "{corpus:?}");
    assert!(
        calibrate_capability_corpus(&no_cases, None).is_err(),
        "calibration requires cases"
    );
    let unknown = dir.path().join("unknown.json");
    std::fs::write(
        &unknown,
        r#"{"briefs": [{"id": "a", "purpose": "P.", "crate": "c", "domain": "d", "source_path": "s", "anchor": "a", "bogus": 1}]}"#,
    )
    .expect("write");
    assert!(
        load_capability_corpus(&unknown).is_err(),
        "unknown brief fields fail loudly"
    );
    let dup = dir.path().join("dup.json");
    std::fs::write(
        &dup,
        r#"{"briefs": [{"id": "a", "purpose": "P.", "crate": "c", "domain": "d", "source_path": "s", "anchor": "a"}, {"id": "a", "purpose": "Q.", "crate": "c", "domain": "d", "source_path": "s", "anchor": "b"}]}"#,
    )
    .expect("write");
    assert!(
        load_capability_corpus(&dup).is_err(),
        "duplicate ids fail loudly"
    );
}

#[test]
fn capability_calibration_dry_run_is_deterministic() {
    // M15-cal harness properties (no endpoint): support and control
    // counts, stable dataset hash, run-to-run determinism. The
    // measured values themselves are recorded in the milestone
    // report, not asserted here.
    let nlp = rule_pipeline();
    let first = calibrate_capability_corpus(&seed_corpus_path(), Some(&nlp)).expect("dry run");
    let second = calibrate_capability_corpus(&seed_corpus_path(), Some(&nlp)).expect("dry run");
    assert_eq!(first.support, 6, "{first:?}");
    assert_eq!(first.control_total, 2, "{first:?}");
    assert_eq!(first.dataset_hash.len(), 64, "{first:?}");
    assert_eq!(first, second, "calibration is deterministic");
    eprintln!(
        "capability calibration: recall@1={:.3} support={} control_fires={}/{}",
        first.recall_at_1, first.support, first.control_fires, first.control_total
    );
}

#[test]
fn capability_trigger_bars_hold_on_seed_corpus() {
    // M5-cal gate: branch-1 rank quality plus zero control fires over
    // the checked-in seed corpus. Bars lock here — on failure the
    // ranker or the corpus changes, never the bar.
    let nlp = rule_pipeline();
    let report = calibrate_capability_corpus(&seed_corpus_path(), Some(&nlp)).expect("dry run");
    assert!(
        report.passed,
        "recall@1={:.3} control_fires={}/{}",
        report.recall_at_1, report.control_fires, report.control_total
    );
    assert_eq!(report.control_fires, 0);
}
