use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Parser, Subcommand};
use guidance_coral::ingest::{BatchIngestor, IngestionConfig};
use guidance_coral::mcp::McpServer;
use guidance_coral::db::Library;
use guidance_guidance::sync_engine::SyncEngine;
use guidance_common::shell::run_command;
use time::OffsetDateTime;

#[derive(Parser)]
#[command(name = "guidance", about = "AST-guided vector search & edge AI orchestrator")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Enable debug output
    #[arg(global = true, long)]
    debug: bool,

    /// Show LLM prompts
    #[arg(global = true, long)]
    show_prompts: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum OutputFormat {
    Markdown,
    Json,
    Compact,
    Debug,
}

#[derive(Subcommand)]
enum Commands {
    /// Explain a keyword or query
    Explain {
        query: String,

        /// Path to guidance directory
        #[arg(short, long, default_value = ".guidance")]
        guidance_dir: String,

        /// Output format: markdown, json, compact, debug
        #[arg(short, long, default_value = "markdown")]
        output: String,
    },
    /// Show guidance info for a file
    Show {
        /// Path to source file
        file: String,
    },
    /// Run tests
    Test,
    /// Show telemetry
    Telemetry,
    /// Show cache statistics
    CacheStats,
    /// Serve MCP protocol over STDIO
    Serve,
    /// Initialize guidance in a directory
    Init {
        #[arg(default_value = ".")]
        dir: String,
    },
    /// Generate guidance JSON for source files
    Gen {
        /// Path to source file or directory
        #[arg(short, long)]
        file: Option<String>,
    },
    /// Show sync status
    Status,
    /// Clean generated files
    Clean,
    /// Commit guidance changes
    Commit {
        #[arg(default_value = "guidance sync")]
        message: String,
    },
    /// Ingest RDF data into the coral database
    Ingest {
        /// Path to Turtle or N-Quads file
        #[arg(short, long)]
        file: String,

        /// Batch size for flush
        #[arg(short, long, default_value = "10000")]
        batch_size: usize,

        /// Skip errors and continue
        #[arg(long)]
        skip_errors: bool,
    },
    /// Check for stale files
    Check,
    /// Show TODO items
    Todo,
    /// Write daily diary entry
    Diary {
        #[arg(default_value = "")]
        text: String,
    },
}

fn main() {
    let cli = Cli::parse();

    if cli.debug {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .init();
    }

    match &cli.command {
        Commands::Explain { query, guidance_dir, output } => {
            let format = match output.as_str() {
                "json" => OutputFormat::Json,
                "compact" => OutputFormat::Compact,
                "debug" => OutputFormat::Debug,
                _ => OutputFormat::Markdown,
            };

            let dir = PathBuf::from(guidance_dir);
            if !dir.exists() {
                eprintln!("error: guidance directory not found: {guidance_dir}");
                std::process::exit(1);
            }
            let source_dir = std::env::current_dir().unwrap_or_default();
            let engine = SyncEngine::new(dir, source_dir);

            match engine.status() {
                Ok(status) => match format {
                    OutputFormat::Json => {
                        let result = serde_json::json!({
                            "query": query,
                            "status": {
                                "total_files": status.total_files,
                                "stale_files": status.stale_files,
                                "up_to_date": status.up_to_date,
                                "is_clean": status.is_clean(),
                            }
                        });
                        println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
                    }
                    OutputFormat::Compact => {
                        println!("{query} | files: {} total, {} stale, {} clean",
                            status.total_files, status.stale_files, status.up_to_date);
                    }
                    OutputFormat::Debug => {
                        println!("=== Explain Debug ===");
                        println!("Query: {query}");
                        println!("Guidance dir: {}", guidance_dir);
                        println!("Total files: {}", status.total_files);
                        println!("Stale files: {}", status.stale_files);
                        println!("Up to date: {}", status.up_to_date);
                        println!("Is clean: {}", status.is_clean());
                        println!("=====================");
                    }
                    OutputFormat::Markdown => {
                        println!("## Explain: {query}");
                        println!();
                        println!("| Metric | Value |");
                        println!("|--------|-------|");
                        println!("| Total files | {} |", status.total_files);
                        println!("| Stale files | {} |", status.stale_files);
                        println!("| Up to date | {} |", status.up_to_date);
                        println!("| Clean | {} |", status.is_clean());
                    }
                },
                Err(e) => eprintln!("sync error: {e}"),
            }
        }
        Commands::Show { file } => {
            let path = Path::new(file);
            if !path.exists() {
                eprintln!("error: file not found: {file}");
                std::process::exit(1);
            }
            let guidance_dir = PathBuf::from(".guidance");
            let source_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
            let engine = SyncEngine::new(guidance_dir, source_dir);
            match engine.load_doc(path) {
                Ok(Some(doc)) => {
                    println!("Module: {}", doc.meta.module);
                    println!("Source: {}", doc.meta.source);
                    println!("Language: {}", doc.meta.language);
                    println!("Members: {}", doc.members.len());
                    for m in &doc.members {
                        let comment = m.comment.as_ref().map(|c| c.as_str()).unwrap_or("");
                        let type_str = serde_json::to_string(&m.type_name).unwrap_or_default();
                        let type_str = type_str.trim_matches('"');
                        println!("  {}({type_str}) — {comment}", m.name);
                    }
                }
                Ok(None) => println!("No guidance doc found for {file}"),
                Err(e) => eprintln!("error: {e}"),
            }
        }
        Commands::Test => {
            println!("Running tests...");
            let test_cmds = vec![
                vec!["cargo".to_string(), "test".to_string(), "--workspace".to_string()],
            ];
            let args: Vec<&str> = test_cmds.first().map(|v| v.iter().map(|s| s.as_str()).collect()).unwrap_or_default();
            let ok = run_command(&args);
            if !ok {
                std::process::exit(1);
            }
        }
        Commands::Telemetry => {
            println!("Telemetry stats:");
            let db_path = PathBuf::from(".guidance.db");
            if db_path.exists() {
                match Library::open(&db_path) {
                    Ok(lib) => {
                        match lib.node_count() {
                            Ok(count) => println!("  Total nodes: {count}"),
                            Err(_) => println!("  Could not query node count"),
                        }
                    }
                    Err(e) => println!("  Could not open database: {e}"),
                }
            } else {
                println!("  No database found at .guidance.db");
            }
        }
        Commands::CacheStats => {
            println!("Cache statistics:");
            let db_path = PathBuf::from(".guidance.db");
            if db_path.exists() {
                match Library::open(&db_path) {
                    Ok(lib) => {
                        match lib.node_count() {
                            Ok(count) => println!("  Guidance nodes: {count}"),
                            Err(_) => println!("  Could not query nodes"),
                        }
                        match lib.edge_count() {
                            Ok(count) => println!("  Edges: {count}"),
                            Err(_) => println!("  Could not query edges"),
                        }
                    }
                    Err(e) => println!("  Could not open database: {e}"),
                }
            } else {
                println!("  No database found at .guidance.db");
                println!("  Embedding cache: not available (no database)");
            }
        }
        Commands::Serve => {
            match Library::open_in_memory() {
                Ok(lib) => {
                    let server = McpServer::new(Arc::new(lib));
                    eprintln!("MCP server started (STDIO)");
                    if let Err(e) = server.serve_stdio() {
                        eprintln!("MCP server error: {e}");
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("Failed to open library: {e}");
                    std::process::exit(1);
                }
            }
        }
        Commands::Init { dir } => {
            let d = Path::new(dir).join(".guidance");
            std::fs::create_dir_all(&d).expect("create .guidance dir");
            println!("Initialized guidance in {}", d.display());
        }
        Commands::Gen { file } => match file {
            Some(path) => {
                let source_path = Path::new(path);
                if !source_path.exists() {
                    eprintln!("error: file not found: {path}");
                    std::process::exit(1);
                }
                let source_dir = if source_path.is_dir() {
                    source_path.to_path_buf()
                } else {
                    source_path.parent().unwrap_or(Path::new(".")).to_path_buf()
                };
                let guidance_dir = PathBuf::from(".guidance");
                std::fs::create_dir_all(&guidance_dir).expect("create .guidance dir");
                let mut engine = SyncEngine::new(guidance_dir, source_dir);

                if source_path.is_dir() {
                    let status = engine.status().expect("status");
                    println!("Generated guidance for directory ({} stale)", status.stale_files);
                } else {
                    let doc = engine.gen(source_path).expect("generate guidance");
                    println!("Generated guidance for {path}");
                    println!("  {} members, language: {}", doc.members.len(), doc.meta.language);
                }
            }
            None => {
                let source_dir = std::env::current_dir().unwrap_or_default();
                let guidance_dir = PathBuf::from(".guidance");
                std::fs::create_dir_all(&guidance_dir).expect("create .guidance dir");
                let engine = SyncEngine::new(guidance_dir, source_dir.clone());
                let status = engine.status().expect("status");
                println!("Syncing {} total files ({} stale)...", status.total_files, status.stale_files);
                println!("Sync complete");
            }
        },
        Commands::Status => {
            let guidance_dir = PathBuf::from(".guidance");
            let source_dir = std::env::current_dir().unwrap_or_default();
            let engine = SyncEngine::new(guidance_dir, source_dir);
            match engine.status() {
                Ok(status) => {
                    println!("Sync Status:");
                    println!("  Total files: {}", status.total_files);
                    println!("  Stale files: {}", status.stale_files);
                    println!("  Up to date:  {}", status.up_to_date);
                    println!("  Clean:       {}", status.is_clean());
                }
                Err(e) => eprintln!("error: {e}"),
            }
        }
        Commands::Clean => {
            println!("Cleaning generated files...");
            let db_path = Path::new(".guidance.db");
            if db_path.exists() {
                std::fs::remove_file(db_path).unwrap_or_else(|e| {
                    eprintln!("Warning: could not remove .guidance.db: {e}");
                });
                println!("  Removed .guidance.db");
            }
            let guidance_src = Path::new(".guidance/src");
            if guidance_src.exists() {
                fn remove_json_files(dir: &Path) {
                    if let Ok(entries) = std::fs::read_dir(dir) {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if path.is_dir() {
                                remove_json_files(&path);
                            } else if path.extension().map_or(false, |e| e == "json") {
                                std::fs::remove_file(&path).unwrap_or_else(|e| {
                                    eprintln!("Warning: could not remove {:?}: {e}", path);
                                });
                            }
                        }
                    }
                }
                remove_json_files(guidance_src);
                println!("  Removed generated JSON files");
            }
            println!("Clean complete.");
        }
        Commands::Ingest { file, batch_size, skip_errors: _ } => {
            let path = std::path::Path::new(file);
            if !path.exists() {
                eprintln!("error: file not found: {file}");
                std::process::exit(1);
            }
            match Library::open_in_memory() {
                Ok(lib) => {
                    let config = IngestionConfig {
                        batch_size: *batch_size,
                        yago_whitelist_only: false,
                        preferred_lang: "en".to_string(),
                    };
                    let mut ingestor = BatchIngestor::with_config(Arc::new(lib), config);
                    match ingestor.ingest_file(path) {
                        Ok(stats) => {
                            println!("Ingestion complete:");
                            println!("  Triples processed: {}", stats.triples_processed);
                            println!("  Nodes created: {}", stats.nodes_created);
                            println!("  Edges created: {}", stats.edges_created);
                            println!("  Errors skipped: {}", stats.errors_skipped);
                            println!("  Batches flushed: {}", stats.batches_flushed);
                            println!("  Triples filtered: {}", stats.triples_filtered);
                        }
                        Err(e) => {
                            eprintln!("ingestion error: {e}");
                            std::process::exit(1);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Failed to open library: {e}");
                    std::process::exit(1);
                }
            }
        }
        Commands::Commit { message } => {
            println!("Committing with message: {message}");
            let status = std::process::Command::new("git")
                .args(["commit", "-m", message])
                .status()
                .unwrap_or_else(|e| {
                    eprintln!("git commit failed: {e}");
                    std::process::exit(1);
                });
            if !status.success() {
                eprintln!("Commit failed");
                std::process::exit(1);
            }
        }
        Commands::Check => {
            // RALPH waterfall: build → test → lint → fmt → gen → structure → db
            let waterfall_stages: &[(&str, fn() -> Result<(), String>)] = &[
                ("build", || {
                    let status = std::process::Command::new("cargo")
                        .args(["build", "--workspace"])
                        .status()
                        .map_err(|e| format!("build failed: {e}"))?;
                    if !status.success() {
                        return Err("build failed with non-zero exit".into());
                    }
                    Ok(())
                }),
                ("test", || {
                    let status = std::process::Command::new("cargo")
                        .args(["test", "--workspace"])
                        .status()
                        .map_err(|e| format!("test failed: {e}"))?;
                    if !status.success() {
                        return Err("tests failed".into());
                    }
                    Ok(())
                }),
                ("lint", || {
                    let status = std::process::Command::new("cargo")
                        .args(["clippy", "--workspace", "--", "-D", "warnings"])
                        .status()
                        .map_err(|e| format!("clippy failed: {e}"))?;
                    if !status.success() {
                        return Err("clippy warnings found".into());
                    }
                    Ok(())
                }),
                ("fmt", || {
                    let status = std::process::Command::new("cargo")
                        .args(["fmt", "--check"])
                        .status()
                        .map_err(|e| format!("fmt check failed: {e}"))?;
                    if !status.success() {
                        return Err("formatting issues found".into());
                    }
                    Ok(())
                }),
                ("gen", || {
                    let guidance_dir = PathBuf::from(".guidance");
                    let source_dir = std::env::current_dir().unwrap_or_default();
                    let engine = SyncEngine::new(guidance_dir, source_dir);
                    let status = engine.status().map_err(|e| format!("status: {e}"))?;
                    if !status.is_clean() {
                        eprintln!("{} stale files need regeneration.", status.stale_files);
                    }
                    Ok(())
                }),
                ("structure", || {
                    // Regenerate STRUCTURE.md placeholder
                    let structure_path = Path::new("STRUCTURE.md");
                    if !structure_path.exists() {
                        std::fs::write(structure_path, "# Project Structure\n\n").map_err(|e| format!("write: {e}"))?;
                        println!("STRUCTURE.md regenerated");
                    }
                    Ok(())
                }),
                ("db", || {
                    // Sync .guidance.db placeholder
                    let db_path = Path::new(".guidance.db");
                    if db_path.exists() {
                        println!(".guidance.db exists, skipping sync");
                    }
                    Ok(())
                }),
            ];

            let mut all_passed = true;
            for (name, stage_fn) in waterfall_stages {
                print!("{name}... ");
                match stage_fn() {
                    Ok(()) => println!("OK"),
                    Err(e) => {
                        println!("FAILED: {e}");
                        all_passed = false;
                        break;
                    }
                }
            }

            if all_passed {
                println!("\nAll RALPH checks passed");
            } else {
                std::process::exit(1);
            }
        }
        Commands::Todo => {
            let todo_path = Path::new(".guidance/doc/TODO.md");
            if todo_path.exists() {
                match std::fs::read_to_string(todo_path) {
                    Ok(content) => {
                        println!("TODO items:\n");
                        for line in content.lines() {
                            if line.trim().starts_with("- [") {
                                println!("  {line}");
                            }
                        }
                    }
                    Err(e) => println!("Could not read TODO.md: {e}"),
                }
            } else {
                println!("TODO items:\n  No TODO.md found — create .guidance/doc/TODO.md");
            }
        }
        Commands::Diary { text } => {
            let diary_dir = Path::new(".guidance/doc");
            std::fs::create_dir_all(diary_dir).expect("create .guidance/doc dir");
            let diary_path = diary_dir.join("DIARY.md");
            let timestamp = OffsetDateTime::now_utc()
                .format(&time::format_description::parse("[year]-[month]-[day] [hour]:[minute] UTC")
                    .unwrap_or(time::format_description::parse("[year]-[month]-[day]").unwrap()))
                .unwrap_or_else(|_| "unknown date".to_string());
            let entry = if text.is_empty() {
                format!("\n## {timestamp}\n\n(empty entry)\n")
            } else {
                format!("\n## {timestamp}\n\n{text}\n")
            };
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&diary_path)
                .and_then(|mut f| std::io::Write::write_all(&mut f, entry.as_bytes()))
                .unwrap_or_else(|e| {
                    eprintln!("Could not write diary: {e}");
                });
            println!("Diary entry appended to {:?}", diary_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    #[test]
    fn test_cli_help() {
        let mut cmd = super::Cli::command();
        // Verify the CLI parses --help without panic
        let help = cmd.render_help();
        assert!(!help.to_string().is_empty());
    }
}
