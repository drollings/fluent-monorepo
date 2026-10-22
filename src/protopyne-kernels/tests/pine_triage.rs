//! Task-indicator triage pins: canonical file homes, the absent RSIMD and
//! Visuals files, the Regressions substitute quarantine, the version-skew
//! table, the zero-reference scan, the IntegrationLite control, and the
//! loader version property.
//!
//! Every assertion pins observed file truth so drift fails loudly. Nothing
//! here changes engine behavior: loading warns on skew and still resolves,
//! unknown imports refuse with a reason.
//!
//! Test path note: this file rides the `pine::closure` unit module, so the
//! runnable filter is `pine::closure`, not the file stem.

use super::*;
use crate::{
    closure_data_root, load_closure_sources, load_pine_source, parse_pine_source, parse_program,
    resolve_imports, KernelError, LibraryId, ModuleGraph, ModuleId, ModuleInput, SourceKind,
    SUPPORTED_PINE_VERSION,
};
use fluent_wvr::validate::Validate;
use std::path::PathBuf;

fn data_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs")
}

fn load_rel(relpath: &str) -> crate::PineSource {
    load_pine_source(&data_root().join(relpath))
        .unwrap_or_else(|err| panic!("real file {relpath} loads: {err}"))
}

fn library_id(publisher: &str, name: &str, version: u64) -> LibraryId {
    LibraryId {
        publisher: publisher.to_owned(),
        name: name.to_owned(),
        version,
    }
}

fn module_key(publisher: &str, name: &str) -> ModuleId {
    ModuleId {
        publisher: publisher.to_owned(),
        name: name.to_owned(),
    }
}

/// Load one real file as a graph input over its byte-identical text.
fn real_module(id: LibraryId, relpath: &str) -> ModuleInput {
    let source = load_rel(relpath);
    let program = parse_program(&source.text)
        .unwrap_or_else(|err| panic!("real file {relpath} parses: {err}"));
    ModuleInput::new(id, source, program)
}

/// The seven on-disk libraries as graph inputs (versions metadata-only).
fn on_disk_libraries() -> Vec<ModuleInput> {
    REAL_LIBRARIES
        .iter()
        .map(|entry| {
            real_module(
                library_id(entry.publisher, entry.name, entry.version),
                entry.relpath,
            )
        })
        .collect()
}

/// One task indicator plus the on-disk libraries, linked in import order.
fn link_task_indicator(stem: &str, relpath: &str) -> Result<ModuleGraph, KernelError> {
    let mut modules = on_disk_libraries();
    modules.push(real_module(library_id("local", stem, 6), relpath));
    resolve_imports(modules)
}

#[test]
fn task_indicator_homes_are_canonical() {
    // Both task indicators already live under the single data root; no
    // second root exists and none is introduced.
    for (relpath, kind_check) in [
        ("indicators/_DeltaVolumeProfileProtopyne.pine", "indicator"),
        ("indicators/_ApexPPAS.pine", "indicator"),
        ("indicators/_ApexRSIMD.pine", "indicator"),
        ("libraries/_libBuffers.pine", "library"),
    ] {
        let source = load_rel(relpath);
        assert_eq!(source.version, Some(6), "{relpath} declares v6");
        assert!(
            source.warnings.is_empty(),
            "{relpath} warns nothing: {:?}",
            source.warnings
        );
        match kind_check {
            "indicator" => assert_eq!(source.kind, SourceKind::Indicator),
            "library" => match &source.kind {
                SourceKind::Library { name } => assert_eq!(name, "libBuffers"),
                other => panic!("{relpath} is a library, got {other:?}"),
            },
            _ => unreachable!(),
        }
    }
}

#[test]
fn version_skew_table_pins_task_imports() {
    // Documentation-only skew table: task files pin older library versions
    // than the closure; linking matches on publisher/name only, so this
    // table can never grow a version-keyed branch.
    let dvp = load_rel("indicators/_DeltaVolumeProfileProtopyne.pine");
    let dvp_imports: Vec<(&str, u64, &str)> = dvp
        .imports
        .iter()
        .map(|decl| {
            (
                decl.library.name.as_str(),
                decl.library.version,
                decl.alias.as_str(),
            )
        })
        .collect();
    assert_eq!(
        dvp_imports,
        vec![
            ("libUtil", 12, "UTIL"),
            ("libBuffers", 6, "libBuffers"),
            ("libApex", 88, "libApex"),
            ("libDrawing", 61, "libDrawing"),
        ],
        "DVP import pins drift loudly"
    );
    let ppas = load_rel("indicators/_ApexPPAS.pine");
    let ppas_imports: Vec<(&str, u64, &str)> = ppas
        .imports
        .iter()
        .map(|decl| {
            (
                decl.library.name.as_str(),
                decl.library.version,
                decl.alias.as_str(),
            )
        })
        .collect();
    assert_eq!(
        ppas_imports,
        vec![
            ("libUtil", 6, "UTIL"),
            ("libCandles", 11, "libCandles"),
            ("libApex", 59, "libApex"),
        ],
        "PPAS import pins drift loudly (older than closure 12/21/88)"
    );
    let rsimd = load_rel("indicators/_ApexRSIMD.pine");
    let rsimd_imports: Vec<(&str, u64, &str)> = rsimd
        .imports
        .iter()
        .map(|decl| {
            (
                decl.library.name.as_str(),
                decl.library.version,
                decl.alias.as_str(),
            )
        })
        .collect();
    assert_eq!(
        rsimd_imports,
        vec![
            ("libUtil", 10, "UTIL"),
            ("libApex", 84, "libApex"),
            ("libMAs", 84, "libMAs"),
            ("libDrawing", 60, "libDrawing"),
        ],
        "RSIMD import pins drift loudly"
    );
}

#[test]
fn task_closures_link_despite_skew() {
    // Skewed pins still resolve: versions never refuse, order holds.
    for (stem, relpath) in [
        (
            "DeltaVolumeProfileProtopyne",
            "indicators/_DeltaVolumeProfileProtopyne.pine",
        ),
        ("ApexPPAS", "indicators/_ApexPPAS.pine"),
        ("ApexRSIMD", "indicators/_ApexRSIMD.pine"),
    ] {
        let graph =
            link_task_indicator(stem, relpath).unwrap_or_else(|err| panic!("{stem} links: {err}"));
        let root = module_key("local", stem);
        assert!(
            graph.order().contains(&root),
            "{stem} root is ordered: {:?}",
            graph.order()
        );
        assert!(
            graph.warnings().is_empty(),
            "{stem} warns nothing (all files declare v6): {:?}",
            graph.warnings()
        );
    }
}

#[test]
fn rsimd_supplied_and_pinned() {
    // `_ApexRSIMD.pine` was supplied during triage: it lives under the
    // single data root, declares v6, and parses. Its import pins join the
    // skew table above; no golden may claim an RSIMD name beyond what the
    // later proof milestones cover.
    let source = load_rel("indicators/_ApexRSIMD.pine");
    assert_eq!(source.version, Some(6));
    assert!(source.warnings.is_empty());
    assert_eq!(source.kind, SourceKind::Indicator);
    parse_program(&source.text).expect("supplied RSIMD parses");
}

#[test]
fn visuals_absent_regressions_quarantined() {
    // `_Visuals.pine` exists nowhere; `_Regressions.pine` is the approved
    // substitute. It imports two libraries absent from the tree, and its
    // statement-level `simple` qualifier (used nowhere in scope) stops the
    // parser fail-closed — so the substitute is quarantined at every level
    // (no file for its imports, no parse, no link, no execution), and any
    // drift of these pins fails loudly instead of silently entering proof.
    assert!(
        !data_root().join("indicators/_Visuals.pine").exists(),
        "Visuals file appeared: revisit the substitution before proving it"
    );
    let regressions = load_rel("indicators/_Regressions.pine");
    let imports: Vec<(&str, u64)> = regressions
        .imports
        .iter()
        .map(|decl| (decl.library.name.as_str(), decl.library.version))
        .collect();
    assert!(
        imports.contains(&("libTrendConsensus", 45)),
        "substitute still pins the quarantined import: {imports:?}"
    );
    assert!(
        imports.contains(&("libPivots", 9)),
        "substitute still pins the second absent import: {imports:?}"
    );
    for missing in [
        "libraries/_libTrendConsensus.pine",
        "libraries/_libPivots.pine",
    ] {
        let err = load_pine_source(&data_root().join(missing))
            .expect_err("quarantined library has no file");
        assert!(
            err.to_string().contains(missing),
            "refusal names the absent path: {err}"
        );
    }
    let err = parse_program(&regressions.text).expect_err("substitute refuses to parse");
    let reason = err.to_string();
    assert!(
        reason.contains("350:1") && reason.contains("simple"),
        "parse refusal stays literal (statement-level `simple`): {reason}"
    );
}

#[test]
fn trend_consensus_references_pinned() {
    // The quarantined alias has zero item references in every task file and
    // linked library under proof, and a live (non-dead) count in the
    // quarantined substitute — which is why the substitute links only
    // against a refusal, never for execution.
    for relpath in [
        "indicators/_DeltaVolumeProfileProtopyne.pine",
        "indicators/_ApexPPAS.pine",
        "indicators/_ApexRSIMD.pine",
        "libraries/_libBuffers.pine",
    ] {
        let source = load_rel(relpath);
        let hits = source.text.matches("libTrendConsensus.").count();
        assert_eq!(hits, 0, "{relpath} must stay free of the alias");
    }
    let regressions = load_rel("indicators/_Regressions.pine");
    let hits = regressions.text.matches("libTrendConsensus.").count();
    assert_eq!(
        hits, 12,
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
    let absent = data_root().join("libraries/_no_such_library.pine");
    load_pine_source(&absent).expect_err("unreadable path refuses with a reason");
}

#[test]
fn integration_lite_control_links_clean() {
    // Control group that must NOT refuse: the existing closure loads and
    // links exactly as before.
    let root = closure_data_root(env!("CARGO_MANIFEST_DIR"));
    let sources = load_closure_sources(&root).expect("closure loads");
    assert_eq!(sources.len(), 8, "seven libraries plus the indicator");
    let modules: Vec<ModuleInput> = sources
        .into_iter()
        .map(|(key, source)| {
            let program = parse_program(&source.text)
                .unwrap_or_else(|err| panic!("closure file {key} parses: {err}"));
            let id = if key == "local/IntegrationLite" {
                crate::indicator_library_id()
            } else {
                let (publisher, name) = key.split_once('/').expect("key shape");
                library_id(publisher, name, 1)
            };
            ModuleInput::new(id, source, program)
        })
        .collect();
    let graph = resolve_imports(modules).expect("control closure links clean");
    assert_eq!(graph.order().len(), 8);
    assert!(graph.warnings().is_empty());
}
