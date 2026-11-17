//! Rule-table intent routing: exact prechecks, cue phrases, light
//! verbs, controls. Included via `#[path]` forwarder from
//! `read/intent.rs`, per repo convention. Pure: no I/O, no model.

use super::*;
use crate::query::strategy::QueryIntent;

#[test]
fn exact_prechecks_win_without_a_model() {
    assert_eq!(classify(""), QueryIntent::GeneralSearch);
    assert_eq!(classify("   "), QueryIntent::GeneralSearch);
    assert_eq!(classify("clean_build_cache"), QueryIntent::SingleIdentifier);
    assert_eq!(classify("src/router/dispatch.rs"), QueryIntent::FilePath);
    assert_eq!(
        classify("where is clean_build_cache"),
        QueryIntent::SingleIdentifier
    );
    assert_eq!(
        classify("where is clean_build_cache defined"),
        QueryIntent::SingleIdentifier
    );
    assert_eq!(classify("what is FsmEngine"), QueryIntent::SingleIdentifier);
}

#[test]
fn graph_cues_route_to_graph() {
    assert_eq!(classify("callers of gen_scoped"), QueryIntent::Graph);
    assert_eq!(classify("callees of dispatch"), QueryIntent::Graph);
    assert_eq!(classify("dependents of SyncEngine"), QueryIntent::Graph);
    assert_eq!(classify("owner of dispatch"), QueryIntent::Graph);
    assert_eq!(classify("who calls clean_build_cache"), QueryIntent::Graph);
}

#[test]
fn ownership_cues_route_to_ownership() {
    assert_eq!(
        classify("which crate owns calibration"),
        QueryIntent::Ownership
    );
    assert_eq!(classify("who owns this file"), QueryIntent::Ownership);
    assert_eq!(
        classify("where does sync state live"),
        QueryIntent::Ownership
    );
}

#[test]
fn orientation_cues_route_to_orientation() {
    assert_eq!(
        classify("what are the major components of this codebase"),
        QueryIntent::Orientation
    );
    assert_eq!(
        classify("list the member projects"),
        QueryIntent::Orientation
    );
    assert_eq!(
        classify("what is this repo about"),
        QueryIntent::Orientation
    );
    assert_eq!(
        classify("high level codebase tour"),
        QueryIntent::Orientation
    );
}

#[test]
fn provenance_cues_route_to_provenance() {
    assert_eq!(
        classify("where do capability briefs come from"),
        QueryIntent::Provenance
    );
    assert_eq!(
        classify("what consumes the jump manifest"),
        QueryIntent::Provenance
    );
}

#[test]
fn mechanism_queries_stay_code_first() {
    assert_eq!(
        classify("how does guidance sync decide what to reprocess"),
        QueryIntent::HowTo
    );
    assert_eq!(
        classify("how is the query pipeline ranked"),
        QueryIntent::HowTo
    );
    assert_eq!(
        classify("why is the vector route empty"),
        QueryIntent::HowTo
    );
    assert_eq!(classify("what happens when sync runs"), QueryIntent::HowTo);
}

#[test]
fn generic_verbs_do_not_hijack_routing() {
    // `decide`/`rank`/`code` are light verbs, not symbol intent: a
    // mechanism question mentioning them stays code-first instead of
    // routing to an exact-symbol lookup.
    assert_eq!(
        classify("how does the router decide where to rank code hits"),
        QueryIntent::HowTo
    );
    assert_eq!(
        classify("explain the decide logic"),
        QueryIntent::GeneralSearch
    );
}

#[test]
fn fallback_is_general() {
    assert_eq!(
        classify("zxqy nonexistent token zzz"),
        QueryIntent::GeneralSearch
    );
    assert_eq!(classify("the thing"), QueryIntent::GeneralSearch);
}

#[test]
fn symbol_extraction_runs_only_for_symbol_and_graph() {
    assert!(extracts_symbols(QueryIntent::SingleIdentifier));
    assert!(extracts_symbols(QueryIntent::Graph));
    assert!(!extracts_symbols(QueryIntent::HowTo));
    assert!(!extracts_symbols(QueryIntent::Orientation));
    assert!(!extracts_symbols(QueryIntent::GeneralSearch));
    assert!(!extracts_symbols(QueryIntent::Ownership));
    assert!(!extracts_symbols(QueryIntent::Provenance));
}

/// M3.1 characterization: `contains_cue` boundary table (C8).
/// Whole-word match on `!alphanumeric && != '_'` separators:
/// `_` joins, Unicode alphanumerics count as word chars, empty
/// cues never match. Must hold after migrating onto the shared
/// separator-predicate primitive.
#[test]
fn contains_cue_boundary_table() {
    let cases: &[(&str, &str, bool)] = &[
        ("how does the pipeline work", "pipeline", true),
        ("pipelines carry data", "pipeline", false),
        ("pipeline!", "pipeline", true),
        ("my_pipeline", "pipeline", false),
        ("pipe x", "pipe", true),
        ("xpipe", "pipe", false),
        ("anything", "", false),
        ("anything", "   ", false),
        ("le m\u{e9}canisme", "m\u{e9}canisme", true),
        ("how does the pipeline work", " pipeline ", true),
        ("a-b c", "b", true),
    ];
    for (lower, cue, want) in cases {
        assert_eq!(
            super::contains_cue(lower, cue),
            *want,
            "lower={lower:?} cue={cue:?}"
        );
    }
}
