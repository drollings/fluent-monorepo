use super::*;

#[test]
fn class_entry_single_home() {
    let taxonomy_entry = crate::taxonomy::TaxonomyEntry {
        parents: vec!["a".to_string()],
        ancestors: vec!["a".to_string()],
    };
    assert_eq!(taxonomy_entry.parents, vec!["a".to_string()]);
    let registry_entry = crate::yago_loader::RegistryEntry {
        iri: "http://example.org/A".to_string(),
        label: "A".to_string(),
        superclass: None,
    };
    assert_eq!(registry_entry.label, "A");
}

#[test]
fn entity_control_group_silent() {
    for text in [
        "Monday Tuesday Wednesday",
        "January February March",
        "The meeting is now",
        "This is just some text",
    ] {
        let entities = extract_entities(text, 2);
        assert!(
            entities.is_empty(),
            "control group must be silent for {text:?}: {entities:?}"
        );
    }
}

#[test]
fn entity_must_extract_recall() {
    let text = "Alice met Bob at Googleplex and Alice showed Bob Googleplex maps and Alice left";
    let entities = extract_entities(text, 2);
    for name in ["Alice", "Bob", "Googleplex"] {
        assert!(
            entities.iter().any(|e| e.name == name),
            "must extract {name:?}: {entities:?}"
        );
    }
}

#[test]
fn normalize_determinism() {
    assert_eq!(
        crate::normalize::normalize_yago_name("yago:Remix__U0028_Work_U0029"),
        crate::normalize::normalize_yago_name("yago:Remix__U0028_Work_U0029")
    );
}

#[test]
fn entity_calibration_precision_recall() {
    let must_extract =
        "Alice met Bob at Googleplex and Alice showed Bob Googleplex maps and Alice left";
    let hits = extract_entities(must_extract, 2);
    let recall_hits = ["Alice", "Bob", "Googleplex"]
        .iter()
        .filter(|n| hits.iter().any(|e| &e.name == *n))
        .count();
    let recall = recall_hits as f64 / 3.0;
    assert!(
        (recall - 1.0).abs() < f64::EPSILON,
        "recall at min_frequency 2"
    );

    let mut false_positives = 0;
    for text in ["Monday Monday Tuesday Tuesday", "The The This This"] {
        false_positives += extract_entities(text, 2).len();
        false_positives += extract_entities(text, 3).len();
    }
    assert_eq!(
        false_positives, 0,
        "stoplist must hold at frequency 2 and 3"
    );
}
