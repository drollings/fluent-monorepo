//! Single-vocabulary contracts: term extraction, alias expansion,
//! light-verb removal, symbol extraction, deterministic ordering.
//! Included via `#[path]` forwarder from `read/vocabulary.rs`, per
//! repo convention. Pure: no I/O, no model.

use super::*;
use search_vector::aliases::SemanticAliases;

#[test]
fn query_terms_match_the_shared_funnel_rule() {
    assert_eq!(query_terms(""), Vec::<String>::new());
    assert_eq!(
        query_terms("What are the major components"),
        vec!["major".to_string(), "components".to_string()]
    );
    // Length floor and stopwords agree with the funnel exactly.
    assert_eq!(
        query_terms("how does guidance sync decide"),
        crate::query::funnel::significant_terms("how does guidance sync decide")
    );
}

#[test]
fn light_verbs_leave_unless_they_are_all_that_remains() {
    assert_eq!(
        content_terms("how does the router decide where to rank code hits"),
        vec![
            "router".to_string(),
            "rank".to_string(),
            "code".to_string(),
            "hits".to_string()
        ]
    );
    // Only verbs: dropping everything answers nothing, so they stay.
    assert_eq!(
        content_terms("decide how to run"),
        vec!["decide".to_string(), "run".to_string()]
    );
}

#[test]
fn alias_expansion_is_first_seen_ordered() {
    let json = r#"{"hits": ["strike"], "code": ["source"]}"#;
    let aliases = SemanticAliases::from_json(json).expect("aliases");
    assert_eq!(
        expand_terms("code hits", &aliases),
        vec![
            "code".to_string(),
            "source".to_string(),
            "hits".to_string(),
            "strike".to_string()
        ]
    );
    // Repository-neutral default: no aliases, terms pass through.
    assert_eq!(
        expand_terms("code hits", &SemanticAliases::new()),
        vec!["code".to_string(), "hits".to_string()]
    );
}

#[test]
fn symbol_terms_come_from_significant_terms_only() {
    // The shared tokenizer splits snake_case, so multi-part
    // identifiers arrive as parts — identical to the funnel's
    // `symbols` stage, never a second rule.
    assert_eq!(
        symbol_terms("where is clean_build_cache"),
        vec![
            "clean".to_string(),
            "build".to_string(),
            "cache".to_string()
        ]
    );
    // Scaffolding never reaches extraction: `what`/`is`/`the` drop out
    // in the term stage, so only content words are candidates.
    assert_eq!(
        symbol_terms("what is the router dispatch table"),
        vec![
            "router".to_string(),
            "dispatch".to_string(),
            "table".to_string()
        ]
    );
    assert_eq!(symbol_terms("the a an"), Vec::<String>::new());
}

#[test]
fn ordering_is_first_seen_deterministic() {
    assert_eq!(
        expand_terms("beta alpha beta", &SemanticAliases::new()),
        vec!["beta".to_string(), "alpha".to_string()]
    );
}

fn known_crates() -> Vec<String> {
    crate::component_map::known_crate_names()
}

#[test]
fn subject_anchor_prefers_known_crates_over_identifiers() {
    // Control: "router" names a crate; "crate" itself never anchors.
    let anchor =
        subject_anchor("which crate contains the router", &known_crates()).expect("router anchors");
    assert_eq!(anchor.source, AnchorSource::Crate);
    assert_eq!(
        anchor.terms,
        vec!["fluent-router".to_string(), "router".to_string()]
    );
    // Bare scaffolding anchors nothing: pass-through unchanged.
    assert_eq!(subject_anchor("which crate", &known_crates()), None);
    assert_eq!(subject_anchor("the a an", &known_crates()), None);
}

#[test]
fn subject_anchor_finds_the_free_term_subject() {
    // Q2 anchors calibration, not the scaffolding word "crate".
    let anchor = subject_anchor(
        "Which crate owns calibration, and how is a decision point promoted?",
        &known_crates(),
    )
    .expect("Q2 anchors");
    assert_eq!(anchor.source, AnchorSource::Identifier);
    assert!(
        anchor.terms.contains(&"calibration".to_string()),
        "{anchor:?}"
    );
    assert!(!anchor.terms.contains(&"crate".to_string()), "{anchor:?}");
    // Q4 anchors capability. (Q3 resolves through the engine: the
    // crate word "guidance" only anchors ownership questions, so the
    // mechanism query anchors its free term "sync" instead.)
    let anchor = subject_anchor(
        "Where do capability briefs come from and what consumes them?",
        &known_crates(),
    )
    .expect("Q4 anchors");
    assert_eq!(anchor.source, AnchorSource::Identifier);
    assert!(
        anchor.terms.contains(&"capability".to_string()),
        "{anchor:?}"
    );
}

#[test]
fn coldstart_goldens_survive_the_anchor_filter() {
    // Every cold-start question anchors, and every enforced (crate)
    // anchor is mentioned by its golden files and sections — so the
    // filter keeps the answer. Identifier anchors stay shadow and
    // filter nothing.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../doc/guidance/eval/coldstart.jsonl");
    let text = std::fs::read_to_string(&path).expect("coldstart reads");
    let known = known_crates();
    let mut cases = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(line).expect("valid json");
        if value
            .get("control")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        cases += 1;
        let query = value
            .get("query")
            .and_then(|query| query.as_str())
            .expect("query");
        let anchor = subject_anchor(query, &known).expect("cold-start anchors");
        if anchor.source != AnchorSource::Crate {
            continue;
        }
        let mut golden = String::new();
        for key in ["golden_files", "golden_sections", "golden_facts"] {
            if let Some(items) = value.get(key).and_then(|items| items.as_array()) {
                for item in items {
                    if let Some(text) = item.as_str() {
                        golden.push_str(text);
                        golden.push(' ');
                    }
                }
            }
        }
        let golden = golden.to_lowercase();
        assert!(
            anchor
                .terms
                .iter()
                .any(|term| golden.contains(term.as_str())),
            "crate anchor {anchor:?} names no golden for {query:?}"
        );
    }
    assert!(cases >= 6, "six cold-start questions measured");
}

#[test]
fn anchor_selectivity_caps_common_free_terms() {
    let anchor = SubjectAnchor {
        terms: vec!["guidance".to_string()],
        source: AnchorSource::Identifier,
    };
    // "guidance" mentioning nearly every card is not selective.
    let bodies = [
        "guidance sync",
        "guidance index",
        "guidance mirror",
        "other work",
    ];
    assert!(
        !anchor_selective(&anchor, &bodies[..3]),
        "3 of 3 mentions is not selective"
    );
    let half = ["guidance sync", "guidance index", "other work", "more work"];
    assert!(
        anchor_selective(&anchor, &half),
        "exactly half stays selective"
    );
    // Rare terms select; crate anchors always pass.
    let rare = SubjectAnchor {
        terms: vec!["calibration".to_string()],
        source: AnchorSource::Identifier,
    };
    assert!(anchor_selective(&rare, &bodies));
    let exact = SubjectAnchor {
        terms: vec!["fluent-router".to_string()],
        source: AnchorSource::Crate,
    };
    assert!(anchor_selective(&exact, &bodies[..3]));
}

#[test]
fn evidence_grounds_only_on_the_asked_subject() {
    assert!(evidence_names_subject(
        "Which crate owns calibration, and how is a decision point promoted?",
        "# Calibration\n\nDecision point promotion needs a passing registry entry."
    ));
    assert!(
        !evidence_names_subject(
            "What is the zzqy nonexistent subsystem?",
            "pub fn stale_files() -> usize { 0 }"
        ),
        "unrelated code never grounds"
    );
    assert!(
        !evidence_names_subject(
            "airspeed velocity of an unladen swallow",
            "pub fn main_entry() { let step_0 = 0; }"
        ),
        "nonsense names nothing in the index"
    );
}

#[test]
fn identifier_shaped_terms_are_exact_requirements() {
    let query = "Which crate owns the qqqNoSuchSymbol calibrator?";
    assert!(
        !evidence_names_subject(
            query,
            "# Architecture\n\nThis crate owns the calibration map."
        ),
        "camelCase subject must appear byte for byte"
    );
    assert!(evidence_names_subject(
        query,
        "pub struct qqqNoSuchSymbol { value: u64 }"
    ));
    assert!(evidence_names_subject(
        "Where is ANSWER_PROMPT_VERSION set?",
        "pub const ANSWER_PROMPT_VERSION: u64 = 1;"
    ));
}

#[test]
fn empty_queries_never_ground_and_case_is_folded() {
    assert!(!evidence_names_subject("", "anything at all"));
    assert!(!evidence_names_subject("   ", "anything at all"));
    assert!(evidence_names_subject(
        "What owns the build cache?",
        "//! Entry point owning the Build Cache under .guidance/target."
    ));
}
