//! Directory-driven triage pins: discovered files carry scannable
//! identity, version skew never refuses linkage, the fixture substitute
//! stays quarantined, the quarantined alias stays unreferenced by runnable
//! closures, the loader version property holds, and the IntegrationLite
//! fixture closure links clean.
//!
//! Nothing here changes engine behavior: loading warns on skew and still
//! resolves, unknown imports refuse with a reason. Exact file-content
//! pins live over the checked-in fixtures only — never over the volatile
//! live root, where adding, removing, or renaming a script must change
//! no outcome.
//!
//! Test path note: this file rides the `pine::closure` unit module, so the
//! runnable filter is `pine::closure`, not the file stem.

use crate::pine::test_fixture as fixture;
use crate::{
    closure_inputs, parse_pine_source, parse_program, resolve_imports, ClosureOutcome, KernelError,
    ModuleGraph, ModuleId, ModuleInput, SourceKind, SUPPORTED_PINE_VERSION,
};
use fluent_wvr::validate::Validate;

fn fixture_text(relpath: &str) -> String {
    std::fs::read_to_string(fixture::fixture_root().join(relpath))
        .unwrap_or_else(|err| panic!("fixture file {relpath} reads: {err}"))
}

fn module_key(publisher: &str, name: &str) -> ModuleId {
    ModuleId {
        publisher: publisher.to_owned(),
        name: name.to_owned(),
    }
}

/// One discovered closure as graph inputs, by fixture stem.
fn fixture_inputs(stem: &str) -> Vec<ModuleInput> {
    let closure = fixture::fixture_outcomes()
        .into_iter()
        .find_map(|outcome| match outcome {
            ClosureOutcome::Linked(closure) if closure.stem == stem => Some(closure),
            ClosureOutcome::Linked(_) => None,
            ClosureOutcome::Refused { stem, reason, .. } => {
                panic!("fixture closure {stem} links: {reason}")
            }
        })
        .unwrap_or_else(|| panic!("fixture closure {stem} discovered"));
    closure_inputs(&closure).unwrap_or_else(|err| panic!("fixture closure {stem} parses: {err}"))
}

fn link_inputs(modules: Vec<ModuleInput>) -> Result<ModuleGraph, KernelError> {
    resolve_imports(modules)
}

#[test]
fn discovered_files_carry_scannable_identity() {
    // Every discovered file — fixture and live — either scans with a sane
    // version/warning shape or refuses with a reason. Indicators scan as
    // indicators, libraries declare their names.
    let mut outcomes = fixture::fixture_outcomes();
    outcomes.extend(fixture::discovered_outcomes());
    assert!(!outcomes.is_empty(), "the roots hold indicators");
    for outcome in &outcomes {
        match outcome {
            ClosureOutcome::Linked(closure) => {
                for (key, source) in &closure.sources {
                    match source.version {
                        Some(version) => assert!(
                            (1..=12).contains(&version),
                            "{key} declares a sane version: {version}"
                        ),
                        None => {
                            assert!(!source.warnings.is_empty(), "{key} without a header warns");
                        }
                    }
                    assert!(
                        source.warnings.len() <= 1,
                        "{key} warns at most once: {:?}",
                        source.warnings
                    );
                }
                let (_, root) = closure.sources.last().expect("root is last");
                assert_eq!(
                    root.kind,
                    SourceKind::Indicator,
                    "{} scans as an indicator",
                    closure.stem
                );
            }
            ClosureOutcome::Refused { stem, reason, .. } => {
                assert!(!reason.is_empty(), "{stem} refuses with a reason");
            }
        }
    }
}

#[test]
fn version_skew_never_refuses_linkage() {
    // Linking matches on publisher/name only: every linkable closure —
    // fixture and live, whatever versions its import lines pin — resolves.
    // Import *names* are structural (resolution depends on them) and stay
    // pinned over the hermetic fixture; versions are metadata and asserted
    // nowhere.
    let mut outcomes = fixture::fixture_outcomes();
    outcomes.extend(fixture::discovered_outcomes());
    let mut linked_any = false;
    for outcome in &outcomes {
        let ClosureOutcome::Linked(closure) = outcome else {
            continue;
        };
        linked_any = true;
        let inputs =
            closure_inputs(closure).unwrap_or_else(|err| panic!("{} parses: {err}", closure.stem));
        link_inputs(inputs)
            .unwrap_or_else(|err| panic!("{} links despite skew: {err}", closure.stem));
    }
    assert!(linked_any, "at least one closure links");
    // Fixture DVP import names (structural: the link above depends on them).
    let dvp = fixture::fixture_outcomes()
        .into_iter()
        .find_map(|outcome| match outcome {
            ClosureOutcome::Linked(closure) if closure.stem == "_DeltaVolumeProfileProtopyne" => {
                Some(closure)
            }
            _ => None,
        })
        .expect("fixture DVP closure discovered");
    let (_, root) = dvp.sources.last().expect("root is last");
    let names: Vec<&str> = root
        .imports
        .iter()
        .map(|decl| decl.library.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["libUtil", "libBuffers", "libApex", "libDrawing"],
        "DVP import names drift loudly"
    );
}

#[test]
fn fixture_regressions_stays_quarantined() {
    // The fixture substitute imports libraries absent from the fixture
    // tree, and its statement-level `simple` qualifier (used nowhere in
    // scope) stops the parser fail-closed — so the substitute is
    // quarantined at every level (no file for its imports, no parse, no
    // link, no execution), and any drift of these pins fails loudly
    // instead of silently entering proof.
    let text = fixture_text("indicators/_Regressions.pine");
    let source = parse_pine_source("indicators/_Regressions.pine", &text);
    let names: Vec<&str> = source
        .imports
        .iter()
        .map(|decl| decl.library.name.as_str())
        .collect();
    assert!(
        names.contains(&"libTrendConsensus"),
        "substitute still pins the quarantined import: {names:?}"
    );
    assert!(
        names.contains(&"libPivots"),
        "substitute still pins the second absent import: {names:?}"
    );
    for missing in [
        "libraries/_libTrendConsensus.pine",
        "libraries/_libPivots.pine",
    ] {
        assert!(
            !fixture::fixture_root().join(missing).exists(),
            "quarantined library stays absent from the fixture tree"
        );
    }
    let err = parse_program(&text).expect_err("substitute refuses to parse");
    let reason = err.to_string();
    assert!(
        reason.contains("simple"),
        "parse refusal stays literal (statement-level `simple`): {reason}"
    );
    // Discovery refuses it with an unknown-import reason, never silence.
    let refused = fixture::fixture_outcomes()
        .into_iter()
        .find_map(|outcome| match outcome {
            ClosureOutcome::Refused { stem, reason, .. } if stem == "_Regressions" => Some(reason),
            _ => None,
        })
        .expect("fixture Regressions is refused");
    assert!(
        refused.contains("libTrendConsensus") || refused.contains("libPivots"),
        "discovery names the absent library: {refused}"
    );
}

#[test]
fn quarantined_alias_stays_unreferenced_by_runnable_closures() {
    // The quarantined alias has zero item references in any linkable
    // closure — fixture or live — which is why no runnable closure links
    // against a refusal. The quarantined fixture substitute keeps live
    // references (its imports are not dead): that count pins the
    // substitute's shape over hermetic bytes.
    let mut outcomes = fixture::fixture_outcomes();
    outcomes.extend(fixture::discovered_outcomes());
    for outcome in &outcomes {
        let ClosureOutcome::Linked(closure) = outcome else {
            continue;
        };
        for (key, source) in &closure.sources {
            let hits = source.text.matches("libTrendConsensus.").count();
            assert_eq!(hits, 0, "{key} must stay free of the alias");
        }
    }
    let regressions = fixture_text("indicators/_Regressions.pine");
    let hits = regressions.matches("libTrendConsensus.").count();
    assert!(
        hits > 0,
        "substitute keeps live references (import is not dead): got {hits}"
    );
}

#[test]
fn loader_version_property() {
    // Exhaustive over the small input space (stronger than sampling):
    // version skew warns exactly once and still validates; only empty
    // input refuses, via validation.
    for version in 1..=12u64 {
        let text = format!("//@version={version}\nindicator('probe')\n");
        let source = parse_pine_source("probe.pine", &text);
        assert_eq!(source.version, Some(version));
        if version == SUPPORTED_PINE_VERSION {
            assert!(
                source.warnings.is_empty(),
                "supported version warns nothing"
            );
        } else {
            assert_eq!(source.warnings.len(), 1, "skew warns exactly once");
            assert!(
                source.warnings[0].message.contains(&version.to_string()),
                "warning names the declared version"
            );
        }
        source.validate().expect("skewed source still validates");
    }
    let missing = parse_pine_source("probe.pine", "indicator('probe')\n");
    assert_eq!(missing.version, None);
    assert_eq!(missing.warnings.len(), 1, "missing header warns once");
    missing
        .validate()
        .expect("headerless source still validates");
    let empty = parse_pine_source("empty.pine", "  \n");
    empty
        .validate()
        .expect_err("only empty input refuses validation");
    let absent = super::closure_data_root(env!("CARGO_MANIFEST_DIR"))
        .join("indicators/_no_such_indicator.pine");
    crate::load_pine_source(&absent).expect_err("unreadable path refuses with a reason");
}

#[test]
fn integration_lite_fixture_links_clean() {
    // Control group that must NOT refuse: the fixture closure (seven
    // libraries plus the indicator root) loads and links exactly as before.
    let modules = fixture_inputs("_IntegrationLite");
    assert_eq!(modules.len(), 8, "seven libraries plus the indicator");
    let graph = link_inputs(modules).expect("fixture closure links clean");
    assert_eq!(graph.order().len(), 8);
    assert!(graph.warnings().is_empty());
    assert!(
        graph
            .order()
            .contains(&module_key("local", "_IntegrationLite")),
        "the indicator root is ordered: {:?}",
        graph.order()
    );
}
