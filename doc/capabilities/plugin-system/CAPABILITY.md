---
name: plugin-system
description: External AST provider protocol for non-Zig languages. Guidance discovers provider executables via PATH or config, invokes them per-file, and merges their JSON output into the guidance index.
anchors:
  - LanguagePlugin
  - PluginRegistry
  - discoverProvider
  - invokeProviderFile
---

# Plugin System

Allows non-Zig source files (Python, TypeScript, etc.) to be indexed by guidance through external provider executables that speak the guidance JSON protocol.

## Provider discovery

Guidance looks for provider executables named `guidance-<ext>` (e.g. `guidance-py` for `.py` files) in:
1. The project's `bin/` directory
2. `PATH`

## JSON protocol

A provider receives a source file path via stdin or argv, and outputs a guidance JSON document to stdout:

```json
{
  "meta": { "module": "src.foo.bar", "source": "src/foo/bar.py", "language": "python" },
  "comment": "Module-level description.",
  "members": [
    { "type": "fn_decl", "name": "doThing", "signature": "def doThing(x)", "comment": "...", "line": 10 }
  ]
}
```

## Built-in providers

| Extension | Provider | Location |
|-----------|----------|----------|
| `.zig` | Built-in `AstParser` | `src/guidance/ast_parser.zig` |
| `.py` | `guidance-py` | `bin/guidance-py` |
| `.md` | `MarkdownPlugin` | `src/guidance/plugins/` |

## Key files

- `src/guidance/provider_discovery.zig` — `discoverProvider`, `invokeProviderFile`
- `src/guidance/plugin.zig` — `PluginProvider` interface
- `src/guidance/plugin_registry.zig` — Built-in plugin registry
- `bin/guidance-py` — Python AST provider implementation

## CLI

```bash
guidance gen --all-languages    # discover and invoke external providers
```

<!-- AUTO-SOURCES: do not edit below this line. Updated by `guidance gen`. -->
## Sources (8 files, auto-discovered)

| File | Confidence | Reason |
|------|-----------|--------|
| `src/guidance/provider_discovery.zig` | 1.0 | defines_anchor |
| `src/guidance/plugin.zig` | 1.0 | defines_anchor |
| `src/guidance/plugin_registry.zig` | 1.0 | defines_anchor |
| `src/guidance/main.zig` | 0.9 | used_by |
| `src/guidance/query_engine.zig` | 0.9 | used_by |
| `src/guidance/sync_engine.zig` | 0.9 | used_by |
| `src/guidance/plugins/zig_plugin.zig` | 0.4 | path_heuristic |
| `src/guidance/plugins/markdown_plugin.zig` | 0.4 | path_heuristic |

