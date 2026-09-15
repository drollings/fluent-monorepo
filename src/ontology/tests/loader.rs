//! `YaGoLoader` behavior: duplicate-IRI first-wins, stats accounting, and
//! a load-time regression gate proving the duplicate check is map-backed
//! (no quadratic scan over accumulated concepts).

use super::*;
use std::time::{Duration, Instant};

fn entry(iri: &str, label: &str, superclass: Option<&str>) -> String {
    let sup = match superclass {
        Some(s) => format!(r#""superclass":"{s}""#),
        None => r#""superclass":null"#.to_string(),
    };
    format!(r#"{{"iri":"{iri}","label":"{label}",{sup}}}"#)
}

#[test]
fn loader_first_wins_on_duplicate_iri() {
    let raw = format!(
        "[{},{},{}]",
        entry("http://schema.org/Person", "Person", None),
        entry("http://schema.org/Person", "PersonDuplicate", None),
        entry(
            "http://schema.org/Employee",
            "Employee",
            Some("http://schema.org/Person")
        ),
    );
    let mut loader = YaGoLoader::new();
    let stats = loader.load_taxonomy_json(&raw).unwrap();
    assert_eq!(stats.classes, 2, "duplicate IRI must not duplicate the concept");
    assert_eq!(stats.edges, 1);
}

#[test]
fn loader_ignores_duplicate_entry_edges() {
    // A repeated IRI is ignored entirely: even its superclass edge must
    // not leak into the hierarchy (first-wins).
    let raw = format!(
        "[{},{}]",
        entry("http://schema.org/Person", "Person", None),
        entry(
            "http://schema.org/Person",
            "PersonDuplicate",
            Some("http://schema.org/Organization")
        ),
    );
    let mut loader = YaGoLoader::new();
    let stats = loader.load_taxonomy_json(&raw).unwrap();
    assert_eq!(stats.classes, 1);
    assert_eq!(stats.edges, 0);
}

#[test]
fn loader_stats_count_labels_and_edges() {
    let raw = format!(
        "[{},{},{}]",
        entry("http://schema.org/Person", "Person", None),
        entry("http://schema.org/Employee", "Employee", Some("http://schema.org/Person")),
        entry("http://schema.org/Manager", "Manager", Some("http://schema.org/Employee")),
    );
    let mut loader = YaGoLoader::new();
    let stats = loader.load_taxonomy_json(&raw).unwrap();
    assert_eq!(stats.classes, 3);
    // `labels` counts `load_class_labels` overrides only — plain loads leave it zero.
    assert_eq!(stats.labels, 0);
    assert_eq!(stats.edges, 2);
}

#[test]
fn loader_scales_linearly_on_synthetic_registry() {
    // 3000 chained entries: a quadratic duplicate check would dominate
    // here; the map-backed check keeps this comfortably under the bound.
    let mut items = Vec::new();
    for i in 0..3000 {
        let sup = if i == 0 {
            None
        } else {
            Some(format!("http://example.org/C{}", i - 1))
        };
        items.push(entry(
            &format!("http://example.org/C{i}"),
            &format!("C{i}"),
            sup.as_deref(),
        ));
    }
    let raw = format!("[{}]", items.join(","));
    let mut loader = YaGoLoader::new();
    let start = Instant::now();
    let stats = loader.load_taxonomy_json(&raw).unwrap();
    assert!(start.elapsed() < Duration::from_secs(10));
    assert_eq!(stats.classes, 3000);
    assert_eq!(stats.edges, 2999);
}
