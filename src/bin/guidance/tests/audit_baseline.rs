//! Baseline lock for the six benchmark questions: the measurement
//! instrument (task file) plus hermetic characterization of the three
//! failure inputs. Included via `#[path]` forwarder from `src/search.rs`,
//! per repo convention.

use super::*;
use std::collections::BTreeSet;

fn task_file() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("guidance")
        .join("corpora")
        .join("benchmark_tasks.jsonl")
}

fn task_ids() -> Vec<(String, Vec<String>)> {
    let text = std::fs::read_to_string(task_file()).expect("benchmark tasks load");
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let value: serde_json::Value =
                serde_json::from_str(line).expect("task line parses");
            let id = value["id"].as_str().expect("task id").to_string();
            let relevant = value["relevant"]
                .as_array()
                .expect("relevant array")
                .iter()
                .map(|entry| entry.as_str().expect("relevant entry").to_string())
                .collect();
            (id, relevant)
        })
        .collect()
}

#[test]
fn benchmark_instrument_covers_six_questions() {
    let tasks = task_ids();
    assert_eq!(tasks.len(), 6);
    let ids: BTreeSet<String> = tasks.iter().map(|task| task.0.clone()).collect();
    for expected in [
        "q1-components",
        "q2-calibration",
        "q3-sync",
        "q4-capabilities",
        "q5-clean",
        "q6-ranking",
    ] {
        assert!(ids.contains(expected), "missing task {expected}");
    }
}

#[test]
fn benchmark_goldens_name_expected_owners() {
    let tasks: std::collections::HashMap<String, Vec<String>> =
        task_ids().into_iter().collect();
    let basenames = |id: &str| -> BTreeSet<String> {
        tasks[id]
            .iter()
            .map(|path| {
                std::path::Path::new(path)
                    .file_name()
                    .expect("basename")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    };
    assert!(basenames("q1-components").contains("README.md"));
    assert!(basenames("q1-components").contains("Cargo.toml"));
    assert!(basenames("q2-calibration").contains("decision_points.rs"));
    assert!(basenames("q2-calibration").contains("registry.rs"));
    assert!(basenames("q3-sync").contains("sync_engine.rs"));
    assert!(basenames("q3-sync").contains("staleness.rs"));
    assert!(basenames("q4-capabilities").contains("capability_match.rs"));
    assert!(basenames("q5-clean").contains("main.rs"));
    assert!(basenames("q6-ranking").contains("search.rs"));
    assert!(basenames("q6-ranking").contains("plan.rs"));
}

#[test]
fn q1_orientation_query_carries_no_symbol_signal() {
    let tokens = significant_query_tokens("What are the major components of this codebase?");
    assert!(!tokens.is_empty());
    let symbols =
        search_vector::tokens::symbol_names("What are the major components of this codebase?");
    // Over-minting characterization: every prose word passes the symbol
    // filter, yet none is identifier-shaped (no `_` or `::`), so the
    // symbol-filtered routes fuse prose matches into rank. M4 gates this.
    assert!(!symbols.is_empty());
    assert!(
        symbols
            .iter()
            .all(|name| !name.contains('_') && !name.contains("::")),
        "orientation prose mints no identifier-shaped filter: {symbols:?}"
    );
    let plan = build_search_plan(
        "What are the major components of this codebase?",
        SearchMode::Fuse,
        10,
        false,
        true,
        &[],
        &[],
    );
    assert!(!plan.routes.is_empty());
}

#[test]
fn q3_sync_query_mints_the_decide_collision_pair() {
    let tokens =
        significant_query_tokens("How does guidance sync decide what to reprocess after an edit?");
    assert!(tokens.contains(&"decide".to_string()), "tokens: {tokens:?}");
    assert!(tokens.contains(&"sync".to_string()), "tokens: {tokens:?}");
}

#[test]
fn q6_ranking_query_names_both_competing_vocabulary() {
    let tokens = significant_query_tokens(
        "How does the query pipeline rank code hits vs capability briefs?",
    );
    assert!(tokens.contains(&"capability".to_string()), "tokens: {tokens:?}");
    assert!(
        tokens.contains(&"rank".to_string()) || tokens.contains(&"code".to_string()),
        "tokens: {tokens:?}"
    );
}

#[test]
fn capability_trigger_floor_is_explicit() {
    assert_eq!(CAPABILITY_TRIGGER_HITS, 3);
}

#[test]
fn doc_rank_drops_zero_overlap_paragraphs() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("weather.md"),
        "# Weather\n\nCompletely unrelated skies and cloud observations here.\n",
    )
    .expect("write");
    let ranked = rank_doc_paragraphs(
        "fee calculator total",
        Some(dir.path()),
        dir.path(),
        5,
        false,
    );
    assert!(ranked.is_empty());
}

struct CountingBackend {
    calls: std::sync::atomic::AtomicUsize,
    fail: bool,
}

impl fluent_llm::client::ChatBackend for CountingBackend {
    fn chat_complete(
        &self,
        _messages: &[fluent_llm::protocol::ChatMessage],
    ) -> Result<String, fluent_llm::protocol::LlmError> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail {
            return Err(fluent_llm::protocol::LlmError::Http("boom".to_string()));
        }
        Ok("{}".to_string())
    }
}

#[test]
fn empty_rank_issues_zero_judge_calls() {
    use std::sync::atomic::Ordering;
    let backend = CountingBackend {
        calls: std::sync::atomic::AtomicUsize::new(0),
        fail: false,
    };
    let kept: Vec<(String, String)> = judge_doc_paragraphs("DependencyGraph", &[], &backend);
    assert!(kept.is_empty());
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
}

/// M2-cal: rank-half calibration over the doc-judge goldens.
///
/// Measured with the real `rank_doc_paragraphs` (overlap >= 1 keeps):
/// keeps surviving rank 3/5 (recall 0.6), traps surviving rank 3/4.
/// The lexical pre-gate is a cost control (judge-call budget), never a
/// relevance authority: semantic keeps with no surface overlap
/// (sync-reprocess keep 0, major-components keep 0) fall out here and
/// can only return via the judge over a wider candidate set or via the
/// capability tier. Bars lock the current behavior against drift; they
/// do not claim the rank half is sufficient.
#[test]
fn doc_gate_rank_half_keeps_and_traps() {
    let cases: Vec<(&str, Vec<&str>, Vec<usize>, Vec<usize>)> = vec![
        (
            "Which crate owns calibration, and how is a decision point promoted?",
            vec![
                "All decision points ship shadow-only: the default model has no calibration entry, so `authorize` fails closed. Promotion needs a passing entry on all three keys (point, model fingerprint, question version).",
                "Stamp the corpus copy with the probed fingerprint. `RegistryEntry::calibrate` over the corpus records the gate measurement truthfully; `gate.passed` may be false, in which case `authorize` still fails closed.",
                "The ladder runs deterministic filter, fast classifier, local model, then frontier. A request only reaches a rung after the previous one failed to resolve it.",
                "The triage scorecard reports macro precision 0.69 and recall 0.639 on the golden set; the decision histogram tracks per-point latency.",
            ],
            vec![0, 1],
            vec![2, 3],
        ),
        (
            "How does guidance sync decide what to reprocess after an edit?",
            vec![
                "A file is complete exactly when its sidecar's modification time is strictly newer than its source's. A missing sidecar means incomplete.",
                "The sync engine regenerates a file when its sidecar is missing, stuck, or older than the source, or when a stored content hash disagrees with live bytes.",
                "The maintenance loop reprocesses dirty files through the cheapest sufficient tier: deterministic fixers, then indexed recipes, then triage.",
            ],
            vec![0, 1],
            vec![2],
        ),
        (
            "What are the major components of this codebase?",
            vec![
                "Member projects: Fluent WVR, Fluent Concurrency, Fluent DAG, shared crates, Coral Router, Coral Context, Guidance, Orator, spacy-rs.",
                "Each component presents the same Arc<dyn Component> interface whether it is a native struct, a WASM plugin, or database-driven config.",
            ],
            vec![0],
            vec![1],
        ),
    ];
    let mut keeps_found = 0;
    let mut keeps_total = 0;
    let mut traps_surviving = 0;
    let mut traps_total = 0;
    for (query, paras, keeps, traps) in &cases {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("doc.md"), paras.join("\n\n")).expect("write");
        let ranked = rank_doc_paragraphs(query, Some(dir.path()), dir.path(), 5, false);
        let survived: Vec<usize> = ranked
            .iter()
            .filter_map(|rank| {
                rank.0
                    .strip_prefix("doc.md#")
                    .and_then(|index| index.parse().ok())
            })
            .collect();
        for keep in keeps {
            keeps_total += 1;
            keeps_found += usize::from(survived.contains(keep));
        }
        for trap in traps {
            traps_total += 1;
            traps_surviving += usize::from(survived.contains(trap));
        }
    }
    assert_eq!((keeps_found, keeps_total), (3, 5));
    assert_eq!((traps_surviving, traps_total), (3, 4));
}

/// M2-cal control: an exact-symbol query over a doc tree ranks nothing,
/// so the judge is never invoked. Control fire-rate at the judge-call
/// level is exactly zero.
#[test]
fn exact_symbol_query_triggers_no_judge_call() {
    use std::sync::atomic::Ordering;
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("calibration.md"),
        "# Calibration\n\nAll decision points ship shadow-only and authorize fails closed.\n",
    )
    .expect("write");
    let ranked = rank_doc_paragraphs(
        "DependencyGraph",
        Some(dir.path()),
        dir.path(),
        5,
        false,
    );
    assert!(ranked.is_empty());
    let backend = CountingBackend {
        calls: std::sync::atomic::AtomicUsize::new(0),
        fail: false,
    };
    let kept = judge_doc_paragraphs("DependencyGraph", &ranked, &backend);
    assert!(kept.is_empty());
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
}

fn evidence_snippets() -> Vec<(String, String, u32, u32, String)> {
    vec![
        (
            "gen_if_stale".to_string(),
            "src/guidance/src/sync_engine.rs".to_string(),
            280,
            310,
            "fn gen_if_stale body".to_string(),
        ),
        (
            "is_stale".to_string(),
            "src/guidance/src/sync/staleness.rs".to_string(),
            1,
            30,
            "fn is_stale body".to_string(),
        ),
    ]
}

#[test]
fn answer_citing_undisplayed_crate_falls_back() {
    let answer = "The decide function in orator-agent/src/tools/policy.rs:85 \
        decides what to reprocess after an edit.";
    let constrained = constrain_answer_to_evidence(answer, &evidence_snippets());
    assert!(
        !constrained.contains("orator"),
        "undisplayed crate must not survive: {constrained}"
    );
    assert!(
        constrained.contains("src/guidance/src/sync_engine.rs:280-310"),
        "fallback must list shown evidence: {constrained}"
    );
}

#[test]
fn answer_citing_shown_evidence_passes_through() {
    let answer = "Reprocessing is decided by gen_if_stale \
        (src/guidance/src/sync_engine.rs:295) with is_stale \
        (src/guidance/src/sync/staleness.rs:5).";
    let constrained = constrain_answer_to_evidence(answer, &evidence_snippets());
    assert_eq!(constrained, answer);
}

#[test]
fn answer_citing_out_of_range_line_falls_back() {
    let answer = "See gen_if_stale (src/guidance/src/sync_engine.rs:99999).";
    let constrained = constrain_answer_to_evidence(answer, &evidence_snippets());
    assert!(
        !constrained.contains("99999"),
        "unshown line must not survive: {constrained}"
    );
    assert!(
        constrained.contains("do not support a verified answer"),
        "fallback template: {constrained}"
    );
}

#[test]
fn answer_without_citations_passes_through() {
    let answer = "The excerpts do not answer the question.";
    let constrained = constrain_answer_to_evidence(answer, &evidence_snippets());
    assert_eq!(constrained, answer);
}

#[test]
fn stored_range_clamps_to_live_lines() {
    assert_eq!(clamp_range_to_live(1, 339, 338), Some((1, 338)));
    assert_eq!(clamp_range_to_live(5, 5, 338), Some((5, 5)));
    assert_eq!(clamp_range_to_live(400, 410, 338), None);
    assert_eq!(clamp_range_to_live(1, 10, 0), None);
}

use guidance_core::search_types::{
    Entity, FileInfo, FragmentContent, FragmentSpan, RecallPath, SearchHit, SearchHitEvidence,
    SearchMatchedBy,
};

fn overlay_hit(relative_path: &str) -> SearchHit {
    SearchHit {
        entity: Entity {
            id: relative_path.to_string(),
            file_id: relative_path.to_string(),
            range: FragmentSpan::File,
            content: FragmentContent::Text {
                text: String::new(),
            },
            metadata: None,
        },
        file: FileInfo {
            id: relative_path.to_string(),
            absolute_path: format!("/repo/{relative_path}"),
            relative_path: relative_path.to_string(),
            root_path: "/repo".to_string(),
            size_bytes: 10,
            last_modified_time: 10,
            content_hash: None,
            kind: None,
            format: "rust".to_string(),
            index_status: None,
        },
        evidence: vec![SearchHitEvidence {
            range: FragmentSpan::File,
            content: FragmentContent::Text {
                text: String::new(),
            },
            metadata: None,
            is_entity: true,
            path: RecallPath::Fts,
            route_id: None,
            query: None,
            rank: None,
            score: None,
            forced: None,
        }],
        rank: 1,
        score: guidance_core::search_types::RrfScore::new(0.5),
        matched_by: SearchMatchedBy::Fts,
        trace: None,
    }
}

fn overlay_order(hits: &[SearchHit]) -> Vec<String> {
    hits.iter()
        .map(|hit| hit.file.relative_path.clone())
        .collect()
}

#[test]
fn crate_overlay_lifts_query_signalled_owner() {
    let router_hit = overlay_hit("src/router/src/stages/nlp.rs");
    let guidance_hit = overlay_hit("src/guidance/src/sync_engine.rs");
    let ranked = apply_crate_overlay(
        vec![router_hit, guidance_hit],
        "How does guidance sync decide what to reprocess after an edit?",
        None,
    );
    assert_eq!(
        overlay_order(&ranked),
        vec![
            "src/guidance/src/sync_engine.rs".to_string(),
            "src/router/src/stages/nlp.rs".to_string(),
        ]
    );
}

#[test]
fn crate_overlay_preserves_membership_and_is_stable() {
    let first = overlay_hit("src/router/src/stages/nlp.rs");
    let second = overlay_hit("src/guidance/src/sync_engine.rs");
    let ranked = apply_crate_overlay(
        vec![first, second],
        "How does guidance sync decide what to reprocess after an edit?",
        None,
    );
    assert_eq!(ranked.len(), 2);
    assert_eq!(
        overlay_order(&ranked).len(),
        overlay_order(&ranked)
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        "no drops, no duplicates"
    );
}

#[test]
fn crate_overlay_is_noop_without_signal() {
    let dag_hit = overlay_hit("src/dag/src/dep_graph.rs");
    let batch_hit = overlay_hit("src/fluent-concurrency/src/batch.rs");
    let before = vec![dag_hit, batch_hit];
    let expected = overlay_order(&before);
    let ranked = apply_crate_overlay(before, "What is a DependencyGraph", None);
    assert_eq!(overlay_order(&ranked), expected);
}

#[test]
fn empty_capability_table_returns_no_briefs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("empty.db");
    let _db = search_vector::GuidanceDb::open(&db_path).expect("open empty db");
    let nlp = guidance_core::query::ingest::LazyNlp::new();
    let branch = recall_capability_briefs_for(
        "Where do capability briefs come from and what consumes them?",
        db_path.to_str().unwrap(),
        &[],
        &nlp,
    );
    assert!(branch.briefs.is_empty());
    assert_eq!(branch.ready, 0);
}

#[test]
fn code_fat_hits_skip_capability_branch_before_db_work() {
    let nlp = guidance_core::query::ingest::LazyNlp::new();
    let hits = vec![
        overlay_hit("src/guidance/src/sync_engine.rs"),
        overlay_hit("src/guidance/src/sync/staleness.rs"),
        overlay_hit("src/guidance/src/freshness.rs"),
    ];
    let branch = recall_capability_briefs_for(
        "How does guidance sync decide what to reprocess after an edit?",
        "/nonexistent/missing.db",
        &hits,
        &nlp,
    );
    assert!(
        branch.briefs.is_empty(),
        "trigger-false must not touch the index at all"
    );
    assert_eq!(branch.ready, 0);
}

#[test]
fn traverse_goldens_pin_rank_one_and_control_exclusion() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("guidance")
        .join("corpora")
        .join("brief_traverse_goldens.json");
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("goldens load"))
            .expect("goldens parse");
    let cases = doc["cases"].as_array().expect("cases");
    assert_eq!(cases.len(), 3);
    for case in cases {
        let briefs = case["briefs"].as_array().expect("briefs");
        let mut traversed = 0;
        for (index, brief) in briefs.iter().enumerate() {
            let rank = index + 1;
            let expected = brief["expected_traverse"].as_bool().expect("flag");
            let opens = brief_opens_traversal(rank, BriefStanding::Unjudged);
            assert_eq!(
                opens, expected,
                "case {:?} brief {}: rank {rank} opens={opens} expected={expected}",
                case["query"], brief["id"]
            );
            traversed += usize::from(opens);
            if brief["control"].as_bool().unwrap_or(false) {
                assert!(
                    !opens,
                    "control trap must never traverse: {}",
                    brief["id"]
                );
            }
        }
        assert_eq!(traversed, 1, "rank-1-only bound: {:?}", case["query"]);
    }
}

use fluent_llm::decision::CalibrationStatus;
use std::collections::HashMap;

fn shadow_only() -> CalibrationStatus {
    CalibrationStatus::ShadowOnly {
        reason: "uncalibrated".to_string(),
    }
}

fn q3_findings() -> Vec<Finding> {
    vec![
        Finding {
            id: "orator-agent/src/tools/policy.rs".to_string(),
            symbol: Some("decide".to_string()),
            package: "orator-agent".to_string(),
        },
        Finding {
            id: "router/src/stages/nlp.rs".to_string(),
            symbol: Some("decide".to_string()),
            package: "fluent-router".to_string(),
        },
        Finding {
            id: "guidance/src/sync_engine.rs".to_string(),
            symbol: Some("gen_if_stale".to_string()),
            package: "guidance-core".to_string(),
        },
        Finding {
            id: "protopyne-venue/src/paper.rs".to_string(),
            symbol: Some("evaluate".to_string()),
            package: "protopyne-venue".to_string(),
        },
    ]
}

fn exclude_all_verdicts(findings: &[Finding]) -> HashMap<String, FindingVerdict> {
    findings
        .iter()
        .map(|finding| {
            (
                finding.id.clone(),
                FindingVerdict {
                    standing: FindingStanding::Exclude,
                    reason: "synthetic".to_string(),
                },
            )
        })
        .collect()
}

#[test]
fn shadow_only_verdicts_leave_deterministic_set_unchanged() {
    let findings = q3_findings();
    let verdicts = exclude_all_verdicts(&findings);
    let outcome = advisory_filter_findings(findings.clone(), &verdicts, &shadow_only());
    assert_eq!(
        outcome.kept.iter().map(|f| &f.id).collect::<Vec<_>>(),
        findings.iter().map(|f| &f.id).collect::<Vec<_>>()
    );
    assert!(outcome.excluded.is_empty());
    assert_eq!(outcome.noted.len(), 4, "verdicts recorded, not obeyed");
}

#[test]
fn filter_order_follows_findings_not_verdicts() {
    let mut findings = q3_findings();
    findings.reverse();
    let verdicts = exclude_all_verdicts(&q3_findings());
    let outcome = advisory_filter_findings(findings.clone(), &verdicts, &shadow_only());
    assert_eq!(
        outcome.kept.iter().map(|f| &f.id).collect::<Vec<_>>(),
        findings.iter().map(|f| &f.id).collect::<Vec<_>>()
    );
}

#[test]
fn deterministic_verdicts_flag_unsigned_owner_rows() {
    let findings = q3_findings();
    let tokens = significant_query_tokens(
        "How does guidance sync decide what to reprocess after an edit?",
    );
    let mut verdicts = deterministic_finding_verdicts(&findings, &tokens);
    // Advisory never excludes, even when told to: the would-drop is
    // recorded and the finding stays.
    verdicts.insert(
        "router/src/stages/nlp.rs".to_string(),
        FindingVerdict {
            standing: FindingStanding::Exclude,
            reason: "synthetic drop".to_string(),
        },
    );
    let advisory = CalibrationStatus::Advisory(fluent_llm::decision::ThresholdSet {
        act: 0.9,
        review: 0.7,
    });
    let outcome = advisory_filter_findings(findings, &verdicts, &advisory);
    assert_eq!(outcome.kept.len(), 4, "shadow regime never drops");
    assert!(outcome.excluded.is_empty());
    let flagged: Vec<&str> = outcome
        .flagged
        .iter()
        .map(|(id, _)| id.as_str())
        .collect();
    // Structural symbol matches include regardless of crate; the
    // owner-unsigned row flags without dropping.
    assert!(flagged.contains(&"protopyne-venue/src/paper.rs"));
    assert!(
        !flagged.contains(&"guidance/src/sync_engine.rs"),
        "signalled owner stays unflagged: {flagged:?}"
    );
}

#[test]
fn advisory_never_excludes_even_when_told_to() {
    // M6-cal control: unanimous Exclude verdicts under Advisory keep
    // every finding and record every would-drop. Exclusion requires
    // Calibrated authorization, which no empty registry can grant.
    let findings = q3_findings();
    let verdicts = exclude_all_verdicts(&findings);
    let advisory = CalibrationStatus::Advisory(fluent_llm::decision::ThresholdSet {
        act: 0.9,
        review: 0.7,
    });
    let outcome = advisory_filter_findings(findings.clone(), &verdicts, &advisory);
    assert_eq!(outcome.kept.len(), findings.len());
    assert!(outcome.excluded.is_empty());
    assert_eq!(outcome.noted.len(), findings.len());
}

#[test]
fn real_doc_tree_jumps_resolve_to_golden_owners() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..");
    for (doc, owner) in [
        ("doc/guidance/ARCHITECTURE.md", "guidance-core"),
        ("doc/guidance/CALIBRATION.md", "guidance-core"),
        ("doc/guidance/CAPABILITY_SUMMARIES.md", "guidance-core"),
    ] {
        let text = std::fs::read_to_string(root.join(doc))
            .unwrap_or_else(|_| panic!("repo doc readable: {doc}"));
        let jumps = guidance_core::jumps::derive_jumps_for_doc(doc, &text);
        assert!(!jumps.is_empty(), "{doc} chunks");
        assert!(
            jumps.iter().all(|jump| jump.crate_name == owner),
            "{doc} owner"
        );
    }
    let text = std::fs::read_to_string(root.join("doc/guidance/ARCHITECTURE.md"))
        .expect("architecture");
    let jumps =
        guidance_core::jumps::derive_jumps_for_doc("doc/guidance/ARCHITECTURE.md", &text);
    let terms: Vec<&str> = jumps
        .iter()
        .flat_map(|jump| jump.terms.iter().map(String::as_str))
        .collect();
    assert!(terms.contains(&"sync"), "overview funnel mentions sync");
}
