//! Ingest-side tests for `query::capability` (chunker, decomposition,
//! manifest parsing, brief validation, cache keys, SVO normalization,
//! and the background pass). Included via `#[path]` forwarder, per
//! repo convention. Hermetic: rule pipeline, hand-built aliases are
//! not needed here, in-memory `ChatBackend` fakes, no endpoint.

use super::*;
use fluent_llm::client::ChatBackend;
use fluent_llm::protocol::LlmError;

struct FakeBackend {
    response: Result<String, LlmError>,
    calls: std::sync::Mutex<usize>,
}

impl FakeBackend {
    fn ready(text: &str) -> Self {
        Self {
            response: Ok(text.to_string()),
            calls: std::sync::Mutex::new(0),
        }
    }

    fn echo() -> Self {
        Self {
            response: Ok(String::new()),
            calls: std::sync::Mutex::new(0),
        }
    }

    fn echo_response(&self, messages: &[fluent_llm::protocol::ChatMessage]) -> String {
        match &self.response {
            Ok(canned) if !canned.is_empty() => canned.clone(),
            _ => {
                let body: Vec<&str> = messages.iter().map(|m| m.content.as_str()).collect();
                let mut map = std::collections::BTreeMap::new();
                for line in body.join("\n").lines() {
                    if let Some(id) = line.strip_prefix("## ") {
                        let id = id.trim().to_string();
                        map.insert(id.clone(), format!("Brief of {id}."));
                    }
                }
                serde_json::to_string(&map).expect("json")
            }
        }
    }

    fn failing(error: LlmError) -> Self {
        Self {
            response: Err(error),
            calls: std::sync::Mutex::new(0),
        }
    }

    fn call_count(&self) -> usize {
        *common_core::sync::lock(&self.calls)
    }
}

impl ChatBackend for FakeBackend {
    fn chat_complete(
        &self,
        _messages: &[fluent_llm::protocol::ChatMessage],
    ) -> Result<String, LlmError> {
        self.response.clone()
    }

    fn chat_complete_with_extras(
        &self,
        messages: &[fluent_llm::protocol::ChatMessage],
        _extras: &serde_json::Value,
    ) -> Result<String, LlmError> {
        *common_core::sync::lock(&self.calls) += 1;
        if let Err(error) = &self.response {
            return Err(error.clone());
        }
        Ok(self.echo_response(messages))
    }
}

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
    assert!(big_chunks
        .iter()
        .all(|c| c.text.len() <= MAX_CAPABILITY_INPUT_CHARS));
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
fn capability_globs_default_and_override() {
    let cfg = crate::config::ProjectConfig::default();
    assert_eq!(
        capability_globs(&cfg),
        DEFAULT_CAPABILITY_GLOBS
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    );
    let mut custom = crate::config::ProjectConfig::default();
    custom.capability_docs = vec!["custom/**/*.md".to_string()];
    assert_eq!(
        capability_globs(&custom),
        vec!["custom/**/*.md".to_string()]
    );
}

#[test]
fn cache_key_tracks_content_and_model() {
    let first = capability_cache_key("hash-a", "model-x");
    assert_eq!(first, capability_cache_key("hash-a", "model-x"));
    assert_ne!(first, capability_cache_key("hash-b", "model-x"));
    assert_ne!(first, capability_cache_key("hash-a", "model-y"));
}

#[test]
fn validation_accepts_think_wrapped_and_rejects_unknown_ids() {
    let chunks = vec![
        chunk_fixture("a#one", "First."),
        chunk_fixture("a#two", "Second."),
    ];
    let backend = FakeBackend::ready(
        "<think>briefing</think> {\"a#one\": \"One purpose.\", \"a#two\": \"Two purpose.\"}",
    );
    let outcomes = brief_chunks_with_backend(&backend, "model-x", &chunks);
    assert_eq!(outcomes.len(), 2);
    assert!(
        matches!(&outcomes[0].1, SummaryOutcome::Ready { text } if text == "One purpose."),
        "{outcomes:?}"
    );
    let rogue = FakeBackend::ready(r#"{"a#one": "One.", "rogue": "Rogue."}"#);
    let outcomes = brief_chunks_with_backend(&rogue, "model-x", &chunks);
    assert!(
        outcomes.iter().all(|(_, outcome)| matches!(
            outcome,
            SummaryOutcome::Failed { kind, .. } if kind == "malformed"
        )),
        "{outcomes:?}"
    );
}

#[test]
fn validation_fails_only_the_missing_chunk() {
    let chunks = vec![
        chunk_fixture("a#one", "First."),
        chunk_fixture("a#two", "Second."),
    ];
    let backend = FakeBackend::ready(r#"{"a#one": "One purpose."}"#);
    let outcomes = brief_chunks_with_backend(&backend, "model-x", &chunks);
    assert!(
        matches!(&outcomes[0].1, SummaryOutcome::Ready { .. }),
        "{outcomes:?}"
    );
    assert!(
        matches!(&outcomes[1].1, SummaryOutcome::Failed { kind, .. } if kind == "missing"),
        "{outcomes:?}"
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

fn chunk_fixture(id: &str, text: &str) -> CapabilityChunk {
    let (source, anchor) = id.rsplit_once('#').expect("id shape");
    CapabilityChunk {
        id: id.to_string(),
        source_path: source.to_string(),
        anchor: anchor.to_string(),
        text: text.to_string(),
        content_hash: common_core::hash::blake3_hex(text.as_bytes()),
        project: "monorepo".to_string(),
        crate_name: "guidance-core".to_string(),
        domain: "guidance".to_string(),
    }
}

fn pass_parts() -> (
    search_vector::db::GuidanceDb,
    crate::model_lanes::LaneGate,
    spacy_rs::pipeline::NlpPipeline,
) {
    (
        search_vector::db::GuidanceDb::open_in_memory().expect("db"),
        crate::model_lanes::LaneGate::with_defaults(),
        rule_pipeline(),
    )
}

#[tokio::test]
async fn pass_briefs_chunks_and_persists_rows() {
    let backend = FakeBackend::ready(r#"{"doc/a.md#overview": "Indexes fragments for recall."}"#);
    let (db, gate, nlp) = pass_parts();
    let mut pass = CapabilityPass::new(&backend, "test-model".to_string(), &gate, &db, Some(&nlp));
    let (chunks, gaps) = chunk_markdown_sections("doc/a.md", "# Overview\n\nBody text here.\n");
    let outcome = pass
        .brief_document("monorepo", "doc/a.md", &chunks, &gaps)
        .await
        .expect("pass");
    assert_eq!(outcome.briefed, 1, "{outcome:?}");
    assert_eq!(outcome.cached, 0, "{outcome:?}");
    assert_eq!(backend.call_count(), 1);
    let stored = db.capabilities_for_source("doc/a.md").expect("rows");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].purpose, "Indexes fragments for recall.");
    assert_eq!(stored[0].predicate, "index");
    assert_eq!(stored[0].status, "ready");
    let roles: Vec<String> = serde_json::from_str(&stored[0].roles_json).expect("roles json");
    assert_eq!(roles, vec!["fragment".to_string()], "{roles:?}");
    let hits = db.search_capabilities("fragments", 10).expect("search");
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].id, "doc/a.md#overview");
    let lemma_ids = db
        .capabilities_for_lemmas(&["fragment".to_string()], 10)
        .expect("lemmas");
    assert_eq!(lemma_ids, vec!["doc/a.md#overview".to_string()]);
}

#[tokio::test]
async fn pass_skips_cached_chunks_on_repass() {
    let backend = FakeBackend::echo();
    let (db, gate, nlp) = pass_parts();
    let mut pass = CapabilityPass::new(&backend, "test-model".to_string(), &gate, &db, Some(&nlp));
    let (chunks, gaps) = chunk_markdown_sections("doc/a.md", "# Overview\n\nBody text here.\n");
    let first = pass
        .brief_document("monorepo", "doc/a.md", &chunks, &gaps)
        .await
        .expect("pass");
    assert_eq!(first.briefed, 1);
    let second = pass
        .brief_document("monorepo", "doc/a.md", &chunks, &gaps)
        .await
        .expect("pass");
    assert_eq!(second.briefed, 0, "{second:?}");
    assert_eq!(second.cached, 1, "{second:?}");
    assert_eq!(backend.call_count(), 1, "repass must not call the model");
}

#[tokio::test]
async fn pass_reruns_only_edited_chunks() {
    let backend = FakeBackend::echo();
    let (db, gate, nlp) = pass_parts();
    let mut pass = CapabilityPass::new(&backend, "test-model".to_string(), &gate, &db, Some(&nlp));
    let (chunks, gaps) =
        chunk_markdown_sections("doc/a.md", "# One\n\nFirst.\n\n# Two\n\nSecond.\n");
    pass.brief_document("monorepo", "doc/a.md", &chunks, &gaps)
        .await
        .expect("pass");
    let (edited, edited_gaps) =
        chunk_markdown_sections("doc/a.md", "# One\n\nFirst edited.\n\n# Two\n\nSecond.\n");
    let outcome = pass
        .brief_document("monorepo", "doc/a.md", &edited, &edited_gaps)
        .await
        .expect("pass");
    assert_eq!(outcome.briefed, 1, "{outcome:?}");
    assert_eq!(outcome.cached, 1, "{outcome:?}");
    assert_eq!(backend.call_count(), 2);
}

#[tokio::test]
async fn pass_records_unavailable_as_held() {
    let backend = FakeBackend::failing(fluent_llm::protocol::LlmError::Http("boom".to_string()));
    let (db, gate, nlp) = pass_parts();
    let mut pass = CapabilityPass::new(&backend, "test-model".to_string(), &gate, &db, Some(&nlp));
    let (chunks, gaps) = chunk_markdown_sections("doc/a.md", "# Overview\n\nBody.\n");
    let outcome = pass
        .brief_document("monorepo", "doc/a.md", &chunks, &gaps)
        .await
        .expect("pass");
    assert_eq!(outcome.unavailable, 1, "{outcome:?}");
    let stored = db.capabilities_for_source("doc/a.md").expect("rows");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].status, "held");
    assert!(
        db.search_capabilities("overview", 10)
            .expect("search")
            .is_empty(),
        "held rows must stay out of recall"
    );
}

#[tokio::test]
async fn pass_empty_doc_records_gaps_and_persists_nothing() {
    // Empty-doc control: whitespace-only input yields gaps, a clean
    // exit, and no rows in any table.
    let backend = FakeBackend::ready("{}");
    let (db, gate, nlp) = pass_parts();
    let mut pass = CapabilityPass::new(&backend, "test-model".to_string(), &gate, &db, Some(&nlp));
    let (chunks, gaps) = chunk_markdown_sections("doc/empty.md", "   \n\n  \n");
    assert!(chunks.is_empty());
    let outcome = pass
        .brief_document("monorepo", "doc/empty.md", &chunks, &gaps)
        .await
        .expect("pass");
    assert_eq!(outcome.gaps.len(), gaps.len(), "{outcome:?}");
    assert!(!outcome.gaps.is_empty(), "{outcome:?}");
    assert_eq!(backend.call_count(), 0, "no chunks means no model call");
    assert!(db
        .capabilities_for_source("doc/empty.md")
        .expect("rows")
        .is_empty());
}

fn seed_corpus_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("corpora/capability_seed.json")
}

#[test]
fn capability_corpus_loads_checked_in_goldens() {
    let corpus = load_capability_corpus(&seed_corpus_path()).expect("seed corpus loads");
    assert_eq!(corpus.briefs.len(), 6, "{corpus:?}");
    assert_eq!(corpus.cases.len(), 8, "{corpus:?}");
    assert_eq!(corpus.cases.iter().filter(|c| c.control).count(), 2);
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
        r#"{"briefs": [{"id": "a", "purpose": "P.", "crate": "c", "domain": "d", "source_path": "s", "anchor": "a"}], "cases": []}"#,
    )
    .expect("write");
    assert!(load_capability_corpus(&no_cases).is_err());
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

fn map_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"src/guidance\", \"src/router\"]\n",
    )
    .expect("write root manifest");
    for (prefix, name, desc) in [
        (
            "src/guidance",
            "guidance-core",
            "Local code index for recall.",
        ),
        (
            "src/router",
            "fluent-router",
            "Routes requests onward through the table.",
        ),
    ] {
        let dir_path = root.join(prefix);
        std::fs::create_dir_all(&dir_path).expect("mkdir");
        std::fs::write(
            dir_path.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\ndescription = \"{desc}\"\n"),
        )
        .expect("write manifest");
    }
    dir
}

#[test]
fn workspace_map_chunks_carry_name_members_component_and_description() {
    let dir = map_fixture();
    let (chunks, gaps) = workspace_map_chunks(dir.path(), "fixture");
    assert!(gaps.is_empty(), "{gaps:?}");
    assert_eq!(chunks.len(), 2, "{chunks:?}");
    assert_eq!(chunks[0].source_path, WORKSPACE_MAP_SOURCE);
    assert_eq!(chunks[1].source_path, WORKSPACE_MAP_SOURCE);
    for chunk in &chunks {
        assert_eq!(chunk.project, "fixture");
        assert_eq!(chunk.domain, "map");
        assert!(!chunk.content_hash.is_empty());
    }
    let guidance = chunks
        .iter()
        .find(|c| c.id == format!("{WORKSPACE_MAP_SOURCE}#guidance-core"))
        .expect("guidance map chunk");
    assert_eq!(guidance.crate_name, "guidance-core");
    assert!(guidance.text.contains("guidance-core"), "{}", guidance.text);
    assert!(guidance.text.contains("guidance"), "{}", guidance.text);
    assert!(guidance.text.contains("src/guidance"), "{}", guidance.text);
    assert!(
        guidance.text.contains("Local code index for recall."),
        "{}",
        guidance.text
    );
    let router = chunks
        .iter()
        .find(|c| c.id == format!("{WORKSPACE_MAP_SOURCE}#fluent-router"))
        .expect("router map chunk");
    assert_eq!(router.crate_name, "fluent-router");
    assert!(router.text.contains("fluent-router"), "{}", router.text);
    assert!(router.text.contains("src/router"), "{}", router.text);
    assert!(
        router
            .text
            .contains("Routes requests onward through the table."),
        "{}",
        router.text
    );
    let again = workspace_map_chunks(dir.path(), "fixture").0;
    assert_eq!(chunks, again, "map derivation is deterministic");
}

#[test]
fn workspace_map_manifest_package_name_parses() {
    assert_eq!(
        manifest_package_name("[package]\nname = \"guidance-core\"\n"),
        Some("guidance-core".to_string())
    );
    assert_eq!(manifest_package_name("[workspace]\nmembers = []\n"), None);
}

#[test]
fn workspace_map_rederives_on_manifest_change_only() {
    let dir = map_fixture();
    let (before, _) = workspace_map_chunks(dir.path(), "fixture");
    std::fs::write(
        dir.path().join("src/router/Cargo.toml"),
        "[package]\nname = \"fluent-router\"\ndescription = \"Routes requests edited.\"\n",
    )
    .expect("edit manifest");
    let (after, _) = workspace_map_chunks(dir.path(), "fixture");
    assert_eq!(before.len(), after.len());
    for chunk in &before {
        let next = after.iter().find(|c| c.id == chunk.id).expect("stable id");
        if chunk.id.ends_with("#fluent-router") {
            assert_ne!(chunk.content_hash, next.content_hash);
            assert_ne!(chunk.text, next.text);
        } else {
            assert_eq!(chunk.content_hash, next.content_hash, "{}", chunk.id);
        }
    }
}

#[test]
fn workspace_map_unknown_crate_falls_back_without_dropping() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"src/mystery\"]\n",
    )
    .expect("write");
    std::fs::create_dir_all(root.join("src/mystery")).expect("mkdir");
    std::fs::write(
        root.join("src/mystery/Cargo.toml"),
        "[package]\nname = \"mystery\"\ndescription = \"Mystery crate.\"\n",
    )
    .expect("write");
    let (chunks, gaps) = workspace_map_chunks(root, "fixture");
    assert!(gaps.is_empty(), "{gaps:?}");
    assert_eq!(chunks.len(), 1, "{chunks:?}");
    assert_eq!(chunks[0].crate_name, "mystery");
    assert!(chunks[0].text.contains("unknown"), "{}", chunks[0].text);
}

#[tokio::test]
async fn workspace_map_pass_caches_and_rebriefs_edited_only() {
    let dir = map_fixture();
    let backend = FakeBackend::echo();
    let (db, gate, nlp) = pass_parts();
    let mut pass = CapabilityPass::new(&backend, "test-model".to_string(), &gate, &db, Some(&nlp));
    let (chunks, gaps) = workspace_map_chunks(dir.path(), "fixture");
    let first = pass
        .brief_document("fixture", WORKSPACE_MAP_SOURCE, &chunks, &gaps)
        .await
        .expect("pass");
    assert_eq!(first.briefed, 2, "{first:?}");
    let second = pass
        .brief_document("fixture", WORKSPACE_MAP_SOURCE, &chunks, &gaps)
        .await
        .expect("pass");
    assert_eq!(second.briefed, 0, "{second:?}");
    assert_eq!(second.cached, 2, "{second:?}");
    assert_eq!(backend.call_count(), 1, "repass must not call the model");
    std::fs::write(
        dir.path().join("src/router/Cargo.toml"),
        "[package]\nname = \"fluent-router\"\ndescription = \"Routes requests edited.\"\n",
    )
    .expect("edit manifest");
    let (edited, edited_gaps) = workspace_map_chunks(dir.path(), "fixture");
    let third = pass
        .brief_document("fixture", WORKSPACE_MAP_SOURCE, &edited, &edited_gaps)
        .await
        .expect("pass");
    assert_eq!(third.briefed, 1, "{third:?}");
    assert_eq!(third.cached, 1, "{third:?}");
}

#[tokio::test]
async fn workspace_map_major_components_recall_with_unrelated_control() {
    use crate::query::capability_match::{funneled_query_signals, rank_briefs, BriefCandidate};
    use crate::query::funnel::funnel_terms;
    let dir = map_fixture();
    let nlp = rule_pipeline();
    let (chunks, gaps) = workspace_map_chunks(dir.path(), "fixture");
    assert!(gaps.is_empty());
    let backend = FakeBackend::ready(
        "{\"workspace-map.md#fluent-router\": \"Major components include the router dispatch crate.\", \"workspace-map.md#guidance-core\": \"Major components include the guidance index crate.\"}",
    );
    let (db, gate, _) = pass_parts();
    let mut pass = CapabilityPass::new(&backend, "test-model".to_string(), &gate, &db, Some(&nlp));
    let outcome = pass
        .brief_document("fixture", WORKSPACE_MAP_SOURCE, &chunks, &gaps)
        .await
        .expect("pass");
    assert_eq!(outcome.briefed, 2, "{outcome:?}");
    let aliases = search_vector::aliases::SemanticAliases::new();
    let recall_rank1 = |query: &str| {
        let funnel = funnel_terms(query, &aliases, None);
        let signals = funneled_query_signals(&funnel, Some(&nlp));
        let expanded = funnel.expanded.join(" ");
        let mut fts_scores: std::collections::HashMap<String, f64> =
            std::collections::HashMap::new();
        if let Ok(found) = db.search_capabilities(&expanded, 20) {
            for hit in found {
                fts_scores.insert(hit.id, hit.score);
            }
        }
        let mut ids: std::collections::HashSet<String> = fts_scores.keys().cloned().collect();
        let lemmas = crate::query::ingest::query_lemmas(Some(&nlp), &expanded);
        if !lemmas.is_empty() {
            if let Ok(lemma_ids) = db.capabilities_for_lemmas(&lemmas, 20) {
                ids.extend(lemma_ids);
            }
        }
        let mut ordered: Vec<String> = ids.into_iter().collect();
        ordered.sort();
        let stored = db.capabilities_for_ids(&ordered).unwrap_or_default();
        let candidates: Vec<BriefCandidate> = stored
            .into_iter()
            .map(|row| {
                let roles: Vec<String> = serde_json::from_str(&row.roles_json).unwrap_or_default();
                BriefCandidate {
                    id: row.id.clone(),
                    purpose: row.purpose,
                    predicate: row.predicate,
                    roles,
                    crate_name: row.crate_name,
                    domain: row.domain,
                    source_path: row.source_path,
                    anchor: row.anchor,
                    fts_score: fts_scores.remove(&row.id),
                }
            })
            .collect();
        rank_briefs(&signals, &candidates)
    };
    let ranked = recall_rank1("what are the major components of this codebase");
    assert!(
        !ranked.is_empty(),
        "map must surface for major-components query"
    );
    assert!(ranked[0].id.starts_with(WORKSPACE_MAP_SOURCE), "{ranked:?}");
    let control = recall_rank1("how do I bake sourdough bread");
    assert!(
        control.first().is_none_or(|top| top.overlap.is_empty()),
        "unrelated query must not promote map: {control:?}"
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
        report.recall_at_1,
        report.control_fires,
        report.control_total
    );
    assert_eq!(report.control_fires, 0);
}
