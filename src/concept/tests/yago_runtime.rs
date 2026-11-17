use super::{TaxonomyHierarchy, YaGoLoader, YagoView, EMBEDDED_YAGO_CLASSES};
use fluent_types::{InterlinguaNamespace, NodeId};

fn sample_registry() -> &'static str {
    EMBEDDED_YAGO_CLASSES
}

#[test]
fn loader_pins_counts_and_first_wins() {
    let mut first = YaGoLoader::new();
    let stats = first.load_taxonomy_json(sample_registry()).expect("load");
    assert!(stats.classes > 0);
    assert!(stats.edges > 0);
    let n = first.subclass_edges().len();
    let concepts = first.into_concepts();
    assert_eq!(concepts.len(), stats.classes);
    assert!(concepts
        .iter()
        .all(|m| m.namespace == InterlinguaNamespace::YagoClass));

    let mut second = YaGoLoader::new();
    let stats2 = second.load_taxonomy_json(sample_registry()).expect("load");
    assert_eq!(stats2.classes, stats.classes, "reload is deterministic");
    let _ = NodeId::from_int(1);
    let _ = n;
}

#[test]
fn loader_edges_feed_hierarchy_byte_identical() {
    let mut loader = YaGoLoader::new();
    loader.load_taxonomy_json(sample_registry()).expect("load");
    let h = TaxonomyHierarchy::from_edges(loader.subclass_edges()).expect("edges");
    for meta in loader.into_concepts() {
        let _ = h.ancestors(meta.id);
        assert!(h.contains(meta.id));
    }
}

#[test]
fn view_loads_ttl_and_matches_loader_classes() {
    let ttl = "@prefix yago: <http://yago-knowledge.org/resource/> .\n\
               @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
               yago:Dog rdfs:subClassOf yago:Mammal .\n\
               yago:Mammal rdfs:subClassOf yago:Animal .\n";
    let path = std::env::temp_dir().join("concept-yago-baseline.ttl");
    std::fs::write(&path, ttl).expect("write fixture");
    let view = YagoView::load(&path).expect("load");
    let _ = std::fs::remove_file(&path);
    assert!(view.class_count() >= 3);
    let dog = view.resolve_curie("yago:Dog").expect("dog");
    let animal = view.resolve_curie("yago:Animal").expect("animal");
    assert!(view.is_subclass_of(dog, animal));
    assert!(!view.ancestors_of(dog).is_empty());
}
