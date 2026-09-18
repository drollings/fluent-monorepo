use super::*;
use crate::lod::Lod;
use fluent_concept::content_store::{ContentNodeStore, InMemoryContentNodeStore};
use fluent_types::ContentNode;
use std::collections::HashSet;
use std::sync::Arc;

fn node(content: &str, session: &str) -> ContentNode {
    ContentNode {
        lod: vec![content.to_string()],
        session_id: Some(session.to_string()),
        ..Default::default()
    }
}

fn store() -> Arc<InMemoryContentNodeStore> {
    let s = Arc::new(InMemoryContentNodeStore::new());
    s.insert(node("First sentence here. Second follows.", "s"));
    s.insert(node("second", "s"));
    s
}

#[test]
fn parallel_render_default_and_override() {
    let s = store();
    let view = ParallelLedger::for_session(s.clone(), "s").with_default_lod(Lod::LOD0);
    let body = view.render();
    assert!(body.contains("First sentence here.") && body.contains("second"));
    let ids = view.node_ids();
    let view = ParallelLedger::for_session(s, "s")
        .with_default_lod(Lod::LOD0)
        .with_override(ids[0], Lod::LOD5);
    let body = view.render();
    assert!(body.contains("First sentence here."), "override renders label");
    assert!(!body.contains("Second follows"), "label tier, not full text");
}

#[test]
fn exclusion_is_free_and_missing_skipped() {
    let s = store();
    let ids = ParallelLedger::for_session(s.clone(), "s").node_ids();
    let mut excluded = HashSet::new();
    excluded.insert(ids[0]);
    let view =
        FilteredLedger::new(ParallelLedger::for_session(s, "s"), excluded);
    let body = view.render();
    assert!(!body.contains("First sentence here."));
    assert!(body.contains("second"));
}

#[test]
fn render_transform_applies_per_line() {
    let s = store();
    let view = FilteredLedger::new(
        ParallelLedger::for_session(s, "s").with_default_lod(Lod::LOD0),
        HashSet::new(),
    )
    .with_transform(|_, text| text.to_uppercase());
    let body = view.render();
    assert!(body.contains("FIRST") && body.contains("SECOND"));
}

#[test]
fn missing_nodes_never_render_empty_lines() {
    let s: Arc<dyn ContentNodeStore> =
        Arc::new(InMemoryContentNodeStore::new()) as Arc<dyn ContentNodeStore>;
    let ghost = fluent_types::NodeId::from_int(4242);
    struct Ghosts {
        store: Arc<dyn ContentNodeStore>,
        ids: Vec<fluent_types::NodeId>,
    }
    impl LedgerView for Ghosts {
        fn store(&self) -> &Arc<dyn ContentNodeStore> {
            &self.store
        }
        fn node_ids(&self) -> Vec<fluent_types::NodeId> {
            self.ids.clone()
        }
        fn lod_for(&self, _id: fluent_types::NodeId) -> Lod {
            Lod::LOD0
        }
    }
    let view = Ghosts {
        store: s,
        ids: vec![ghost],
    };
    assert_eq!(view.render(), "", "missing node skipped, not empty line");
}
