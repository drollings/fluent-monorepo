use std::sync::Arc;

use guidance_common::error::DbError;
use guidance_common::types::ContextNode;
use thiserror::Error;

use crate::db::Library;

#[derive(Error, Debug)]
pub enum IngestError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Db(#[from] DbError),
    #[error("library error: {0}")]
    Library(#[from] crate::db::LibraryError),
    #[error("parse error: {0}")]
    Parse(String),
}

pub struct BatchIngestor {
    library: Arc<Library>,
    batch: Vec<ContextNode>,
    batch_size: usize,
}

impl BatchIngestor {
    pub fn new(library: Arc<Library>, batch_size: usize) -> Self {
        Self {
            library,
            batch: Vec::with_capacity(batch_size),
            batch_size,
        }
    }

    pub fn add(&mut self, node: ContextNode) -> Result<Option<guidance_common::types::NodeId>, IngestError> {
        let has_embedding = node.embedding.is_some();
        let node_id = self.library.insert_node(&node)?;

        if has_embedding || self.batch.len() >= self.batch_size {
            self.flush()?;
        }

        Ok(Some(node_id))
    }

    pub fn flush(&mut self) -> Result<(), IngestError> {
        self.batch.clear();
        Ok(())
    }

    pub fn pending_count(&self) -> usize {
        self.batch.len()
    }
}

pub struct TripleMapper;

impl TripleMapper {
    pub fn new() -> Self {
        Self
    }

    pub fn map_triple(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
    ) -> (ContextNode, ContextNode) {
        let sub = ContextNode {
            id: None,
            name: subject.into(),
            source: format!("{subject} {predicate} {object}"),
            lod: vec![
                format!("{subject} {predicate} {object}"),
                format!("{subject} -> {object}"),
                subject.into(),
            ],
            embedding: None,
        };

        let obj = ContextNode {
            id: None,
            name: object.into(),
            source: object.into(),
            lod: vec![object.into()],
            embedding: None,
        };

        (sub, obj)
    }
}

impl Default for TripleMapper {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_batch_ingestor_add() {
        let lib = Arc::new(Library::open_in_memory().expect("db"));
        let mut ingestor = BatchIngestor::new(lib, 100);
        let node = ContextNode {
            id: None,
            name: "test_ingest".into(),
            source: "source".into(),
            lod: vec![],
            embedding: None,
        };
        let id = ingestor.add(node).expect("add");
        assert!(id.is_some());
        assert_eq!(ingestor.pending_count(), 0);
    }

    #[test]
    fn test_triple_mapper() {
        let mapper = TripleMapper::new();
        let (sub, obj) = mapper.map_triple("Zig", "is_a", "language");
        assert_eq!(sub.name.as_str(), "Zig");
        assert_eq!(obj.name.as_str(), "language");
        assert_eq!(sub.lod.len(), 3);
    }
}
