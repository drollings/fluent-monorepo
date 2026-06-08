use std::collections::HashMap;
use std::path::{Path, PathBuf};

use guidance_common::types::{GuidanceDoc, Meta};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum PluginError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("plugin not found for extension: {0}")]
    PluginNotFound(String),
    #[error("plugin execution failed: {0}")]
    ExecutionFailed(String),
}

pub type PluginResult = Result<GuidanceDoc, PluginError>;

#[derive(Debug, Clone)]
pub struct Plugin {
    pub name: String,
    pub extensions: Vec<String>,
    pub path: PathBuf,
}

pub struct PluginRegistry {
    plugins: HashMap<String, Plugin>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            plugins: HashMap::new(),
        }
    }

    pub fn register(&mut self, ext: &str, plugin: Plugin) {
        self.plugins.insert(ext.to_string(), plugin);
    }

    pub fn get_plugin(&self, ext: &str) -> Option<&Plugin> {
        self.plugins.get(ext)
    }

    pub fn discover(paths: &[PathBuf]) -> Self {
        let mut registry = Self::new();

        for path in paths {
            if path.is_dir() {
                if let Ok(entries) = std::fs::read_dir(path) {
                    for entry in entries.flatten() {
                        let entry_path = entry.path();
                        if entry_path.is_file() {
                            registry.register_from_path(&entry_path);
                        }
                    }
                }
            } else if path.is_file() {
                registry.register_from_path(path);
            }
        }

        registry
    }

    fn register_from_path(&mut self, path: &Path) {
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with("guidance-") {
                let extensions = infer_extensions(name);
                let plugin = Plugin {
                    name: name.to_string(),
                    extensions: extensions.clone(),
                    path: path.to_path_buf(),
                };
                for ext in extensions {
                    self.register(&ext, plugin.clone());
                }
            }
        }
    }

    pub fn list_plugins(&self) -> Vec<&Plugin> {
        self.plugins.values().collect()
    }

    pub fn has_plugin_for(&self, ext: &str) -> bool {
        self.plugins.contains_key(ext)
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn infer_extensions(plugin_name: &str) -> Vec<String> {
    match plugin_name {
        n if n.contains("zig") => vec!["zig".into(), "zon".into()],
        n if n.contains("python") || n.contains("py") => vec!["py".into()],
        n if n.contains("rust") || n.contains("rs") => vec!["rs".into()],
        n if n.contains("markdown") || n.contains("md") => vec!["md".into(), "markdown".into()],
        n if n.contains("typescript") || n.contains("ts") => vec!["ts".into(), "tsx".into()],
        _ => vec![],
    }
}

pub fn invoke_plugin(_plugin: &Plugin, _file: &Path, source: &str) -> PluginResult {
    let json: serde_json::Value = serde_json::from_str(source)?;

    let meta = json
        .get("meta")
        .and_then(|m| m.as_object())
        .ok_or_else(|| PluginError::ExecutionFailed("missing meta in plugin output".into()))?;

    let module = meta.get("module").and_then(|v| v.as_str()).unwrap_or("unknown");
    let source_path = meta.get("source").and_then(|v| v.as_str()).unwrap_or("unknown");
    let language = meta.get("language").and_then(|v| v.as_str()).unwrap_or("unknown");

    Ok(GuidanceDoc {
        meta: Meta {
            module: module.into(),
            source: source_path.into(),
            language: language.into(),
        },
        comment: json.get("comment").and_then(|v| v.as_str()).map(Into::into),
        ..GuidanceDoc::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_registry_empty() {
        let registry = PluginRegistry::new();
        assert!(registry.get_plugin("zig").is_none());
        assert!(!registry.has_plugin_for("py"));
    }

    #[test]
    fn test_register_and_lookup() {
        let mut registry = PluginRegistry::new();
        let plugin = Plugin {
            name: "guidance-zig".into(),
            extensions: vec!["zig".into(), "zon".into()],
            path: PathBuf::from("/usr/local/bin/guidance-zig"),
        };
        registry.register("zig", plugin);
        assert!(registry.has_plugin_for("zig"));
        assert!(registry.get_plugin("zig").is_some());
    }

    #[test]
    fn test_discover_from_paths() {
        let dir = tempfile::tempdir().expect("temp dir");
        let zig_plugin = dir.path().join("guidance-zig");
        std::fs::write(&zig_plugin, "#!/bin/sh").expect("write");
        let py_plugin = dir.path().join("guidance-py");
        std::fs::write(&py_plugin, "#!/bin/sh").expect("write");

        let paths = vec![dir.path().to_path_buf()];
        let registry = PluginRegistry::discover(&paths);

        assert!(registry.has_plugin_for("zig"));
        assert!(registry.has_plugin_for("py"));
    }

    #[test]
    fn test_infer_extensions() {
        let exts = infer_extensions("guidance-zig");
        assert!(exts.contains(&"zig".to_string()));
        assert!(exts.contains(&"zon".to_string()));

        let exts = infer_extensions("guidance-py");
        assert!(exts.contains(&"py".to_string()));
    }
}
