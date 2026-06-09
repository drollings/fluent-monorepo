use std::any::Any;
use std::fmt::Debug;
use std::path::{Path, PathBuf};

use crate::node::{ContentNode, LodLevel, NodeType, NodeTypeInfo};

const FILE_LOD_LABELS: &[&str] = &["path", "inode+hash", "", "", "", ""];

#[derive(Debug)]
pub struct FileContentNode {
    path: PathBuf,
    inode: u64,
    hash: blake3::Hash,
}

impl FileContentNode {
    pub fn new(path: PathBuf, inode: u64, hash: blake3::Hash) -> Self {
        Self { path, inode, hash }
    }

    pub fn path(&self) -> &Path { &self.path }
    pub fn inode(&self) -> u64 { self.inode }
    pub fn hash(&self) -> &blake3::Hash { &self.hash }
}

impl ContentNode for FileContentNode {
    fn node_type(&self) -> NodeType { NodeType::File }
    fn lod(&self, level: LodLevel) -> Option<&str> {
        match level {
            LodLevel::Name | LodLevel::Source => Some(self.path.to_str()?),
            _ => None,
        }
    }
    fn set_lod(&mut self, _level: LodLevel, _value: &str) {}
    fn lod_label(&self, level: LodLevel) -> Option<&str> {
        FILE_LOD_LABELS.get(level as usize).copied()
    }
    fn type_info(&self) -> NodeTypeInfo {
        NodeTypeInfo { kind: NodeType::File, name: "FileContentNode", lod_labels: FILE_LOD_LABELS }
    }
    fn as_any(&self) -> &dyn Any { self }
    fn as_any_mut(&mut self) -> &mut dyn Any { self }
}
