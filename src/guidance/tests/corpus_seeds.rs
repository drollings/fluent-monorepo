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
        ("corpora/support_seed.json", 1.0),
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

#[test]
fn doc_admission_seed_shape_loads_tune_cases() {
    // Briefs-and-cases shape (shared with the capability seed): every
    // relevant id names a brief, controls admit nothing.
    let raw = std::fs::read_to_string(std::path::Path::new("corpora/doc_admission_seed.json"))
        .expect("seed reads");
    let value: serde_json::Value = serde_json::from_str(&raw).expect("seed parses");
    let briefs = value
        .get("briefs")
        .and_then(|ids| ids.as_array())
        .expect("briefs");
    let cases = value
        .get("cases")
        .and_then(|cases| cases.as_array())
        .expect("cases");
    assert!(!briefs.is_empty() && !cases.is_empty());
    let ids: std::collections::HashSet<&str> = briefs
        .iter()
        .filter_map(|brief| brief.get("id").and_then(|id| id.as_str()))
        .collect();
    assert_eq!(ids.len(), briefs.len(), "brief ids unique");
    for case in cases {
        for id in case
            .get("relevant")
            .and_then(|relevant| relevant.as_array())
            .expect("relevant")
        {
            let id = id.as_str().expect("id string");
            assert!(ids.contains(id), "relevant {id} names a brief");
        }
    }
    assert!(
        cases.iter().any(|case| case
            .get("control")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)),
        "seed carries controls"
    );
}
