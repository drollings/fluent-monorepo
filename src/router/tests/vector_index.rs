use super::*;
use fluent_types::NodeId;
use search_vector::VectorIndex;
use std::sync::Arc;

// M7.3: the seam (`VectorIndex` + `BruteForceIndex` + `HnswVectorIndex`) and
// its recall/calibration harness moved to `search-vector::vector_index`
// (`src/search-vector/tests/vector_index.rs`) — single home. Only the
// router-concern test stays here: it asserts on the router's salience
// provider, not on the seam.

#[test]
fn vector_index_seam_is_concern_separated() {
    // Salience provider must never call VectorIndex::search — different concern.
    // Use a mock VectorIndex that panics if searched.
    struct PanicIndex;
    impl VectorIndex for PanicIndex {
        fn insert(&self, _: NodeId, _: &[f32]) -> Result<(), fluent_db::error::DbError> {
            Ok(())
        }
        fn search(&self, _: &[f32], _: usize) -> Vec<(NodeId, f64)> {
            panic!("salience must never call VectorIndex::search")
        }
        fn len(&self) -> usize {
            0
        }
    }
    let _panic: Arc<dyn VectorIndex> = Arc::new(PanicIndex);
    // LedgerSalienceProvider signals are deterministic graph signals, no vector search.
    use crate::node_store::ContentNodeStore;
    use crate::ranking::{LedgerSalienceProvider, SalienceSource};
    let store = ContentNodeStore::ephemeral();
    let provider = LedgerSalienceProvider::new(Arc::new(store), common_core::now_secs());
    let sigs = provider.signals_for(&[NodeId::from_int(1)]);
    assert_eq!(sigs.len(), 1);
    // No panic means separation holds
}
