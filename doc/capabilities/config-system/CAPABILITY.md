---
name: config-system
description: CLI argument parsing with clap and project configuration via ProjectConfig (serde_json + bon::Builder)
anchors:
  - Cli
  - Commands
  - ProjectConfig
  - ConfigError
  - guidance_dir
---

# Config System

Parses CLI arguments with `clap` and loads project configuration from `.guidance/guidance-config.json` via `serde_json`. The `ProjectConfig` struct (with `bon::Builder`) holds all runtime configuration: guidance directory, embedding provider/model, test/lint/fmt commands, and provider endpoints.

## Key files

- `bin/guidance/src/main.rs` — `Cli` parser, `Commands` enum (15 subcommands), `--debug` / `--show-prompts` global flags
- `guidance/src/config.rs` — `ProjectConfig`, `Provider`, `ConfigError`, config loading from JSON

## ProjectConfig

```rust
use guidance::config::ProjectConfig;

// Load from .guidance/guidance-config.json
let config = ProjectConfig::load(dir_path)?;

// Or build programmatically
let config = ProjectConfig::builder()
    .guidance_dir(PathBuf::from(".guidance"))
    .embedding_provider("ollama".into())
    .embedding_model("nomic-embed-text".into())
    .embedding_dims(768)
    .build();
```

### Fields

| Field | Type | Default | Purpose |
|-------|------|---------|---------|
| `guidance_dir` | `PathBuf` | `.guidance` | Root guidance directory |
| `json_base` | `Option<PathBuf>` | `None` | JSON output base directory |
| `skills_dir` | `Option<PathBuf>` | `None` | Skills directory |
| `inbox_dir` | `Option<PathBuf>` | `None` | Inbox directory |
| `db_path` | `Option<PathBuf>` | `None` | SQLite database path |
| `embedding_provider` | `Option<String>` | `None` | Provider name (ollama, openai, none) |
| `embedding_model` | `Option<String>` | `None` | Model name |
| `embedding_dims` | `Option<usize>` | `None` | Embedding dimensions |
| `test_commands` | `HashMap<String, Vec<String>>` | `{}` | Per-language test commands |
| `lint_commands` | `HashMap<String, Vec<String>>` | `{}` | Per-language lint commands |
| `fmt_commands` | `HashMap<String, Vec<String>>` | `{}` | Per-language format commands |
| `providers` | `HashMap<String, Provider>` | `{}` | Provider endpoint configurations |

## CLI subcommands

```
guidance explain <query>      — Semantic codebase search
guidance sync [--file|--scan|--watch|--force] — Incremental JSON/DB sync
guidance check                — Multi-stage CI check (test→lint→fmt→sync→structure→db)
guidance init [dir]           — Create .guidance/ with default config
guidance status               — Show stale/up-to-date file counts
guidance clean                — Remove generated JSON and DB
guidance commit <message>     — Git commit wrapper
guidance benchmark <query>    — Query accuracy benchmark
guidance structure            — Generate STRUCTURE.md
guidance health               — Comment coverage report
guidance test                 — Run cargo test --workspace
guidance telemetry            — DB node/embedding counts
guidance cache-stats          — DB cache statistics
guidance todo                 — Print TODO.md items
guidance diary <text>         — Append diary entry
```

### Global flags

| Flag | Purpose |
|------|---------|
| `--debug` | Show LLM metadata, progress tracking |
| `--show-prompts` | Show complete raw prompt text sent to LLM |

## Example config file

```json
{
  "guidance_dir": ".guidance",
  "embedding_provider": "ollama",
  "embedding_model": "nomic-embed-text",
  "embedding_dims": 768,
  "test_commands": {
    "rs": ["cargo", "test", "--workspace"]
  },
  "lint_commands": {
    "rs": ["cargo", "clippy", "--workspace", "--", "-D", "warnings"]
  },
  "fmt_commands": {
    "rs": ["cargo", "fmt", "--check"]
  }
}
```

## Key files

- `bin/guidance/src/main.rs` — `Cli` struct, `Commands` enum, `--debug` / `--show-prompts` flags
- `guidance/src/config.rs` — `ProjectConfig` (bon::Builder), `Provider`, `ConfigError`, `load()`

## Semantic Deviations

| Aspect | Zig | Rust |
|--------|-----|------|
| CLI parsing | `std.process.args` / hand-rolled `ArgIterator` | `clap` derive macros (`#[derive(Parser)]`) |
| Config loading | `std.json` | `serde_json` with `#[derive(Deserialize)]` |
| Config struct | Manual field initialization | `#[derive(bon::Builder)]` with `#[builder(default)]` |
| Global flags | Per-subcommand | `global = true` on `Cli` struct |
| Error handling | Error sets | `thiserror`-derived `ConfigError` enum |

## Zig reference

See `src/guidance/config.zig` in the Zig source tree for the original `ProjectConfig` struct and `loadConfig()` function using `std.json`.
