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
        self.batch.push(node);

        if has_embedding || self.batch.len() >= self.batch_size {
            self.flush()?;
        }

        Ok(None)
    }

    pub fn flush(&mut self) -> Result<(), IngestError> {
        let batch = std::mem::take(&mut self.batch);
        if batch.is_empty() {
            return Ok(());
        }
        self.library.insert_nodes_batch(&batch)?;
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
            capabilities: None,
        };

        let obj = ContextNode {
            id: None,
            name: object.into(),
            source: object.into(),
            lod: vec![object.into()],
            embedding: None,
            capabilities: None,
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
    fn test_batch_ingestor_buffers_then_flushes() {
        let lib = Arc::new(Library::open_in_memory().expect("db"));
        let mut ingestor = BatchIngestor::new(Arc::clone(&lib), 100);

        let node1 = ContextNode {
            id: None,
            name: "batched_1".into(),
            source: "source".into(),
            lod: vec![],
            embedding: None,
            capabilities: None,
        };
        let id = ingestor.add(node1).expect("add");
        assert!(id.is_none());
        assert_eq!(ingestor.pending_count(), 1);

        let node2 = ContextNode {
            id: None,
            name: "batched_2".into(),
            source: "source".into(),
            lod: vec![],
            embedding: None,
            capabilities: None,
        };
        let id = ingestor.add(node2).expect("add");
        assert!(id.is_none());
        assert_eq!(ingestor.pending_count(), 2);

        ingestor.flush().expect("flush");
        assert_eq!(ingestor.pending_count(), 0);

        assert!(lib.find_node_by_name("batched_1").unwrap().is_some());
        assert!(lib.find_node_by_name("batched_2").unwrap().is_some());
    }

    #[test]
    fn test_batch_ingestor_flushes_on_embedding() {
        let lib = Arc::new(Library::open_in_memory().expect("db"));
        let mut ingestor = BatchIngestor::new(Arc::clone(&lib), 100);

        let node = ContextNode {
            id: None,
            name: "embedded_node".into(),
            source: "source".into(),
            lod: vec![],
            embedding: Some(vec![0.1, 0.2, 0.3]),
            capabilities: None,
        };
        let id = ingestor.add(node).expect("add");
        assert!(id.is_none());
        assert_eq!(ingestor.pending_count(), 0);

        assert!(lib.find_node_by_name("embedded_node").unwrap().is_some());
    }

    #[test]
    fn test_batch_ingestor_flushes_when_full() {
        let lib = Arc::new(Library::open_in_memory().expect("db"));
        let mut ingestor = BatchIngestor::new(Arc::clone(&lib), 2);

        let node1 = ContextNode {
            id: None,
            name: "full_1".into(),
            source: "s".into(),
            lod: vec![],
            embedding: None,
            capabilities: None,
        };
        ingestor.add(node1).expect("add");
        assert_eq!(ingestor.pending_count(), 1);

        let node2 = ContextNode {
            id: None,
            name: "full_2".into(),
            source: "s".into(),
            lod: vec![],
            embedding: None,
            capabilities: None,
        };
        ingestor.add(node2).expect("add");
        assert_eq!(ingestor.pending_count(), 0);

        assert!(lib.find_node_by_name("full_1").unwrap().is_some());
        assert!(lib.find_node_by_name("full_2").unwrap().is_some());
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

