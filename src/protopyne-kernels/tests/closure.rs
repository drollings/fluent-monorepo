//! The shipping closure: what a default build carries.
//!
//! `wq_series` (the `ts_*` window vocabulary) and `wq_half_life` (the
//! half-life weights) have no consumer in the shipping library — they are the
//! alpha/options-activity vocabulary, kept behind `research`. Both are `pub`
//! modules, so their presence in a default build raises no dead-code warning;
//! the gate is the only thing keeping them out, and this pin fails the ordinary
//! suite the moment the gate is dropped.
//!
//! The build-matrix gate (`bin/protopyne-build-matrix.sh`) proves the same
//! property on the compiled artifact — the lean rlib's symbol table names
//! neither module — so a gate that is written but ineffective is caught too.

fn lib_source() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"))
        .expect("lib.rs is readable")
}

#[test]
fn the_research_only_wq_vocabulary_is_gated_out_of_the_default_build() {
    let source = lib_source();
    for module in ["wq_half_life", "wq_series"] {
        let gated_module = format!("#[cfg(feature = \"research\")]\npub mod {module};");
        assert!(
            source.contains(&gated_module),
            "{module} must be declared behind the research feature"
        );
        let gated_use = format!("#[cfg(feature = \"research\")]\npub use {module}::");
        assert!(
            source.contains(&gated_use),
            "{module}'s re-exports must be behind the research feature"
        );
    }
    // `wq_comb` stays in the shipping build: the pine core evaluates its scalar
    // math through it, so it is wired even with every feature off.
    assert!(
        source.contains("pub mod wq_comb;"),
        "wq_comb is a shipping module, not research-only"
    );
}

/// The same vocabulary is whole under `research`: the gate hides it from the
/// default build, it does not amputate it.
#[cfg(feature = "research")]
#[test]
fn the_research_build_carries_the_wq_vocabulary() {
    assert_eq!(
        crate::wq_series::ts_sum(&[1.0, 2.0, 3.0], 2),
        vec![None, Some(3.0), Some(5.0)]
    );
    let weights = crate::wq_half_life::hl_weights(4, 1);
    assert_eq!(weights.len(), 4);
    assert!(
        (weights[3] - 1.0).abs() < f64::EPSILON,
        "the newest bar weighs 1"
    );
}
