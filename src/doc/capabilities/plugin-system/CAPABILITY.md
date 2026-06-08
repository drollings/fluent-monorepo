---
name: plugin-system
description: External AST provider protocol for non-Rust languages. Guidance discovers provider executables by scanning paths, registers them by file extension, and invokes them to produce JSON GuidanceDoc metadata.
anchors:
  - PluginRegistry
  - Plugin
  - PluginError
  - PluginResult
  - discover
  - invoke_plugin
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

- `guidance/src/plugin.rs` — `PluginRegistry`, `Plugin`, `PluginError`, `PluginResult`, `discover()`, `invoke_plugin()`, `infer_extensions()`

## Semantic Deviations

- **`tokio::process::Command` not used** — the Rust `invoke_plugin()` currently parses the plugin output string as JSON without spawning a subprocess; the `Plugin` struct stores a `path` but no subprocess invocation is implemented yet
- **`std::process::Command` would replace Zig's `std.ChildProcess`** — when subprocess invocation is added, `tokio::process::Command` or `std::process::Command` will be used
- **Same JSON protocol** — the `GuidanceDoc` / `Meta` JSON structure matches the Zig version exactly
- **`PluginRegistry` is a `HashMap<String, Plugin>`** — keyed by file extension, rather than Zig's `StringHashMap(LanguagePlugin)`
- **`infer_extensions` is a standalone function** — replaces Zig's inline extension mapping in the discovery loop
- **No built-in `AstParser` plugin** — Zig parsing is handled by `guidance/src/ast_parser.rs` (separate module), not through the plugin system
- **No `guidance-py` or `MarkdownPlugin`** — the Rust version has no built-in Python or Markdown provider implementations

## Example

```rust
use std::path::PathBuf;
use guidance_guidance::plugin::{PluginRegistry, Plugin, invoke_plugin};

let mut registry = PluginRegistry::new();
registry.register("py", Plugin {
    name: "guidance-py".into(),
    extensions: vec!["py".into()],
    path: PathBuf::from("/usr/local/bin/guidance-py"),
});

assert!(registry.has_plugin_for("py"));

// discover from a directory
let plugins = vec![PathBuf::from("/usr/local/bin")];
let registry = PluginRegistry::discover(&plugins);
```

## Zig reference

See `../doc/capabilities/plugin-system/CAPABILITY.md` in the Zig guidance source tree for the original `LanguagePlugin` interface, `discoverProvider`, `invokeProviderFile`, and the built-in `guidance-py` and `MarkdownPlugin` implementations.
