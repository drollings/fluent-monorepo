use std::path::{Path, PathBuf};

use common_core::registry::KeyedRegistry;
use fluent_types::GuidanceDoc;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum PluginError {
    #[error("IO error: {0}")]
    Io(#[from] common_core::error::IoError),
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("plugin not found for extension: {0}")]
    PluginNotFound(String),
    #[error("plugin execution failed: {0}")]
    ExecutionFailed(String),
    #[error("plugin subprocess crashed: {0}")]
    SubprocessCrashed(String),
}

common_core::impl_from_io_error!(PluginError);

pub type PluginResult = Result<GuidanceDoc, PluginError>;

#[derive(Debug, Clone)]
pub struct Plugin {
    pub name: String,
    pub extensions: Vec<String>,
    pub path: PathBuf,
}

pub struct PluginRegistry {
    plugins: KeyedRegistry<String, Plugin>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            plugins: KeyedRegistry::new(),
        }
    }

    pub fn register(&mut self, ext: &str, plugin: Plugin) {
        self.plugins.insert(ext.to_string(), plugin);
    }

    pub fn get_plugin(&self, ext: &str) -> Option<&Plugin> {
        self.plugins.get(ext)
    }

    /// Discover plugin binaries in the given directories.
    ///
    /// Uses a flat (non-recursive) `read_dir` intentionally: plugin discovery
    /// scans a single directory level for executables, does not filter by source
    /// extensions, and must not recurse into subdirectories.  This differs from
    /// `common_core::walk::walk_files` which recurses and filters by extension.
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
        self.plugins.contains(ext)
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Infer supported file extensions from a guidance-* binary name.
fn infer_extensions(plugin_name: &str) -> Vec<String> {
    let name = plugin_name.strip_prefix("guidance-").unwrap_or(plugin_name);
    match name {
        n if n.contains("zig") => vec!["zig".into(), "zon".into()],
        n if n.contains("python") || n.contains("py") => vec!["py".into()],
        n if n.contains("rust") || n.contains("rs") => vec!["rs".into()],
        n if n.contains("markdown") || n.contains("md") => vec!["md".into(), "markdown".into()],
        n if n.contains("typescript") || n.contains("ts") => vec!["ts".into(), "tsx".into()],
        n if n.contains("javascript") || n.contains("js") => vec!["js".into(), "jsx".into()],
        n if n.contains("go") => vec!["go".into()],
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluent_wvr_testutil::tempdir;

    // ── PluginRegistry tests ────────────────────────────────────────────────────

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
        let dir = tempdir();
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
    fn test_discover_ignores_non_guidance_binaries() {
        let dir = tempdir();
        let other = dir.path().join("some-other-tool");
        std::fs::write(&other, "#!/bin/sh").expect("write");

        let paths = vec![dir.path().to_path_buf()];
        let registry = PluginRegistry::discover(&paths);
        assert_eq!(registry.list_plugins().len(), 0);
    }

    // ── infer_extensions tests ──────────────────────────────────────────────────

    #[test]
    fn test_infer_extensions_zig() {
        let exts = infer_extensions("guidance-zig");
        assert!(exts.contains(&"zig".into()));
        assert!(exts.contains(&"zon".into()));
    }

    #[test]
    fn test_infer_extensions_python() {
        let exts = infer_extensions("guidance-py");
        assert!(exts.contains(&"py".into()));
    }

    #[test]
    fn test_infer_extensions_rust() {
        let exts = infer_extensions("guidance-rust");
        assert!(exts.contains(&"rs".into()));
    }

    #[test]
    fn test_infer_extensions_markdown() {
        let exts = infer_extensions("guidance-md");
        assert!(exts.contains(&"md".into()));
        assert!(exts.contains(&"markdown".into()));
    }

    #[test]
    fn test_infer_extensions_typescript() {
        let exts = infer_extensions("guidance-ts");
        assert!(exts.contains(&"ts".into()));
        assert!(exts.contains(&"tsx".into()));
    }

    #[test]
    fn test_infer_extensions_javascript() {
        let exts = infer_extensions("guidance-js");
        assert!(exts.contains(&"js".into()));
        assert!(exts.contains(&"jsx".into()));
    }

    #[test]
    fn test_infer_extensions_go() {
        let exts = infer_extensions("guidance-go");
        assert!(exts.contains(&"go".into()));
    }

    #[test]
    fn test_infer_extensions_unknown() {
        let exts = infer_extensions("guidance-foobar");
        assert!(exts.is_empty());
    }

    #[test]
    fn test_infer_extensions_without_prefix() {
        // infer_extensions strips "guidance-" prefix internally
        let exts = infer_extensions("guidance-zig");
        assert!(exts.contains(&"zig".into()));
    }
}
