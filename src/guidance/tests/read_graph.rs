//! Symbol and graph plane contracts: exact lookup, dotted/terminal
//! normalization, callers/callees/dependents/ownership, and honest
//! empty for unknown symbols. Included via `#[path]` forwarders from
//! `read/planes`, per repo convention. Hermetic: hand-built rows in
//! an in-memory index, no endpoint, no parsing.

use super::graph::{CallerFilter, GraphPlane};
use super::symbol::{normalize_symbol_query, GuidanceStructureStore, StructureStore};

fn seed_db() -> search_vector::db::GuidanceDb {
    use search_vector::db::{FileRecord, FragmentRecord};
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    let file = |id: &str, abs: &str| FileRecord {
        id: id.to_string(),
        absolute_path: abs.to_string(),
        relative_path: abs.trim_start_matches("/repo/").to_string(),
        root_path: "/repo".to_string(),
        size_bytes: 100,
        last_modified_time: 100,
        kind: Some("code".to_string()),
        format: "rust".to_string(),
        content_hash: None,
        index_status: Some("indexed".to_string()),
        fail_count: 0,
        last_error: None,
    };
    let frag = |id: &str, file_id: &str, symbol: &str| FragmentRecord {
        id: id.to_string(),
        group: None,
        file_id: file_id.to_string(),
        range_json: "{\"kind\":\"item\"}".to_string(),
        content_kind: "text".to_string(),
        content_text: Some(format!("fn {symbol}() {{}}")),
        cjk_text: String::new(),
        symbol_type: Some("function".to_string()),
        symbol_name: Some(symbol.to_string()),
        scope: None,
        signature: Some(format!("fn {symbol}()")),
        doc: None,
        modifiers: String::new(),
        heading: None,
        heading_level: None,
        embedding: None,
    };
    db.upsert_fragments(
        &file("file-a", "/repo/src/a.rs"),
        &[
            frag("a-fn", "file-a", "dispatch"),
            frag("a-help", "file-a", "helper"),
        ],
        &[search_vector::db::FragmentLemma {
            fragment_id: "a-fn".to_string(),
            lemma: "dispatch".to_string(),
            confidence: 1.0,
        }],
    )
    .expect("seed a");
    db.upsert_fragments(
        &file("file-b", "/repo/src/b.rs"),
        &[
            frag("b-run", "file-b", "runner"),
            frag("b-fn", "file-b", "dispatch"),
        ],
        &[],
    )
    .expect("seed b");
    // Plain symbol rows mirror the harvest (fragments carry the text,
    // `symbols` carries identity): a homonym definition plus unique names.
    // (`replace_symbols` is delete-by-file, so rows batch per file.)
    for (file_id, rows) in [
        (
            "file-a",
            vec![("file-a#dispatch", "dispatch"), ("file-a#helper", "helper")],
        ),
        (
            "file-b",
            vec![("file-b#runner", "runner"), ("file-b#dispatch", "dispatch")],
        ),
    ] {
        db.replace_symbols(
            &rows
                .into_iter()
                .map(|(symbol_id, name)| search_vector::db::SymbolRecord {
                    symbol_id: symbol_id.to_string(),
                    file_id: file_id.to_string(),
                    name: Some(name.to_string()),
                    scope: None,
                    interface_hash: format!("hash-{symbol_id}"),
                })
                .collect::<Vec<_>>(),
        )
        .expect("seed symbols");
    }
    // Graph rows declare definitions with their calls: b's runner
    // calls dispatch (defined twice → homonym tier); a's helper calls
    // dispatch in its own file (same-file callers resolve like
    // cross-file ones).
    db.replace_file_graph(
        "/repo/src/b.rs",
        &[],
        &[
            ("runner".to_string(), vec!["dispatch".to_string()]),
            ("dispatch".to_string(), vec![]),
        ],
    )
    .expect("graph b");
    db.replace_file_graph(
        "/repo/src/a.rs",
        &[],
        &[
            ("dispatch".to_string(), vec![]),
            ("helper".to_string(), vec!["dispatch".to_string()]),
        ],
    )
    .expect("graph a");
    db
}

#[test]
fn symbol_query_normalizes_dotted_and_terminal_forms() {
    assert_eq!(
        normalize_symbol_query("dispatch"),
        vec!["dispatch".to_string()]
    );
    assert_eq!(
        normalize_symbol_query("engine.gen_scoped"),
        vec!["engine.gen_scoped".to_string(), "gen_scoped".to_string()]
    );
    assert!(normalize_symbol_query("  ").is_empty());
    assert_eq!(
        normalize_symbol_query("\"quoted\""),
        vec!["quoted".to_string()]
    );
}

#[test]
fn exact_lookup_lists_homonyms_and_empty_unknowns() {
    let db = seed_db();
    let store = GuidanceStructureStore::new(&db);
    let sites = store.exact_symbols("dispatch");
    assert_eq!(sites.len(), 2, "{sites:?}");
    assert!(sites.iter().any(|site| site.file == "file-a"));
    assert!(sites.iter().any(|site| site.file == "file-b"));
    let runner = store.exact_symbols("runner");
    assert_eq!(runner.len(), 1, "{runner:?}");
    assert_eq!(runner[0].file, "file-b");
    // Dotted forms fall back to the terminal spelling: expansion, not
    // narrowing — tiers on the rows/edges stay authoritative.
    let dotted = store.exact_symbols("a.dispatch");
    assert_eq!(dotted, sites, "{dotted:?}");
    assert!(store.exact_symbols("no_such_symbol").is_empty());
    assert!(store.exact_symbols("").is_empty());
}

#[test]
fn callers_expand_homonyms_with_tiers() {
    use crate::graph_index::ResolutionMethod;
    let db = seed_db();
    let plane = GraphPlane::hydrate(&db, &["/repo".to_string()]).expect("hydrate");
    let callers = plane.callers("dispatch");
    // Cross-file (b's runner) and same-file (a's helper) callers both
    // resolve; the homonym tier counts both definitions.
    assert_eq!(callers.len(), 2, "{callers:?}");
    assert!(
        callers
            .iter()
            .any(|caller| caller.file == "/repo/src/b.rs"
                && caller.symbol.as_deref() == Some("runner")),
        "{callers:?}"
    );
    assert!(
        callers
            .iter()
            .any(|caller| caller.file == "/repo/src/a.rs"
                && caller.symbol.as_deref() == Some("helper")),
        "{callers:?}"
    );
    assert!(
        callers
            .iter()
            .all(|caller| caller.resolution == ResolutionMethod::Homonym { count: 2 }),
        "{callers:?}"
    );
    // Terminal form reaches the same edge as the dotted form.
    let dotted = plane.callers("engine.dispatch");
    assert_eq!(dotted, callers);
    // Unknown symbols are honest empty, never lexical junk.
    assert!(plane.callers("no_such_symbol").is_empty());
    assert!(plane.callers("").is_empty());
}

fn two_crate_db() -> search_vector::db::GuidanceDb {
    // One homonym split across two crates: alpha defines and calls
    // `duet`, beta defines and calls `duet`.
    use search_vector::db::{FileRecord, FragmentRecord};
    let db = search_vector::db::GuidanceDb::open_in_memory().expect("db");
    let file = |id: &str, abs: &str| FileRecord {
        id: id.to_string(),
        absolute_path: abs.to_string(),
        relative_path: abs.trim_start_matches("/repo/").to_string(),
        root_path: "/repo".to_string(),
        size_bytes: 100,
        last_modified_time: 100,
        kind: Some("code".to_string()),
        format: "rust".to_string(),
        content_hash: None,
        index_status: Some("indexed".to_string()),
        fail_count: 0,
        last_error: None,
    };
    let frag = |id: &str, file_id: &str, symbol: &str| FragmentRecord {
        id: id.to_string(),
        group: None,
        file_id: file_id.to_string(),
        range_json: "{\"kind\":\"item\"}".to_string(),
        content_kind: "text".to_string(),
        content_text: Some(format!("fn {symbol}() {{}}")),
        cjk_text: String::new(),
        symbol_type: Some("function".to_string()),
        symbol_name: Some(symbol.to_string()),
        scope: None,
        signature: None,
        doc: None,
        modifiers: String::new(),
        heading: None,
        heading_level: None,
        embedding: None,
    };
    db.upsert_fragments(
        &file("a", "/repo/src/alpha/a.rs"),
        &[
            frag("a-duet", "a", "duet"),
            frag("a-sing", "a", "sing_alpha"),
        ],
        &[],
    )
    .expect("seed a");
    db.upsert_fragments(
        &file("b", "/repo/src/beta/b.rs"),
        &[
            frag("b-duet", "b", "duet"),
            frag("b-sing", "b", "sing_beta"),
        ],
        &[],
    )
    .expect("seed b");
    db.replace_file_graph(
        "/repo/src/alpha/a.rs",
        &[],
        &[
            ("duet".to_string(), vec![]),
            ("sing_alpha".to_string(), vec!["duet".to_string()]),
        ],
    )
    .expect("graph a");
    db.replace_file_graph(
        "/repo/src/beta/b.rs",
        &[],
        &[
            ("duet".to_string(), vec![]),
            ("sing_beta".to_string(), vec!["duet".to_string()]),
        ],
    )
    .expect("graph b");
    db
}

#[test]
fn scoped_planes_narrow_homonyms_to_the_named_crate() {
    let db = two_crate_db();
    let plane = GraphPlane::hydrate(&db, &["/repo".to_string()]).expect("hydrate");
    assert_eq!(plane.callers("duet").len(), 2, "unscoped spans both crates");
    let alpha = vec!["src/alpha".to_string()];
    let callers = plane.callers_in_scope("duet", CallerFilter::default(), &alpha);
    assert_eq!(callers.len(), 1, "{callers:?}");
    assert_eq!(callers[0].file, "/repo/src/alpha/a.rs");
    assert_eq!(callers[0].symbol.as_deref(), Some("sing_alpha"));
    let callees = plane.callees_in_scope("sing_alpha", &alpha);
    assert_eq!(callees.len(), 1, "{callees:?}");
    assert_eq!(callees[0].name, "duet");
    let dependents = plane.dependents_in_scope("sing_alpha", &alpha);
    assert!(
        dependents.contains(&"/repo/src/alpha/a.rs".to_string()),
        "scope-owned seed always present: {dependents:?}"
    );
    // A scope naming no definition reads as empty: the engine falls
    // back to the unscoped planes, never to nothing.
    let nowhere = vec!["src/nope".to_string()];
    assert!(plane
        .callers_in_scope("duet", CallerFilter::default(), &nowhere)
        .is_empty());
    assert!(plane.callees_in_scope("duet", &nowhere).is_empty());
    assert!(plane.dependents_in_scope("duet", &nowhere).is_empty());
}

#[test]
fn callees_come_from_definition_sites() {
    let db = seed_db();
    let plane = GraphPlane::hydrate(&db, &["/repo".to_string()]).expect("hydrate");
    let callees = plane.callees("runner");
    assert_eq!(callees.len(), 1, "{callees:?}");
    assert_eq!(callees[0].name, "dispatch");
    assert_eq!(callees[0].file, "/repo/src/b.rs");
    let helper_callees = plane.callees("helper");
    assert_eq!(
        helper_callees
            .iter()
            .map(|callee| callee.name.as_str())
            .collect::<Vec<_>>(),
        vec!["dispatch"],
        "same-file calls list at their definition site: {helper_callees:?}"
    );
    assert!(plane.callees("no_such_symbol").is_empty());
}

#[test]
fn dependents_cover_callers_and_definitions() {
    let db = seed_db();
    let plane = GraphPlane::hydrate(&db, &["/repo".to_string()]).expect("hydrate");
    let dependents = plane.dependents("dispatch");
    assert!(
        dependents.contains(&"/repo/src/a.rs".to_string()),
        "{dependents:?}"
    );
    assert!(
        dependents.contains(&"/repo/src/b.rs".to_string()),
        "{dependents:?}"
    );
    assert!(plane.dependents("no_such_symbol").is_empty());
}

#[test]
fn owner_comes_from_the_component_gate() {
    let db = seed_db();
    let plane = GraphPlane::hydrate(&db, &["/repo".to_string()]).expect("hydrate");
    let owner = plane.owner("src/guidance/src/read/planes/graph.rs");
    assert_eq!(owner.package, "guidance-core", "{owner:?}");
    assert_eq!(plane.owner("src/unknown-thing/x.rs").package, "unknown");
}
