//! ServingConfig sub-config (M8) — models, sidecar, onnx, gguf.
use crate::config::{ModelEntry, SidecarConfig};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServingConfig {
    #[serde(default)]
    pub models: Vec<ModelEntry>,
    #[serde(default)]
    pub sidecar: Option<SidecarConfig>,
    #[serde(default)]
    pub onnx: Option<String>,
}
