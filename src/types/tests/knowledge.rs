use super::*;

/// A trivial in-crate fake so the trait is exercised without depending on
/// any store implementation.
struct FakeKnowledge;

impl KnowledgeCapability for FakeKnowledge {
    fn get_node(&self, node_id: NodeId) -> Option<ContentNode> {
        (node_id == NodeId::from_int(1)).then(|| ContentNode {
            id: Some(node_id),
            ..Default::default()
        })
    }
    fn get_session_nodes(&self, session_id: &str, _limit: usize) -> Vec<ContentNode> {
        if session_id == "s" {
            vec![ContentNode {
                id: Some(NodeId::from_int(1)),
                ..Default::default()
            }]
        } else {
            vec![]
        }
    }
    fn insert_node(&self, _node: &ContentNode) -> Result<NodeId, KnowledgeError> {
        Ok(NodeId::from_int(42))
    }
    fn knn_search(&self, _embedding: &[f32], _k: usize) -> Result<Vec<KnnHit>, KnowledgeError> {
        Ok(vec![])
    }
}

#[test]
fn trait_is_dispatchable_through_dyn() {
    let k: &dyn KnowledgeCapability = &FakeKnowledge;
    assert!(k.get_node(NodeId::from_int(1)).is_some());
    assert!(k.get_node(NodeId::from_int(2)).is_none());
    assert_eq!(k.get_session_nodes("s", 10).len(), 1);
    assert!(k.get_session_nodes("other", 10).is_empty());
}

#[test]
fn insert_and_knn_round_trip_through_dyn() {
    let k: &dyn KnowledgeCapability = &FakeKnowledge;
    let node = ContentNode::default();
    assert_eq!(k.insert_node(&node).unwrap(), NodeId::from_int(42));
    assert_eq!(k.knn_search(&[0.0; 4], 3).unwrap().len(), 0);
}

#[test]
fn knowledge_error_variants_display() {
    assert!(KnowledgeError::NotFound(NodeId::from_int(7))
        .to_string()
        .contains("7"));
    assert_eq!(
        KnowledgeError::NoEmbedding.to_string(),
        "node has no embedding"
    );
}
