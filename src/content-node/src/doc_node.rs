use std::any::Any;

use crate::file_node::FileContentNode;
use crate::lod::generate_lod_slices;
use crate::node::{ContentNode, LodLevel, NodeType, NodeTypeInfo};

const DOC_LOD_LABELS: &[&str] = &["full text", "~800ch summary", "keyword index", "<240ch", "<80ch", "path/filename"];

#[derive(Debug)]
pub struct DocumentContentNode {
    inner: FileContentNode,
    lod: Vec<String>,
}

impl DocumentContentNode {
    pub fn new(inner: FileContentNode, full_text: &str) -> Self {
        let lod = generate_lod_slices(full_text);
        Self { inner, lod }
    }

    pub fn inner(&self) -> &FileContentNode { &self.inner }
    pub fn lod_slice(&self, level: LodLevel) -> Option<&str> {
        let idx = level as usize;
        if idx < self.lod.len() { Some(self.lod[idx].as_str()) } else { None }
    }
}

impl ContentNode for DocumentContentNode {
    fn node_type(&self) -> NodeType { NodeType::Document }
    fn lod(&self, level: LodLevel) -> Option<&str> {
        let idx = level as usize;
        if idx < self.lod.len() && !self.lod[idx].is_empty() {
            Some(self.lod[idx].as_str())
        } else {
            self.inner.path().to_str()
        }
    }
    fn set_lod(&mut self, _level: LodLevel, _value: &str) {}
    fn lod_label(&self, level: LodLevel) -> Option<&str> {
        DOC_LOD_LABELS.get(level as usize).copied()
    }
    fn type_info(&self) -> NodeTypeInfo {
        NodeTypeInfo { kind: NodeType::Document, name: "DocumentContentNode", lod_labels: DOC_LOD_LABELS }
    }
    fn as_any(&self) -> &dyn Any { self }
    fn as_any_mut(&mut self) -> &mut dyn Any { self }
}
