---
name: config-system
description: CLI argument parsing and project configuration via clap + serde_json
anchors:
  - Cli
  - Commands
  - parse
  - guidance_dir
---

# Config System

Parses CLI arguments with `clap` and loads project configuration from JSON files via `serde_json`. The binary defines a `Cli` struct with global flags (`--debug`, `--show-prompts`) and subcommands (`Explain`, `Show`, `Gen`, `Serve`, `Init`, `Status`, etc.). Project-level configuration (model, provider, `.guidance/` directory path) is read from `.guidance/guidance-config.json` at runtime.

## Key files

- `bin/guidance/src/main.rs` — `Cli` parser, `Commands` enum, `--debug` / `--show-prompts` flags
- Config loading uses `serde_json` from files on disk (no standalone `config.rs` module; configuration is embedded in the CLI entrypoint)

## Semantic Deviations

- **clap** replaces Zig's `std.process.args` / hand-rolled `ArgIterator` — declarative derive macros for all CLI flags
- **serde_json** replaces `std.json` — deserializes `.guidance/guidance-config.json` via `serde_json::from_str`
- **No Zig comptime reflection** — Rust uses `#[derive(Parser)]` and `#[command]` proc macros
- **Global flags** (`--debug`, `--show-prompts`) defined on `Cli` with `global = true` rather than per-subcommand

## Example

```rust
use clap::Parser;

#[derive(Parser)]
#[command(name = "guidance")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    #[arg(global = true, long)]
    debug: bool,
}

#[derive(clap::Subcommand)]
enum Commands {
    Explain { query: String, #[arg(short, long)] guidance_dir: String },
    Gen { #[arg(short, long)] file: Option<String> },
    Serve,
}
```

## Zig reference

See `../src/guidance/config.zig` (Zig version) for the original `ProjectConfig` struct and `loadConfig()` function using `std.json`.
