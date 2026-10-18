use crate::resolver::{DependencyResolver, ProviderSelection};
use crate::target::TargetRegistry;
use crate::version::{Version, VersionReq};
use common_core::interner::CapabilityRegistry;
use fluent_types::{ExecutorKind, TargetType};
use std::collections::HashMap;

fn registry_with_caps() -> (TargetRegistry, CapabilityRegistry) {
    let caps = CapabilityRegistry::new();
    let mut reg = TargetRegistry::new();
    // Capability index 0 = "bars". bars10/bars11 provide it; app needs it.
    let provides_bars = caps.to_bitvec(&["bars"]);
    let needs_bars = caps.to_bitvec(&["bars"]);
    for (id, name) in [(0i64, "bars10"), (1, "bars11"), (2, "app")] {
        let (d, p) = if name == "app" {
            (needs_bars.clone(), bitvec::vec::BitVec::new())
        } else {
            (bitvec::vec::BitVec::new(), provides_bars.clone())
        };
        reg.register(
            crate::target::Target::new()
                .id(id)
                .name(name.into())
                .target_type(TargetType::File)
                .executor(ExecutorKind::Native)
                .depends(d)
                .provides(p)
                .essential(false)
                .build(),
        )
        .unwrap();
    }
    (reg, caps)
}

fn versions(pairs: &[(&str, &str)]) -> HashMap<String, Version> {
    pairs
        .iter()
        .map(|(n, v)| ((*n).to_string(), Version::parse(v).unwrap()))
        .collect()
}

#[test]
fn version_placeholder() {
    // Scaffolding placeholder: replaced by the real version suites below.
    let _ = Version::parse("1").unwrap();
    let _ = VersionReq::parse("1").unwrap();
}

#[test]
fn exact_match_selects_pinned_provider() {
    let (reg, caps) = registry_with_caps();
    let resolver =
        DependencyResolver::with_narrowing(&reg, &caps).with_selection(ProviderSelection::All);
    let plan = resolver
        .resolve_versioned(
            &["app", "bars/11"],
            &versions(&[("bars10", "10"), ("bars11", "11")]),
        )
        .expect("exact match resolves");
    assert!(plan.target_names.contains(&"bars11".to_string()));
    assert!(!plan.target_names.contains(&"bars10".to_string()));
}

#[test]
fn compatible_range_selects_matching_provider() {
    let (reg, caps) = registry_with_caps();
    let resolver = DependencyResolver::with_narrowing(&reg, &caps);
    let plan = resolver
        .resolve_versioned(
            &["bars/>=10"],
            &versions(&[("bars10", "9"), ("bars11", "11")]),
        )
        .expect("range match resolves");
    assert!(plan.target_names.contains(&"bars11".to_string()));
    assert!(!plan.target_names.contains(&"bars10".to_string()));
}

#[test]
fn mismatch_refuses_with_actionable_reason() {
    let (reg, caps) = registry_with_caps();
    let resolver = DependencyResolver::with_narrowing(&reg, &caps);
    let err = resolver
        .resolve_versioned(
            &["bars/12"],
            &versions(&[("bars10", "10"), ("bars11", "11")]),
        )
        .expect_err("mismatch must refuse");
    let msg = err.to_string();
    assert!(msg.contains("bars"), "names the capability: {msg}");
    assert!(msg.contains("12"), "names the requested version: {msg}");
    assert!(
        msg.contains("10") && msg.contains("11"),
        "lists what exists: {msg}"
    );
}

#[test]
fn duplicate_version_is_ambiguous_with_candidates() {
    let (reg, caps) = registry_with_caps();
    let resolver = DependencyResolver::with_narrowing(&reg, &caps);
    let err = resolver
        .resolve_versioned(
            &["bars/11"],
            &versions(&[("bars10", "11"), ("bars11", "11")]),
        )
        .expect_err("duplicate versions must be ambiguous");
    match err {
        common_core::error::ResolverError::AmbiguousDependency { name, candidates } => {
            assert_eq!(name, "bars");
            assert_eq!(candidates.len(), 2);
        }
        other => panic!("expected AmbiguousDependency, got {other}"),
    }
}

#[test]
fn unversioned_provider_never_matches_a_constraint() {
    let (reg, caps) = registry_with_caps();
    let resolver = DependencyResolver::with_narrowing(&reg, &caps);
    // bars10 has no known version, so only bars11 can satisfy >=10.
    let plan = resolver
        .resolve_versioned(&["bars/>=10"], &versions(&[("bars11", "11")]))
        .expect("unversioned provider is skipped, not matched");
    assert!(plan.target_names.contains(&"bars11".to_string()));
    // ...and when nothing is versioned, the import refuses instead of
    // silently running unknown code.
    let err = resolver
        .resolve_versioned(&["bars/>=10"], &versions(&[]))
        .expect_err("all-unversioned must refuse");
    assert!(err.to_string().contains("bars"));
}

#[test]
fn unconstrained_seeds_behave_byte_identically() {
    let (reg, caps) = registry_with_caps();
    // `All` includes every provider deterministically, so the plan is
    // comparable; `NarrowOne` would refuse this registry as ambiguous.
    let resolver =
        DependencyResolver::with_narrowing(&reg, &caps).with_selection(ProviderSelection::All);
    let plain = resolver.resolve(&["app"]).expect("plain resolve");
    let versioned = resolver
        .resolve_versioned(&["app"], &versions(&[]))
        .expect("unconstrained versioned resolve");
    assert_eq!(plain.order, versioned.order);
    assert_eq!(plain.target_names, versioned.target_names);
}
