---
name: config-system
description: Two-level configuration loader for guidance projects. Reads guidance-config.json from the project .guidance/ directory, falls back to ~/.config/guidance/, then to built-in defaults.
anchors:
  - ProjectConfig
  - loadConfig
  - buildFromParts
---

# Config System

Resolves all guidance configuration from a JSON file with a two-level fallback chain.

## Resolution order

1. `{cwd}/.guidance/guidance-config.json` — project-local
2. `~/.config/guidance/guidance-config.json` — user-global
3. Built-in defaults

## Key fields

```json
{
  "version": "1",
  "guidance_dir": ".guidance",
  "db_path": ".guidance.db",
  "guidance_db_path": ".guidance.db",
  "embedding_provider": "ollama",
  "embedding_model": "nomic-embed-text",
  "embedding_dims": 768,
  "capabilities_dir": "doc/capabilities",
  "src_dirs": ["src"],
  "providers": {
    "local": { "base_url": "http://localhost:11434", "chat_endpoint": "/v1/chat/completions" }
  },
  "models": { "default": "local:code:latest", "fast": "", "thinking": "" },
  "test_commands": { ".zig": ["zig", "build", "test", "--summary", "all"] },
  "lint_commands": { ".zig": ["zig", "fmt", "--check", "{file}"] },
  "fmt_commands":  { ".zig": ["zig", "fmt", "{file}"] }
}
```

## Model references

Model names use the format `"provider:modelname"`, e.g. `"local:code:latest"`. The config resolves these to base URLs via the `providers` map.

## Key files

- `src/guidance/config.zig` — `loadConfig`, `ProjectConfig`, `buildFromParts`
- `.guidance/guidance-config.json` — project configuration file

<!-- AUTO-SOURCES: do not edit below this line. Updated by `guidance gen`. -->
## Sources (6 files, auto-discovered)

| File | Confidence | Reason |
|------|-----------|--------|
| `src/guidance/config.zig` | 1.0 | defines_anchor |
| `src/guidance/main.zig` | 0.9 | used_by |
| `src/guidance/query_engine.zig` | 0.9 | used_by |
| `src/guidance/scanner.zig` | 0.9 | used_by |
| `src/guidance/sync_engine.zig` | 0.9 | used_by |
| `src/coral/config.zig` | 0.7 | keyword_overlap |

