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

use std::sync::Mutex;

use guidance_common::traits::{Describable, FieldAccess, FieldError, WorkContext, WorkError, WorkOutput, WorkUnit};
use internment::ArcIntern;

pub struct WasmComponent {
    name: String,
    plugin: Mutex<Box<dyn WasmPlugin>>,
    config: Mutex<std::collections::HashMap<String, String>>,
    depends: Vec<ArcIntern<str>>,
    provides: Vec<ArcIntern<str>>,
}

impl WasmComponent {
    pub fn new(name: impl Into<String>, plugin: Box<dyn WasmPlugin>) -> Self {
        Self {
            name: name.into(),
            plugin: Mutex::new(plugin),
            config: Mutex::new(std::collections::HashMap::new()),
            depends: Vec::new(),
            provides: Vec::new(),
        }
    }

    pub fn with_depends(mut self, deps: &[ArcIntern<str>]) -> Self {
        self.depends = deps.to_vec();
        self
    }

    pub fn with_provides(mut self, prov: &[ArcIntern<str>]) -> Self {
        self.provides = prov.to_vec();
        self
    }
}

impl FieldAccess for WasmComponent {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        self.config.lock().unwrap().insert(name.to_string(), value.to_string());
        Ok(())
    }

    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        self.config
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .ok_or_else(|| FieldError::NotFound(name.into()))
    }

    fn field_names(&self) -> &'static [&'static str] {
        &[]
    }
}

impl Describable for WasmComponent {
    fn describe(&self) -> serde_json::Value {
        let config = self.config.lock().unwrap();
        serde_json::json!({
            "name": self.name,
            "type": "wasm",
            "config": *config,
        })
    }
}

impl WorkUnit for WasmComponent {
    fn name(&self) -> &str {
        &self.name
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &self.depends
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &self.provides
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        let payload = serde_json::to_vec(&*self.config.lock().unwrap())
            .map_err(|e| WorkError::Execution(format!("serialization: {e}")))?;
        let result = self
            .plugin
            .lock()
            .unwrap()
            .call(&payload)
            .map_err(|e| WorkError::Execution(e.to_string()))?;
        let data: serde_json::Value = serde_json::from_slice(&result).unwrap_or_default();
        Ok(WorkOutput::ok_with_data(format!("{} executed", self.name), data))
    }
}

/// Decode a base64-encoded WASM binary using the extism-compatible simple decoder.
pub fn decode_wasm_base64(b64: &str) -> Result<Vec<u8>, WasmError> {
    // Simple base64 decode without external crate dependency
    let bytes = b64.as_bytes();
    let mut result = Vec::with_capacity(bytes.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits = 0;
    for &c in bytes {
        let val = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b'\n' | b'\r' | b' ' | b'\t' => continue,
            _ => return Err(WasmError::InvalidPayload(format!("invalid base64 char: {}", c as char))),
        };
        buf = (buf << 6) | val as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            result.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockPlugin;

    impl WasmPlugin for MockPlugin {
        fn call(&mut self, payload: &[u8]) -> Result<Vec<u8>, WasmError> {
            Ok(payload.to_vec())
        }
    }

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

    #[test]
    fn test_wasm_component_name() {
        let plugin = MockPlugin;
        let comp = WasmComponent::new("test_wasm", Box::new(plugin));
        assert_eq!(comp.name(), "test_wasm");
    }

    #[test]
    fn test_wasm_component_field_access() {
        let mut comp = WasmComponent::new("test", Box::new(MockPlugin));
        comp.set_field("key1", "value1").unwrap();
        assert_eq!(comp.get_field("key1").unwrap(), "value1");
        assert!(comp.get_field("nonexistent").is_err());
    }

    #[test]
    fn test_wasm_component_describe() {
        let mut comp = WasmComponent::new("desc_test", Box::new(MockPlugin));
        comp.set_field("port", "8080").unwrap();
        let desc = comp.describe();
        assert_eq!(desc["name"], "desc_test");
        assert_eq!(desc["config"]["port"], "8080");
    }

    #[test]
    fn test_wasm_component_execute() {
        let mut comp = WasmComponent::new("exec_test", Box::new(MockPlugin));
        comp.set_field("msg", "hello").unwrap();
        let ctx = WorkContext::default();
        let output = comp.execute(&ctx).unwrap();
        assert!(output.success);
    }

    #[test]
    fn test_wasm_component_deps() {
        let comp = WasmComponent::new("deps_test", Box::new(MockPlugin))
            .with_depends(&[ArcIntern::from("prep")])
            .with_provides(&[ArcIntern::from("result")]);
        assert_eq!(comp.depends().len(), 1);
        assert_eq!(&*comp.provides()[0], "result");
    }
}
