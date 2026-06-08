use std::path::Path;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum WasmError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("plugin load failed: {0}")]
    PluginLoad(String),
    #[error("plugin call failed: {0}")]
    PluginCall(String),
    #[error("invalid payload: {0}")]
    InvalidPayload(String),
}

pub trait WasmPlugin: Send {
    fn call(&mut self, payload: &[u8]) -> Result<Vec<u8>, WasmError>;
}

pub trait WasmRuntime: Send + Sync {
    fn load_plugin(&self, wasm_bytes: &[u8]) -> Result<Box<dyn WasmPlugin>, WasmError>;
    fn load_plugin_from_file(&self, path: &Path) -> Result<Box<dyn WasmPlugin>, WasmError>;
}

pub struct ExtismWasmRuntime;

impl ExtismWasmRuntime {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ExtismWasmRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl WasmRuntime for ExtismWasmRuntime {
    fn load_plugin(&self, wasm_bytes: &[u8]) -> Result<Box<dyn WasmPlugin>, WasmError> {
        let plugin = extism::Plugin::new(
            wasm_bytes,
            [],
            true,
        )
        .map_err(|e| WasmError::PluginLoad(e.to_string()))?;

        Ok(Box::new(ExtismPlugin { plugin }))
    }

    fn load_plugin_from_file(&self, path: &Path) -> Result<Box<dyn WasmPlugin>, WasmError> {
        let wasm_bytes = std::fs::read(path)?;
        self.load_plugin(&wasm_bytes)
    }
}

struct ExtismPlugin {
    plugin: extism::Plugin,
}

impl WasmPlugin for ExtismPlugin {
    fn call(&mut self, payload: &[u8]) -> Result<Vec<u8>, WasmError> {
        let result = self
            .plugin
            .call("execute", payload)
            .map_err(|e| WasmError::PluginCall(e.to_string()))?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_creation() {
        let runtime = ExtismWasmRuntime::new();
        let result = runtime.load_plugin(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_load_plugin_file_not_found() {
        let runtime = ExtismWasmRuntime::new();
        let result = runtime.load_plugin_from_file(Path::new("/nonexistent/plugin.wasm"));
        assert!(result.is_err());
    }
}
