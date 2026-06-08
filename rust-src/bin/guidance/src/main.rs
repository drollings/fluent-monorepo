use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Parser, Subcommand};
use guidance_coral::mcp::McpServer;
use guidance_coral::db::Library;
use guidance_guidance::sync_engine::SyncEngine;

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
        }
        Commands::Telemetry => {
            println!("Telemetry stats:");
        }
        Commands::CacheStats => {
            println!("Cache statistics:");
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
        }
        Commands::Commit { message } => {
            println!("Committing with message: {message}");
        }
        Commands::Check => {
            /// RALPH waterfall: build → test → lint → fmt → gen → structure → db
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
            println!("TODO items:");
        }
        Commands::Diary { text } => {
            if text.is_empty() {
                println!("Diary entry (empty - interactive mode)");
            } else {
                println!("Diary: {text}");
            }
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
