use super::*;
use crate::lod::Lod;
use crate::lod_policy::LodSpec;
use fluent_concept::content_store::InMemoryContentNodeStore;
use fluent_types::{ContentNode, NodeId};
use std::sync::Arc;

fn node(content: &str, embedding: Option<Vec<f32>>) -> ContentNode {
    ContentNode {
        lod: vec![content.to_string()],
        embedding,
        ..Default::default()
    }
}

fn store() -> Arc<InMemoryContentNodeStore> {
    Arc::new(InMemoryContentNodeStore::new())
}

#[test]
fn uniform_selector_assigns_midpoint() {
    let s = store();
    let a = s.insert(node("a", None));
    let ids = vec![a];
    let out = UniformSelector.select(s.as_ref(), &ids, &LodSpec::full());
    assert_eq!(out, vec![(a, Lod::LOD3)]);
}

#[test]
fn cosine_selector_ranks_by_similarity_embeddingless_last() {
    let s = store();
    let near = s.insert(node("near", Some(vec![1.0, 0.0])));
    let far = s.insert(node("far", Some(vec![0.0, 1.0])));
    let no_emb = s.insert(node("noemb", None));
    let ids = vec![far, no_emb, near];
    let sel = CosineRankSelector::new(vec![1.0, 0.0]);
    let out = sel.select(s.as_ref(), &ids, &LodSpec::full());
    let order: Vec<NodeId> = out.iter().map(|(id, _)| *id).collect();
    assert_eq!(order, vec![near, far, no_emb]);
    assert_eq!(out[0].1, Lod::LOD1, "most relevant → finest");
    assert_eq!(out[2].1, Lod::LOD5, "least relevant → coarsest");
}
