//! Unit tests for the `search` command shell (pure plan/render layer plus
//! the hermetic `--rg` L0 path). Included via `#[path]` forwarder from
//! `src/search.rs`, per repo convention.

use super::*;
use guidance_core::query::rg_backend::RgHit;
use guidance_core::query::structure_enrich::EnrichedRgHit;
use guidance_core::zg_types::SearchPlanRouteMode;

fn rg_opts() -> RgDisplayOptions {
    RgDisplayOptions::default()
}

#[test]
fn mode_defaults_to_fuse() {
    assert_eq!(
        resolve_mode(false, false, false, false).unwrap(),
        SearchMode::Fuse
    );
    assert_eq!(
        resolve_mode(false, false, false, true).unwrap(),
        SearchMode::Fuse
    );
    assert_eq!(
        resolve_mode(true, false, false, false).unwrap(),
        SearchMode::Fts
    );
    assert_eq!(
        resolve_mode(false, true, false, false).unwrap(),
        SearchMode::Vector
    );
    assert_eq!(
        resolve_mode(false, false, true, false).unwrap(),
        SearchMode::Rg
    );
}

#[test]
fn conflicting_mode_flags_are_rejected() {
    assert!(resolve_mode(true, true, false, false).is_err());
    assert!(resolve_mode(true, false, true, false).is_err());
    assert!(resolve_mode(false, true, true, true).is_err());
}

#[test]
fn symbol_type_parses_the_closed_6_set() {
    assert_eq!(parse_symbol_type("module").unwrap(), CodeSymbolType::Module);
    assert_eq!(parse_symbol_type("class").unwrap(), CodeSymbolType::Class);
    assert_eq!(
        parse_symbol_type("interface").unwrap(),
        CodeSymbolType::Interface
    );
    assert_eq!(
        parse_symbol_type("function").unwrap(),
        CodeSymbolType::Function
    );
    assert_eq!(parse_symbol_type("value").unwrap(), CodeSymbolType::Value);
    assert_eq!(parse_symbol_type("alias").unwrap(), CodeSymbolType::Alias);
    assert!(parse_symbol_type("namespace").is_err());
    assert!(parse_symbol_type("FUNCTION").is_err());
}

#[test]
fn limit_clamps_to_1_through_50() {
    assert_eq!(clamp_limit(0), 1);
    assert_eq!(clamp_limit(7), 7);
    assert_eq!(clamp_limit(50), 50);
    assert_eq!(clamp_limit(500), 50);
}

#[test]
fn fuse_plan_carries_trace_and_filters() {
    let plan = build_search_plan(
        "needle",
        SearchMode::Fuse,
        7,
        true,
        true,
        &["src/**".to_string()],
        &[CodeSymbolType::Function],
    );
    assert!(!plan.routes.is_empty());
    assert_eq!(plan.limit, Some(7));
    assert!(plan.trace);
    assert_eq!(plan.prefer_symbol, Some(true));
    assert_eq!(plan.globs, Some(vec!["src/**".to_string()]));
    assert_eq!(plan.symbol_types, Some(vec![CodeSymbolType::Function]));
}

#[test]
fn single_route_plans_select_exactly_one_mode() {
    let fts = build_search_plan("needle", SearchMode::Fts, 5, false, false, &[], &[]);
    assert_eq!(fts.routes.len(), 1);
    assert_eq!(fts.routes[0].mode, SearchPlanRouteMode::Fts);
    assert_eq!(fts.routes[0].query, "needle");
    let vector = build_search_plan("needle", SearchMode::Vector, 5, false, false, &[], &[]);
    assert_eq!(vector.routes.len(), 1);
    assert_eq!(vector.routes[0].mode, SearchPlanRouteMode::Vector);
}

#[test]
fn rg_hits_render_as_path_line_col() {
    let hits = vec![EnrichedRgHit {
        hit: RgHit {
            path: "src/a.ts".to_string(),
            line: 3,
            column: 7,
            text: "const Needle = 1;\n".to_string(),
            context_before: vec!["const Before = 0;\n".to_string()],
            context_after: Vec::new(),
        },
        symbol: Some("holder_fn".to_string()),
    }];
    assert_eq!(
        render_rg_hits(&hits, 10),
        "coverage: rg_exhaustive\nsrc/a.ts:3:7 [holder_fn]: const Needle = 1;\n  const Before = 0;\n"
    );
    assert_eq!(
        render_rg_hits(&hits, 1),
        "coverage: rg_truncated\nsrc/a.ts:3:7 [holder_fn]: const Needle = 1;\n  const Before = 0;\n"
    );
}

#[test]
fn missing_index_is_a_named_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("no-such.db");
    let error = run_search(
        "needle",
        dir.path().to_str().unwrap(),
        missing.to_str().unwrap(),
        5,
        false,
        false,
        false,
        false,
        false,
        false,
        false,
        &[],
        &[],
        &rg_opts(),
    )
    .expect_err("missing index must fail");
    assert!(error.contains("run `guidance index` first"), "{error}");
}

#[test]
fn invalid_symbol_type_is_a_named_error() {
    let result = run_search(
        "needle",
        ".",
        ".guidance.db",
        5,
        false,
        false,
        false,
        false,
        false,
        false,
        false,
        &[],
        &["namespace".to_string()],
        &rg_opts(),
    );
    assert!(result.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn rg_route_reports_context_and_symbols() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("b.rs"),
        "fn ctx_holder() {\n    let a = 1;\n    let CtxNeedle = a;\n    let z = 2;\n}\n",
    )
    .expect("write");
    let workspace = dir.path().to_str().unwrap().to_string();
    let unused_db = dir.path().join("unused.db");
    let unused_db = unused_db.to_str().unwrap().to_string();
    let options = RgDisplayOptions {
        before: 1,
        after: 1,
        mtime_after_ms: None,
        mtime_before_ms: None,
        enrich: true,
    };
    let rendered = run_search(
        "CtxNeedle",
        &workspace,
        &unused_db,
        10,
        false,
        false,
        true,
        false,
        false,
        false,
        false,
        &[],
        &[],
        &options,
    )
    .expect("rg route");
    assert!(rendered.contains("coverage: rg_exhaustive"), "{rendered}");
    assert!(rendered.contains("b.rs:3:9 [ctx_holder]:"), "{rendered}");
    assert!(rendered.contains("let a = 1;"), "{rendered}");
    assert!(rendered.contains("let z = 2;"), "{rendered}");
}

#[tokio::test(flavor = "multi_thread")]
async fn rg_route_searches_without_an_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.rs"), "fn needle_fn() {}\n").expect("write");
    let workspace = dir.path().to_str().unwrap().to_string();
    let unused_db = dir.path().join("unused.db");
    let unused_db = unused_db.to_str().unwrap().to_string();
    let rendered = run_search(
        "needle_fn",
        &workspace,
        &unused_db,
        10,
        false,
        false,
        true,
        false,
        false,
        false,
        false,
        &[],
        &[],
        &rg_opts(),
    )
    .expect("rg route");
    assert!(rendered.contains("a.rs:1:"), "{rendered}");
}
