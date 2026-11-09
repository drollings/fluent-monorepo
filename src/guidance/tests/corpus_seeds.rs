use fluent_llm::decision::{gate_for_point_with, CalibrationRegistry, GateParams};

// Seed corpora stay loadable and passing: the files calibrations run
// from are pinned here, hermetically, before any live run.

#[test]
fn seed_corpora_load_and_pass_their_gates() {
    for (file, threshold) in [
        ("corpora/intent_seed.json", 0.9),
        ("corpora/sufficiency_seed.json", 0.9),
        ("corpora/excerpt_seed.json", 0.5),
        ("corpora/staleness_seed.json", 0.9),
    ] {
        let corpus =
            CalibrationRegistry::load_corpus(std::path::Path::new(file)).expect("load corpus");
        assert!(!corpus.cases.is_empty(), "{file}");
        assert!(
            corpus
                .cases
                .iter()
                .any(|case| case.control && !case.expected),
            "{file}: controls present"
        );
        assert!(
            gate_for_point_with(&corpus.cases, threshold, &GateParams::default()),
            "{file}: gate must pass at {threshold}"
        );
    }
}
