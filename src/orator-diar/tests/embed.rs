//! Embedding tests: order-preserving batches through the seam.

use std::sync::Arc;

use crate::embed::{batch_embed, ScriptSeam, TEST_DIMS};

#[test]
fn batches_return_in_index_order() {
    let seam = Arc::new(ScriptSeam {
        script: vec![vec![1.0; TEST_DIMS], vec![0.0; TEST_DIMS]],
    });
    let segments = vec![vec![0.5; 100]; 6];
    let vectors = batch_embed(seam, &segments, 2);
    assert_eq!(vectors.len(), 6);
    assert_eq!(vectors[0], vec![1.0; TEST_DIMS]);
    assert_eq!(vectors[1], vec![0.0; TEST_DIMS]);
    assert_eq!(vectors[2], vec![1.0; TEST_DIMS]);
}
