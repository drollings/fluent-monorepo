//! Unit tests for the MCP `guidance_explain` contract (bounds, plans,
//! rendering). Included via `#[path]` forwarder from `src/mcp.rs`, per repo
//! convention.

use super::*;
use guidance_core::search_types::{
    Entity, EntityMetadata, FileInfo, SearchHit, SearchHitEvidence, SearchPlanRouteMode,
};

fn args(json: serde_json::Value) -> Result<ExplainParams, String> {
    parse_explain_params(&json)
}

#[test]
fn query_is_required() {
    assert!(args(serde_json::json!({})).is_err());
    assert!(args(serde_json::json!({"query": "  "})).is_err());
}

#[test]
fn query_length_is_bounded() {
    let long = "x".repeat(MAX_QUERY_CHARS + 1);
    let error = args(serde_json::json!({"query": long})).expect_err("must fail");
    assert!(error.contains("4000"), "{error}");
    assert!(args(serde_json::json!({"query": "ok"})).is_ok());
}

#[test]
fn groups_are_bounded_at_32() {
    let queries: Vec<String> = (0..MAX_GROUPS + 1)
        .map(|index| format!("q{index}"))
        .collect();
    let error = args(serde_json::json!({"query": "a", "queries": queries})).expect_err("must fail");
    assert!(error.contains("32"), "{error}");
}

#[test]
fn route_flags_are_exclusive() {
    assert!(args(serde_json::json!({"query": "a", "fts": true, "vector": true})).is_err());
    assert!(args(serde_json::json!({"query": "a", "fuse": true, "fts": true})).is_err());
    assert!(args(serde_json::json!({"query": "a", "fuse": true})).is_ok());
}

#[test]
fn limit_clamps_to_50() {
    assert_eq!(
        args(serde_json::json!({"query": "a", "limit": 500}))
            .unwrap()
            .limit,
        50
    );
    assert_eq!(args(serde_json::json!({"query": "a"})).unwrap().limit, 10);
}

#[test]
fn path_filters_are_bounded_at_128() {
    let globs: Vec<String> = (0..MAX_PATH_FILTERS + 1)
        .map(|index| format!("g{index}"))
        .collect();
    let error = args(serde_json::json!({"query": "a", "globs": globs})).expect_err("must fail");
    assert!(error.contains("128"), "{error}");
}

#[test]
fn symbol_types_reject_open_vocabulary() {
    assert!(args(serde_json::json!({"query": "a", "symbolTypes": ["namespace"]})).is_err());
    let params = args(serde_json::json!({"query": "a", "symbolTypes": ["function"]})).unwrap();
    assert_eq!(params.symbol_types, vec![CodeSymbolType::Function]);
}

#[test]
fn freshness_and_mtime_validate() {
    assert!(args(serde_json::json!({"query": "a", "freshness": "soon"})).is_err());
    assert!(args(serde_json::json!({"query": "a", "mtimeAfter": -1})).is_err());
    let params = args(serde_json::json!({"query": "a", "freshness": "wait_for_fresh"})).unwrap();
    assert_eq!(params.freshness, FreshnessMode::WaitForFresh);
}

#[test]
fn context_flag_defaults_off_and_parses() {
    assert!(!args(serde_json::json!({"query": "a"})).unwrap().context);
    assert!(!args(serde_json::json!({"query": "a", "context": false}))
        .unwrap()
        .context);
    assert!(args(serde_json::json!({"query": "a", "context": true}))
        .unwrap()
        .context);
}

#[test]
fn fuse_is_the_default_plan() {
    let params = args(serde_json::json!({"query": "needle"})).unwrap();
    let plans = build_explain_plans(&params);
    assert_eq!(plans.len(), 1);
    assert!(!plans[0].routes.is_empty());
}

#[test]
fn fts_flag_selects_a_single_lexical_route() {
    let params = args(serde_json::json!({"query": "needle", "fts": true})).unwrap();
    let plans = build_explain_plans(&params);
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].routes.len(), 1);
    assert_eq!(plans[0].routes[0].mode, SearchPlanRouteMode::Fts);
}

fn synthetic_hit() -> SearchHit {
    SearchHit {
        entity: Entity {
            id: "entity-1".to_string(),
            file_id: "file-1".to_string(),
            range: FragmentSpan::Text {
                start_line: 10,
                end_line: 12,
                start_offset: 0,
                end_offset: 30,
            },
            content: FragmentContent::Text {
                text: "fn needle() {}".to_string(),
            },
            metadata: None,
        },
        file: FileInfo {
            id: "file-1".to_string(),
            absolute_path: "/ws/src/a.rs".to_string(),
            relative_path: "src/a.rs".to_string(),
            root_path: "/ws".to_string(),
            size_bytes: 16,
            last_modified_time: 0,
            content_hash: None,
            kind: None,
            format: "rust".to_string(),
            index_status: None,
        },
        evidence: vec![SearchHitEvidence {
            range: FragmentSpan::Text {
                start_line: 10,
                end_line: 10,
                start_offset: 0,
                end_offset: 14,
            },
            content: FragmentContent::Text {
                text: "fn needle() {}".to_string(),
            },
            metadata: Some(EntityMetadata::Code {
                symbol_type: CodeSymbolType::Function,
                symbol_name: Some("needle".to_string()),
                scope: None,
                node_type: None,
                signature: None,
                doc: None,
                modifiers: Vec::new(),
            }),
            is_entity: true,
            path: guidance_core::search_types::RecallPath::Fts,
            route_id: None,
            query: None,
            rank: None,
            score: None,
            forced: None,
        }],
        rank: 1,
        score: guidance_core::search_types::RrfScore::new(12.5),
        matched_by: SearchMatchedBy::Fts,
        trace: None,
    }
}

#[test]
fn render_carries_freshness_coverage_ranges_and_matched_lines() {
    let text = render_explain(
        &[synthetic_hit()],
        10,
        Freshness::Fresh,
        Coverage::RankedSample,
        None,
    );
    assert!(text.contains("freshness: fresh"), "{text}");
    assert!(text.contains("coverage: ranked_sample"), "{text}");
    assert!(text.contains("src/a.rs:10-10"), "{text}");
    assert!(text.contains("matched: fn needle() {}"), "{text}");
    assert!(text.contains("matched_by fts"), "{text}");
    assert!(text.contains("needle"), "{text}");
}

#[test]
fn render_empty_reports_no_results_with_headers() {
    let text = render_explain(
        &[],
        10,
        Freshness::PossiblyStale,
        Coverage::RankedSample,
        None,
    );
    assert!(text.contains("freshness: possibly_stale"), "{text}");
    assert!(text.contains("No results found."), "{text}");
}

#[test]
fn toolset_parses_agent_and_full() {
    assert_eq!(Toolset::parse("agent").unwrap(), Toolset::Agent);
    assert_eq!(Toolset::parse("full").unwrap(), Toolset::Full);
    assert!(Toolset::parse("everything").is_err());
}
