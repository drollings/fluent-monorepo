//! M5.1 pins for `YagoResolveStage`: name/depends/provides/describe are
//! byte-pinned so the `describe`-helper migration (M5.9) proves zero output
//! change. The `FieldAccess` no-op trio is pinned too — the migration must
//! not alter the field surface.

use super::*;
use std::sync::Arc;

use fluent_concept::{ConceptStore, InMemoryConceptStore};

fn ascii(deps: &[internment::ArcIntern<str>]) -> Vec<String> {
    deps.iter().map(|d| d.to_string()).collect()
}

#[test]
fn yago_resolve_metadata_and_describe_are_pinned() {
    let store: Arc<InMemoryConceptStore> = Arc::new(InMemoryConceptStore::new());
    let stage = YagoResolveStage::new(Arc::clone(&store) as Arc<dyn ConceptStore>);
    assert_eq!(stage.name(), "yago_resolve");
    assert_eq!(ascii(stage.depends()), vec!["annotated_doc"]);
    assert_eq!(ascii(stage.provides()), vec!["yago_resolved"]);
    // No `purity` key today — the helper migration must not add one.
    assert_eq!(
        stage.describe(),
        serde_json::json!({
            "name": "yago_resolve",
            "depends": ["annotated_doc"],
            "provides": ["yago_resolved"]
        })
    );
    assert!(stage.describe().get("purity").is_none());
}

#[test]
fn yago_resolve_field_access_stays_fieldless() {
    let store: Arc<InMemoryConceptStore> = Arc::new(InMemoryConceptStore::new());
    let mut stage = YagoResolveStage::new(Arc::clone(&store) as Arc<dyn ConceptStore>);
    assert!(stage.field_names().is_empty());
    assert!(stage.set_field("enabled", "true").is_err());
    assert!(stage.get_field("enabled").is_err());
}
