//! Task-indicator closures: each of the three indicators under proof parses,
//! links against the single on-disk library set, and survives
//! parse-then-display structurally; `libBuffers` links as a module without
//! any of its items executing; and a non-`export`ed helper stays invisible,
//! pinning the anti-concatenation guarantee for the new closures.
//!
//! Nothing here executes indicator code: there is no engine in this suite,
//! only loading, parsing, linking, and rendering. Statement/export pins fail
//! loudly on file drift.
//!
//! Test path note: this file rides the `pine::closure` unit module, so the
//! runnable filter is `pine::closure`, not the file stem.

use super::*;
use crate::{
    closure_data_root, display_program, load_listed_closure, parse_program, resolve_imports,
    ClosureEntry, ModuleGraph, TASK_INDICATORS,
};

use crate::pine::test_fixture as fixture;

use fixture::{library_id, module_key};

/// Load, parse, and link one task indicator with the on-disk libraries.
fn link_task(indicator: &ClosureEntry) -> ModuleGraph {
    let root = closure_data_root(env!("CARGO_MANIFEST_DIR"));
    let sources =
        load_listed_closure(&root, REAL_LIBRARIES, indicator).expect("task closure loads");
    let modules: Vec<crate::ModuleInput> = sources
        .into_iter()
        .map(|(key, source)| {
            let program = parse_program(&source.text)
                .unwrap_or_else(|err| panic!("closure file {key} parses: {err}"));
            let (publisher, name) = key.split_once('/').expect("key shape");
            let version = if publisher == "local" {
                indicator.version
            } else {
                REAL_LIBRARIES
                    .iter()
                    .find(|entry| entry.publisher == publisher && entry.name == name)
                    .expect("listed library")
                    .version
            };
            crate::ModuleInput::new(library_id(publisher, name, version), source, program)
        })
        .collect();
    resolve_imports(modules).expect("task closure links")
}

#[test]
fn task_table_lists_three_indicators_beside_frozen_closure() {
    assert_eq!(TASK_INDICATORS.len(), 3, "DVP, PPAS, RSIMD under proof");
    for entry in TASK_INDICATORS {
        assert_eq!(
            entry.publisher, "local",
            "indicator root travels under local"
        );
        assert_eq!(entry.version, 6, "indicator declares v6");
    }
    // The frozen closure is untouched by the new table.
    assert_eq!(REAL_LIBRARIES.len(), 7);
    assert_eq!(INDICATOR_PATH, "indicators/_IntegrationLite.pine");
}

#[test]
fn task_indicators_parse_and_link() {
    for indicator in TASK_INDICATORS {
        let graph = link_task(indicator);
        let root = module_key("local", indicator.name);
        assert!(
            graph.order().contains(&root),
            "{} root is ordered: {:?}",
            indicator.name,
            graph.order()
        );
        for lib in REAL_LIBRARIES {
            assert!(
                graph.order().contains(&lib.module_id()),
                "{} links {}/{}",
                indicator.name,
                lib.publisher,
                lib.name
            );
        }
        assert!(
            graph.warnings().is_empty(),
            "{} warns nothing: {:?}",
            indicator.name,
            graph.warnings()
        );
        // Import lines on disk still match the table entry, in file order.
        let resolved = graph.get(&root).expect("root resolves");
        let imports: Vec<(&str, &str)> = resolved
            .source
            .imports
            .iter()
            .map(|decl| (decl.library.name.as_str(), decl.alias.as_str()))
            .collect();
        let pinned: Vec<(&str, &str)> = indicator
            .imports
            .iter()
            .map(|(name, alias)| (*name, *alias))
            .collect();
        assert_eq!(
            imports, pinned,
            "{} import lines match the table",
            indicator.name
        );
    }
}

#[test]
fn task_indicator_shapes_are_pinned() {
    // (indicator stem, top-level statements, exports in source order).
    // Indicators export nothing; the graph carries that emptiness too.
    let cases: Vec<(&str, usize, Vec<String>)> = vec![
        ("DeltaVolumeProfileProtopyne", 142, vec![]),
        ("ApexPPAS", 67, vec![]),
        ("ApexRSIMD", 90, vec![]),
    ];
    for (stem, statements, exports) in cases {
        let entry = TASK_INDICATORS
            .iter()
            .find(|entry| entry.name == stem)
            .expect("task entry known");
        let root = closure_data_root(env!("CARGO_MANIFEST_DIR"));
        let sources =
            load_listed_closure(&root, REAL_LIBRARIES, entry).expect("task closure loads");
        let (_, source) = sources.last().expect("indicator is last");
        let program =
            parse_program(&source.text).unwrap_or_else(|err| panic!("{stem} parses: {err}"));
        assert_eq!(
            program.statements.len(),
            statements,
            "{stem} statement count"
        );
        let graph = link_task(entry);
        let root_id = module_key("local", stem);
        assert_eq!(
            graph.exports_of(&root_id).unwrap_or(&[]),
            exports.as_slice(),
            "{stem} export table"
        );
    }
}

#[test]
fn display_round_trips_task_closures() {
    // Parse-then-display is structurally stable for every file in every
    // task closure (libraries shared, indicators each).
    let root = closure_data_root(env!("CARGO_MANIFEST_DIR"));
    for indicator in TASK_INDICATORS {
        let sources =
            load_listed_closure(&root, REAL_LIBRARIES, indicator).expect("task closure loads");
        for (key, source) in &sources {
            let program = parse_program(&source.text)
                .unwrap_or_else(|err| panic!("closure file {key} parses: {err}"));
            let displayed = display_program(&program);
            let reparsed = parse_program(&displayed)
                .unwrap_or_else(|err| panic!("displayed {key} re-parses: {err}"));
            assert_eq!(reparsed, program, "{key} survives parse-then-display");
        }
    }
}

#[test]
fn libbuffers_links_without_evaluation() {
    // The dependency loads and links as a module; its methods are not
    // executed — this suite constructs no engine, so linkage is all it
    // observes. Method visibility binds through the alias without running.
    let graph = link_task(&TASK_INDICATORS[0]);
    let buffers = module_key("showmethegrail", "libBuffers");
    let exports = graph.exports_of(&buffers).expect("buffers linked");
    assert!(
        exports.contains(&"BufferFloat".to_owned()),
        "BufferFloat visible: {exports:?}"
    );
    assert!(
        exports.contains(&"BufferInt".to_owned()),
        "BufferInt visible: {exports:?}"
    );
    let dvp = module_key("local", "DeltaVolumeProfileProtopyne");
    let target = graph
        .resolve_reference(&dvp, "libBuffers", "push")
        .expect("libBuffers.push binds");
    assert_eq!(target, &buffers);
}

#[test]
fn task_closure_private_helper_is_invisible() {
    // The anti-concatenation guarantee holds for the new closures exactly as
    // for the frozen one: only `export`-wrapped items cross modules.
    let graph = link_task(&TASK_INDICATORS[0]);
    let apex = module_key("showmethegrail", "libApex");
    graph
        .resolve_reference(&apex, "BARS", "get")
        .expect("exported BARS.get binds");
    let err = graph
        .resolve_reference(&apex, "BARS", "_getPhysicalIndex")
        .expect_err("private helper is not importable");
    let reason = err.to_string();
    assert!(
        reason.contains("_getPhysicalIndex") && reason.contains("is not exported by"),
        "refusal names the hidden item literally: {reason}"
    );
}
