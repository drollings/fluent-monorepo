---
name: plugin-system
description: External AST provider protocol for non-Rust languages. Guidance discovers provider executables by scanning paths, registers them by file extension, and invokes them via subprocess to produce JSON GuidanceDoc metadata.
anchors:
  - PluginRegistry
  - Plugin
  - PluginError
  - PluginResult
  - discover
  - invoke_plugin
  - discover_provider
  - infer_extensions
---

# Plugin System

Allows source files in non-Rust languages (Python, TypeScript, Markdown, etc.) to be indexed by guidance through external provider executables that speak the guidance JSON protocol.

## Provider discovery

`PluginRegistry::discover()` scans file-system paths for executables named `guidance-<name>` (e.g. `guidance-py` for `.py` files). Extensions are inferred from the binary name via `infer_extensions`:

| Binary name pattern | Extensions |
|---------------------|------------|
| Contains `zig` | `zig`, `zon` |
| Contains `python` or `py` | `py` |
| Contains `rust` or `rs` | `rs` |
| Contains `markdown` or `md` | `md`, `markdown` |
| Contains `typescript` or `ts` | `ts`, `tsx` |
| Contains `javascript` or `js` | `js`, `jsx` |
| Contains `go` | `go` |

`discover_provider(workspace, extension)` searches for a plugin binary in order:
1. `{workspace}/bin/guidance-{bare_ext}` — workspace-local binary
2. `PATH` via the `which` crate — system-wide binary

## Subprocess invocation

`invoke_plugin()` spawns the external provider as a subprocess:

```rust
pub fn invoke_plugin(plugin: &Plugin, src_path: &Path, output_dir: &Path) -> PluginResult {
    let output = std::process::Command::new(&plugin.path)
        .arg("sync")
        .arg("--file")
        .arg(src_path)
        .arg("--output")
        .arg(output_dir)
        .output()
        .map_err(|e| PluginError::SubprocessCrashed(...))?;

    // Parse JSON GuidanceDoc from stdout
    let doc: GuidanceDoc = serde_json::from_slice(&output.stdout)?;
    Ok(doc)
}
```

The plugin binary is expected to accept `sync --file {src_path} --output {output_dir}` arguments and produce a `GuidanceDoc` JSON on stdout.

## JSON protocol

An external provider reads a source file, outputs a guidance JSON document to stdout, and the result is parsed by `invoke_plugin()` into a `GuidanceDoc`:

```json
{
  "meta": { "module": "src.foo.bar", "source": "src/foo/bar.py", "language": "python" },
  "comment": "Module-level description.",
  "members": [...]
}
```

## Key files

- `guidance/src/plugin.rs` — `PluginRegistry`, `Plugin`, `PluginError`, `PluginResult`, `discover()`, `invoke_plugin()`, `discover_provider()`, `infer_extensions()`

## Semantic Deviations

- **`std::process::Command`** for subprocess invocation — spawns `{plugin.path} sync --file {src_path} --output {output_dir}`
- **`which` crate** for PATH-based discovery — finds system-wide `guidance-*` binaries
- **Same JSON protocol** — the `GuidanceDoc` / `Meta` JSON structure matches the Zig version exactly
- **`PluginRegistry` is a `HashMap<String, Plugin>`** — keyed by file extension, rather than Zig's `StringHashMap(LanguagePlugin)`
- **`infer_extensions` is a standalone function** — replaces Zig's inline extension mapping in the discovery loop
- **No built-in `AstParser` plugin** — Zig/Python/Rust parsing is handled by `guidance/src/ast_parser.rs` (tree-sitter), not through the plugin system
- **No `guidance-py` or `MarkdownPlugin`** — the Rust version has no built-in Python or Markdown provider implementations; these are expected as external binaries

## Example

```rust
use std::path::PathBuf;
use guidance_core::plugin::{PluginRegistry, Plugin, invoke_plugin, discover_provider};

// Manual registration
let mut registry = PluginRegistry::new();
registry.register("py", Plugin {
    name: "guidance-py".into(),
    extensions: vec!["py".into()],
    path: PathBuf::from("/usr/local/bin/guidance-py"),
});
assert!(registry.has_plugin_for("py"));

// Discover from directories
let registry = PluginRegistry::discover(&[PathBuf::from("/usr/local/bin")]);

// Discover provider for a specific extension
if let Some(plugin) = discover_provider(&workspace, "py") {
    let doc = invoke_plugin(&plugin, &src_path, &output_dir)?;
}
```

## Zig reference

See `../doc/capabilities/plugin-system/CAPABILITY.md` in the Zig guidance source tree for the original `LanguagePlugin` interface, `discoverProvider`, `invokeProviderFile`, and the built-in `guidance-py` and `MarkdownPlugin` implementations.
