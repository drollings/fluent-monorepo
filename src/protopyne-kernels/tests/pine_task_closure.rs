//! Task-indicator closures: every discovered indicator links against the
//! resolved library set and survives parse-then-display structurally;
//! `libBuffers` links as a module without any of its items executing; and
//! a non-`export`ed helper stays invisible, pinning the anti-concatenation
//! guarantee for the discovered closures.
//!
//! Nothing here executes indicator code: there is no engine in this suite,
//! only loading, parsing, linking, and rendering. Statement/export pins
//! over the checked-in fixtures fail loudly on fixture drift; probes over
//! the live data root assert properties only, so adding, removing, or
//! renaming a shipped script changes no outcome here.
//!
//! Test path note: this file rides the `pine::closure` unit module, so the
//! runnable filter is `pine::closure`, not the file stem.

use crate::pine::test_fixture as fixture;
use crate::{
    closure_inputs, display_program, parse_program, resolve_imports, ClosureOutcome,
    DiscoveredClosure, ModuleGraph,
};

use fixture::module_key;

/// Load, parse, and link one discovered closure.
fn link_discovered(closure: &DiscoveredClosure) -> ModuleGraph {
    let inputs = closure_inputs(closure)
        .unwrap_or_else(|err| panic!("closure {} parses: {err}", closure.stem));
    resolve_imports(inputs).unwrap_or_else(|err| panic!("closure {} links: {err}", closure.stem))
}

/// Every linked outcome carries a root-last source list with well-formed
/// `publisher/name` keys.
fn assert_source_shape(closure: &DiscoveredClosure) {
    assert!(
        !closure.sources.is_empty(),
        "{} carries sources",
        closure.stem
    );
    let (last_key, _) = closure.sources.last().expect("nonempty");
    assert_eq!(
        last_key,
        &format!("local/{}", closure.stem),
        "{} root travels under its stem",
        closure.stem
    );
    for (key, _) in &closure.sources {
        assert!(
            key.split_once('/')
                .is_some_and(|(publisher, name)| { !publisher.is_empty() && !name.is_empty() }),
            "{key} is a publisher/name key",
        );
    }
}

#[test]
fn discovery_lists_every_fixture_indicator_with_a_verdict() {
    // The fixture set is fixed, so its verdicts are pinned: the runnable
    // fixture indicators link, and the quarantine fixture refuses naming
    // its absent library. Fixture bytes are checked in, so drift fails
    // loudly here — never over the volatile live root.
    let outcomes = fixture::fixture_outcomes();
    let mut linked = Vec::new();
    let mut refused = Vec::new();
    for outcome in &outcomes {
        match outcome {
            ClosureOutcome::Linked(closure) => {
                assert_source_shape(closure);
                linked.push(closure.stem.as_str());
            }
            ClosureOutcome::Refused { stem, reason, .. } => refused.push((stem.as_str(), reason)),
        }
    }
    linked.sort_unstable();
    assert_eq!(
        linked,
        vec![
            "_DeltaVolumeProfileProtopyne",
            "_FastMAs",
            "_IntegrationLite"
        ],
        "the runnable fixtures link"
    );
    assert_eq!(refused.len(), 1, "exactly one fixture refuses: {refused:?}");
    assert_eq!(
        refused[0].0, "_Regressions",
        "the quarantine fixture refuses"
    );
    assert!(
        refused[0].1.contains("libPivots") || refused[0].1.contains("libTrendConsensus"),
        "the refusal names the absent library: {}",
        refused[0].1
    );
}

#[test]
fn discovery_covers_the_live_root_file_for_file() {
    // Directory-driven: one outcome per file on disk, whatever is there.
    // Adding, removing, or renaming a script changes only which per-file
    // rows this probe produces, never the verdict shape.
    let live = fixture::discovered_outcomes();
    let (files, _) =
        crate::manifest_files::load_pine_sources(&fixture::data_root()).expect("live root loads");
    assert_eq!(
        live.len(),
        files.len(),
        "one verdict per indicator file on disk"
    );
    for outcome in &live {
        match outcome {
            ClosureOutcome::Linked(closure) => assert_source_shape(closure),
            ClosureOutcome::Refused {
                stem,
                relpath,
                reason,
                ..
            } => {
                assert!(!stem.is_empty(), "a refusal names its stem");
                assert!(
                    relpath.ends_with(".pine"),
                    "a refusal names its path: {relpath}"
                );
                assert!(!reason.is_empty(), "{stem} refuses with a reason");
            }
        }
    }
}

#[test]
fn discovered_indicators_parse_and_link() {
    // Every linkable closure — fixture and live — parses, orders its root,
    // and resolves every declared import to a loaded module. Version skew
    // never refuses: warnings are metadata, linkage matches on
    // publisher/name only.
    let mut outcomes = fixture::fixture_outcomes();
    outcomes.extend(fixture::discovered_outcomes());
    let mut linked_any = false;
    for outcome in &outcomes {
        let ClosureOutcome::Linked(closure) = outcome else {
            continue;
        };
        linked_any = true;
        let graph = link_discovered(closure);
        let root = module_key("local", &closure.stem);
        assert!(
            graph.order().contains(&root),
            "{} root is ordered: {:?}",
            closure.stem,
            graph.order()
        );
        let resolved = graph.get(&root).expect("root resolves");
        for decl in &resolved.source.imports {
            let target = module_key(&decl.library.publisher, &decl.library.name);
            assert!(
                graph.order().contains(&target),
                "{} import {} resolves to a loaded module",
                closure.stem,
                decl.library,
            );
        }
    }
    assert!(linked_any, "at least one closure links");
}

#[test]
fn fixture_indicator_shapes_are_pinned() {
    // (fixture stem, top-level statements, exports in source order).
    // Indicators export nothing; the graph carries that emptiness too.
    // Fixture bytes are checked in, so drift fails loudly here — never
    // over the volatile live root.
    let cases: Vec<(&str, usize, Vec<String>)> = vec![
        ("_IntegrationLite", 136, vec![]),
        ("_DeltaVolumeProfileProtopyne", 142, vec![]),
    ];
    let outcomes = fixture::fixture_outcomes();
    for (stem, statements, exports) in cases {
        let closure = outcomes
            .iter()
            .find_map(|outcome| match outcome {
                ClosureOutcome::Linked(closure) if closure.stem == stem => Some(closure),
                _ => None,
            })
            .unwrap_or_else(|| panic!("fixture closure {stem} discovered"));
        let (_, source) = closure.sources.last().expect("root is last");
        let program =
            parse_program(&source.text).unwrap_or_else(|err| panic!("{stem} parses: {err}"));
        assert_eq!(
            program.statements.len(),
            statements,
            "{stem} statement count"
        );
        let graph = link_discovered(closure);
        let root_id = module_key("local", stem);
        assert_eq!(
            graph.exports_of(&root_id).unwrap_or(&[]),
            exports.as_slice(),
            "{stem} export table"
        );
    }
}

#[test]
fn display_round_trips_fixture_closures() {
    // Parse-then-display is structurally stable for every file of every
    // linkable fixture closure. Fixture bytes are checked in, so the exact
    // round-trip pins here; live scripts get the link-or-refuse verdict
    // instead, because display formatting over arbitrary third-party text
    // is not a churn-proof property.
    let fixture_outcomes = fixture::fixture_outcomes();
    let mut closures = Vec::new();
    for outcome in &fixture_outcomes {
        if let ClosureOutcome::Linked(closure) = outcome {
            closures.push(closure);
        }
    }
    assert!(!closures.is_empty(), "at least one closure round-trips");
    for closure in closures {
        for (key, source) in &closure.sources {
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
    let outcomes = fixture::fixture_outcomes();
    let dvp = outcomes
        .iter()
        .find_map(|outcome| match outcome {
            ClosureOutcome::Linked(closure) if closure.stem == "_DeltaVolumeProfileProtopyne" => {
                Some(closure)
            }
            _ => None,
        })
        .expect("fixture DVP closure discovered");
    let graph = link_discovered(dvp);
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
    let dvp_key = module_key("local", "_DeltaVolumeProfileProtopyne");
    let target = graph
        .resolve_reference(&dvp_key, "libBuffers", "push")
        .expect("libBuffers.push binds");
    assert_eq!(target, &buffers);
}

#[test]
fn task_closure_private_helper_is_invisible() {
    // The anti-concatenation guarantee holds for the discovered closures
    // exactly as for the frozen one: only `export`-wrapped items cross
    // modules.
    let outcomes = fixture::fixture_outcomes();
    let lite = outcomes
        .iter()
        .find_map(|outcome| match outcome {
            ClosureOutcome::Linked(closure) if closure.stem == "_IntegrationLite" => Some(closure),
            _ => None,
        })
        .expect("fixture IntegrationLite closure discovered");
    let graph = link_discovered(lite);
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
