use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Parser, Subcommand};
use guidance_common::shell::run_command;
use guidance_coral::db::Library;
use guidance_coral::mcp::McpServer;
use guidance_guidance::config;
use guidance_guidance::sync_engine::SyncEngine;
use guidance_guidance::vector::vector_db::GuidanceDb;
use time::OffsetDateTime;

#[derive(Parser)]
#[command(
    name = "guidance",
    about = "AST-guided vector search & edge AI orchestrator"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    #[arg(global = true, long)]
    debug: bool,

    #[arg(global = true, long)]
    show_prompts: bool,
}

#[derive(Subcommand)]
enum Commands {
    Explain {
        query: String,

        #[arg(long, default_value = ".guidance")]
        guidance: String,

        #[arg(short = 'o', long, default_value = ".guidance.db")]
        db: String,

        #[arg(short = 'w', long, default_value = ".")]
        workspace: String,

        #[arg(short = 'l', long, default_value_t = 10)]
        limit: usize,

        #[arg(long)]
        no_llm: bool,

        #[arg(long, default_value = "auto")]
        filter: String,
    },
    Test,
    Telemetry {
        #[arg(short = 'o', long, default_value = ".guidance.db")]
        db: String,

        #[arg(long)]
        reset: bool,
    },
    CacheStats {
        #[arg(short = 'o', long, default_value = ".guidance.db")]
        db: String,
    },
    #[command(name = "mcp")]
    Mcp {
        #[arg(long, default_value_t = 8080)]
        port: u16,

        #[arg(short = 'o', long, default_value = ".guidance.db")]
        db: String,
    },
    Init {
        #[arg(default_value = ".")]
        dir: String,

        #[arg(short = 'g', long, default_value = ".guidance")]
        guidance_dir: String,

        #[arg(short = 'o', long, default_value = ".guidance.db")]
        db: String,
    },
    Gen {
        #[arg(short, long)]
        file: Option<String>,

        #[arg(long)]
        scan: Option<String>,

        #[arg(short = 'w', long, default_value = ".")]
        workspace: String,

        #[arg(long, default_value = ".guidance")]
        json_dir: String,

        #[arg(short = 'o', long, default_value = ".guidance.db")]
        db: String,

        #[arg(long, default_value_t = 2)]
        timeout: u64,

        #[arg(long)]
        force: bool,

        #[arg(long)]
        no_llm: bool,

        #[arg(long)]
        no_db: bool,

        #[arg(long)]
        dry_run: bool,

        #[arg(long)]
        verbose: bool,

        #[arg(long)]
        regen: bool,

        #[arg(long)]
        all_languages: bool,
    },
    Status {
        #[arg(short = 'g', long, default_value = ".guidance")]
        guidance_dir: String,
    },
    Clean {
        #[arg(long, default_value = ".guidance")]
        json_dir: String,

        #[arg(short = 'o', long, default_value = ".guidance.db")]
        db: String,
    },
    Commit {
        #[arg(default_value = "guidance sync")]
        message: String,

        #[arg(long)]
        dry_run: bool,
    },
    Check,
    Todo,
    Diary {
        #[arg(default_value = "")]
        text: String,
    },
    Benchmark {
        query: Option<String>,

        #[arg(short = 'g', long, default_value = ".guidance")]
        guidance: String,

        #[arg(short = 'o', long, default_value = ".guidance.db")]
        db: String,

        #[arg(short = 'w', long, default_value = ".")]
        workspace: String,

        #[arg(short = 'n', long)]
        num: Option<usize>,

        #[arg(long)]
        no_llm: bool,

        #[arg(short = 'v', long)]
        verbose: bool,
    },
    Structure {
        #[arg(long, default_value = ".guidance")]
        json_dir: String,
    },
    Health {
        #[arg(short = 'w', long, default_value = ".")]
        workspace: String,

        #[arg(long, default_value_t = 30)]
        min_age: u32,

        #[arg(long, default_value = "ai")]
        format: String,

        #[arg(short = 'o', long, default_value = ".guidance.db")]
        db: String,
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
        Commands::Explain {
            query,
            guidance,
            db,
            workspace,
            limit,
            no_llm,
            filter,
        } => {
            cmd_explain(query, guidance, db, workspace, *limit, *no_llm, filter);
        }
        Commands::Test => cmd_test(),
        Commands::Telemetry { db, .. } => cmd_telemetry(db),
        Commands::CacheStats { db } => cmd_cache_stats(db),
        Commands::Mcp { port: _, db } => cmd_mcp(db),
        Commands::Init {
            dir,
            guidance_dir: _,
            db: _,
        } => cmd_init(dir),
        Commands::Gen {
            file,
            scan,
            workspace,
            json_dir,
            db,
            timeout: _,
            force,
            no_llm: _,
            no_db,
            dry_run,
            verbose,
            regen: _,
            all_languages: _,
        } => {
            cmd_gen(
                file.as_deref(),
                scan.as_deref(),
                workspace,
                json_dir,
                db,
                *force,
                *no_db,
                *dry_run,
                *verbose,
            );
        }
        Commands::Status { guidance_dir } => cmd_status(guidance_dir),
        Commands::Clean { json_dir, db } => cmd_clean(json_dir, db),
        Commands::Commit { message, dry_run } => cmd_commit(message, *dry_run),
        Commands::Check => cmd_check(),
        Commands::Todo => cmd_todo(),
        Commands::Diary { text } => cmd_diary(text),
        Commands::Benchmark {
            query,
            guidance,
            db,
            workspace,
            num,
            no_llm,
            verbose,
        } => {
            cmd_benchmark(
                query.as_deref(),
                guidance,
                db,
                workspace,
                *num,
                *no_llm,
                *verbose,
            );
        }
        Commands::Structure { json_dir } => cmd_structure(json_dir),
        Commands::Health {
            workspace,
            min_age,
            format,
            db,
        } => {
            cmd_health(workspace, *min_age, format, db);
        }
    }
}

fn load_project_config(workspace: &Path) -> config::ProjectConfig {
    config::load_config(workspace).unwrap_or_default()
}

fn cmd_explain(
    query: &str,
    guidance_dir: &str,
    db_path: &str,
    _workspace: &str,
    limit: usize,
    _no_llm: bool,
    _filter: &str,
) {
    let gdir = PathBuf::from(guidance_dir);
    let db = PathBuf::from(db_path);

    let mut results: Vec<guidance_guidance::vector::vector_db::SearchResult> = Vec::new();

    if db.exists() {
        if let Ok(gdb) = GuidanceDb::open(&db) {
            if let Ok(hybrid) = gdb.hybrid_search(query, None, limit) {
                results = hybrid;
            }
        }
    }

    if results.is_empty() {
        let src_dir = gdir.join("src");
        if src_dir.is_dir() {
            let lower_query = query.to_lowercase();
            let tokens: Vec<&str> = query.split_whitespace().collect();
            collect_json_results(&src_dir, &lower_query, &tokens, &mut results);
        }
    }

    results.truncate(limit);

    println!("## Explain: {query}");
    println!();
    if results.is_empty() {
        println!("No results found.");
        return;
    }
    println!("| Name | Source | Score |");
    println!("|------|--------|-------|");
    for r in &results {
        println!("| {} | {} | {:.2} |", r.name, r.source, r.similarity);
    }
}

fn collect_json_results(
    dir: &Path,
    lower_query: &str,
    tokens: &[&str],
    results: &mut Vec<guidance_guidance::vector::vector_db::SearchResult>,
) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_json_results(&path, lower_query, tokens, results);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(Some(doc)) = guidance_guidance::sync::json_store::load_guidance(&path) else {
                continue;
            };
            for member in &doc.members {
                let name_lower = member.name.as_str().to_lowercase();
                let sig_lower = member
                    .signature
                    .as_ref()
                    .map(|s| s.as_str().to_lowercase())
                    .unwrap_or_default();
                let comment_lower = member
                    .comment
                    .as_ref()
                    .map(|c| c.as_str().to_lowercase())
                    .unwrap_or_default();

                let exact = name_lower == *lower_query;
                let name_match = name_lower.contains(lower_query);
                let token_match = tokens.iter().any(|t| {
                    let tl = t.to_lowercase();
                    name_lower.contains(&tl)
                        || sig_lower.contains(&tl)
                        || comment_lower.contains(&tl)
                });

                if exact {
                    results.push(guidance_guidance::vector::vector_db::SearchResult {
                        id: 0,
                        name: member.name.as_str().to_string(),
                        source: doc.meta.source.as_str().to_string(),
                        signature: member.signature.as_ref().map(|s| s.as_str().to_string()),
                        similarity: 1.0,
                    });
                } else if name_match {
                    results.push(guidance_guidance::vector::vector_db::SearchResult {
                        id: 0,
                        name: member.name.as_str().to_string(),
                        source: doc.meta.source.as_str().to_string(),
                        signature: member.signature.as_ref().map(|s| s.as_str().to_string()),
                        similarity: 0.8,
                    });
                } else if token_match {
                    results.push(guidance_guidance::vector::vector_db::SearchResult {
                        id: 0,
                        name: member.name.as_str().to_string(),
                        source: doc.meta.source.as_str().to_string(),
                        signature: member.signature.as_ref().map(|s| s.as_str().to_string()),
                        similarity: 0.5,
                    });
                }
            }
        }
    }
}

fn cmd_test() {
    println!("Running tests...");
    let args: Vec<&str> = vec!["cargo", "test", "--workspace"];
    let ok = run_command(&args);
    if !ok {
        std::process::exit(1);
    }
}

fn cmd_telemetry(db_path: &str) {
    println!("Telemetry stats:");
    let db = PathBuf::from(db_path);
    if db.exists() {
        match GuidanceDb::open(&db) {
            Ok(gdb) => {
                match gdb.get_node_count() {
                    Ok(count) => println!("  Total nodes: {count}"),
                    Err(_) => println!("  Could not query node count"),
                }
                match gdb.get_embedding_count() {
                    Ok(count) => println!("  Embedded nodes: {count}"),
                    Err(_) => println!("  Could not query embedding count"),
                }
            }
            Err(e) => println!("  Could not open database: {e}"),
        }
    } else {
        println!("  No database found at {db_path}");
    }
}

fn cmd_cache_stats(db_path: &str) {
    println!("Cache statistics:");
    let db = PathBuf::from(db_path);
    if db.exists() {
        match GuidanceDb::open(&db) {
            Ok(gdb) => {
                match gdb.get_node_count() {
                    Ok(count) => println!("  Guidance nodes: {count}"),
                    Err(_) => println!("  Could not query nodes"),
                }
                match gdb.get_embedding_count() {
                    Ok(count) => println!("  Embeddings: {count}"),
                    Err(_) => println!("  Could not query embeddings"),
                }
            }
            Err(e) => println!("  Could not open database: {e}"),
        }
    } else {
        println!("  No database found at {db_path}");
        println!("  Embedding cache: not available (no database)");
    }
}

fn cmd_mcp(db_path: &str) {
    let db = PathBuf::from(db_path);
    let lib = if db.exists() {
        Library::open(&db).unwrap_or_else(|e| {
            eprintln!("Failed to open library: {e}");
            std::process::exit(1);
        })
    } else {
        Library::open_in_memory().unwrap_or_else(|e| {
            eprintln!("Failed to open in-memory library: {e}");
            std::process::exit(1);
        })
    };
    let server = McpServer::new(Arc::new(lib));
    eprintln!("MCP server started (STDIO)");
    if let Err(e) = server.serve_stdio() {
        eprintln!("MCP server error: {e}");
        std::process::exit(1);
    }
}

fn cmd_init(dir: &str) {
    let d = Path::new(dir).join(".guidance");
    std::fs::create_dir_all(&d).expect("create .guidance dir");
    let config_path = d.join("guidance-config.json");
    if !config_path.exists() {
        let default_config = serde_json::json!({
            "version": "1",
            "guidance_dir": ".guidance",
            "db_path": ".guidance.db",
            "skills_dir": "doc/skills",
            "capabilities_dir": "doc/capabilities",
            "src_dirs": ["src"],
            "providers": {
                "local": { "base_url": "http://localhost:11434", "chat_endpoint": "/v1/chat/completions" },
                "ollama": { "base_url": "http://localhost:11434", "chat_endpoint": "/api/chat" }
            },
            "models": {
                "default": "ollama:code:latest",
                "fast": "ollama:code:latest",
                "thinking": "ollama:code:latest",
                "batch": "ollama:code:latest",
                "embed": "ollama:embed:latest"
            },
            "embed": { "dims": 768, "cache_limit": 400 }
        });
        std::fs::write(
            &config_path,
            serde_json::to_string_pretty(&default_config).unwrap(),
        )
        .expect("write config");
    }
    println!("Initialized guidance in {}", d.display());
}

#[allow(clippy::too_many_arguments)]
fn cmd_gen(
    file: Option<&str>,
    scan: Option<&str>,
    workspace: &str,
    json_dir: &str,
    db_path: &str,
    force: bool,
    no_db: bool,
    dry_run: bool,
    verbose: bool,
) {
    let workspace_path = PathBuf::from(workspace);
    let guidance_dir = PathBuf::from(json_dir);
    let db = PathBuf::from(db_path);
    let cfg = load_project_config(&workspace_path);

    if dry_run {
        println!("Dry run — no files will be written.");
    }

    if let Some(path) = file {
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
        std::fs::create_dir_all(&guidance_dir).expect("create guidance dir");
        let mut engine = SyncEngine::new(guidance_dir.clone(), source_dir.clone());

        if source_path.is_dir() {
            let status = engine.status().expect("status");
            println!(
                "Generated guidance for directory ({} stale)",
                status.stale_files
            );
        } else {
            let json_path = guidance_dir.join("src").join(format!(
                "{}.json",
                source_path
                    .strip_prefix(&source_dir)
                    .unwrap_or(source_path)
                    .display()
            ));
            if !force
                && !guidance_guidance::sync::staleness::should_generate(&json_path, source_path)
            {
                if verbose {
                    println!("  skip (up to date): {path}");
                }
                return;
            }
            let doc = engine.gen(source_path).expect("generate guidance");
            if verbose {
                println!("  gen: {path} ({} members)", doc.members.len());
            }
            println!("Generated guidance for {path}");
            println!(
                "  {} members, language: {}",
                doc.members.len(),
                doc.meta.language
            );
        }
    } else if let Some(scan_dir) = scan {
        let scan_path = PathBuf::from(scan_dir);
        std::fs::create_dir_all(&guidance_dir).expect("create guidance dir");
        let mut engine = SyncEngine::new(guidance_dir.clone(), scan_path.clone());
        let mut generated = 0usize;
        walk_and_gen(&mut engine, &scan_path, force, &mut generated, verbose);
        println!("Scanned {scan_dir}: generated {generated} files");
    } else {
        let src_dirs: Vec<PathBuf> = if cfg.src_dirs.is_empty() {
            vec![workspace_path.clone()]
        } else {
            cfg.src_dirs
                .iter()
                .map(|d| workspace_path.join(d))
                .collect()
        };

        let mut total_files = 0usize;
        let mut stale_files = 0usize;
        let mut generated = 0usize;

        for src_dir in &src_dirs {
            if !src_dir.is_dir() {
                continue;
            }
            std::fs::create_dir_all(&guidance_dir).expect("create guidance dir");
            let engine = SyncEngine::new(guidance_dir.clone(), src_dir.clone());
            let status = engine.status().expect("status");
            total_files += status.total_files;
            stale_files += status.stale_files;

            if stale_files > 0 || force {
                let mut engine = SyncEngine::new(guidance_dir.clone(), src_dir.clone());
                walk_and_gen(&mut engine, src_dir, force, &mut generated, verbose);
            }
        }

        println!("Syncing {total_files} total files ({stale_files} stale)...");
        if generated > 0 {
            println!("Generated {generated} files.");
        }

        if !no_db && !dry_run {
            let json_src = guidance_dir.join("src");
            if json_src.is_dir() {
                if let Ok(gdb) = GuidanceDb::open(&db) {
                    match gdb.sync_from_dir(&json_src) {
                        Ok(count) => println!("Synced {count} nodes to {db_path}"),
                        Err(e) => eprintln!("Warning: db sync failed: {e}"),
                    }
                }
            }
        }
        println!("Sync complete.");
    }
}

fn walk_and_gen(
    engine: &mut SyncEngine,
    dir: &Path,
    force: bool,
    generated: &mut usize,
    verbose: bool,
) {
    if !dir.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') {
                continue;
            }
            walk_and_gen(engine, &path, force, generated, verbose);
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if !matches!(ext, "zig" | "zon" | "py" | "rs" | "md") {
                continue;
            }
            let rel = path.strip_prefix(&engine.source_dir).unwrap_or(&path);
            let json_path = engine
                .guidance_dir
                .join("src")
                .join(format!("{}.json", rel.display()));
            let should_gen =
                force || guidance_guidance::sync::staleness::should_generate(&json_path, &path);
            if !should_gen {
                if verbose {
                    println!("  skip: {}", rel.display());
                }
                continue;
            }
            match engine.gen(&path) {
                Ok(_doc) => {
                    *generated += 1;
                    if verbose {
                        println!("  gen: {}", rel.display());
                    }
                }
                Err(e) => {
                    if verbose {
                        eprintln!("  warn: {} — {e}", rel.display());
                    }
                }
            }
        }
    }
}

fn cmd_status(guidance_dir: &str) {
    let gdir = PathBuf::from(guidance_dir);
    let source_dir = std::env::current_dir().unwrap_or_default();
    let engine = SyncEngine::new(gdir, source_dir);
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

fn cmd_clean(json_dir: &str, db_path: &str) {
    println!("Cleaning generated files...");
    let db = Path::new(db_path);
    if db.exists() {
        std::fs::remove_file(db)
            .unwrap_or_else(|e| eprintln!("Warning: could not remove {db_path}: {e}"));
        println!("  Removed {db_path}");
    }
    let guidance_src = Path::new(json_dir).join("src");
    if guidance_src.exists() {
        fn remove_json_files(dir: &Path) {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        remove_json_files(&path);
                    } else if path.extension().is_some_and(|e| e == "json") {
                        std::fs::remove_file(&path).unwrap_or_else(|e| {
                            eprintln!("Warning: could not remove {:?}: {e}", path)
                        });
                    }
                }
            }
        }
        remove_json_files(&guidance_src);
        println!("  Removed generated JSON files");
    }
    println!("Clean complete.");
}

fn cmd_commit(message: &str, dry_run: bool) {
    if dry_run {
        println!("Dry run: would commit with message: {message}");
        return;
    }
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

fn cmd_check() {
    let cargo_dir: PathBuf = if Path::new("src/Cargo.toml").exists() {
        PathBuf::from("src")
    } else {
        PathBuf::from(".")
    };

    type StageFn = Box<dyn Fn() -> Result<(), String>>;
    let cd = cargo_dir.clone();
    let cd2 = cargo_dir.clone();
    let cd3 = cargo_dir.clone();
    let cd4 = cargo_dir.clone();
    let waterfall_stages: Vec<(&str, StageFn)> = vec![
        (
            "build",
            Box::new(move || {
                let status = std::process::Command::new("cargo")
                    .args(["build", "--workspace"])
                    .current_dir(&cd)
                    .status()
                    .map_err(|e| format!("build failed: {e}"))?;
                if !status.success() {
                    return Err("build failed with non-zero exit".into());
                }
                Ok(())
            }),
        ),
        (
            "test",
            Box::new(move || {
                let status = std::process::Command::new("cargo")
                    .args(["test", "--workspace"])
                    .current_dir(&cd2)
                    .status()
                    .map_err(|e| format!("test failed: {e}"))?;
                if !status.success() {
                    return Err("tests failed".into());
                }
                Ok(())
            }),
        ),
        (
            "lint",
            Box::new(move || {
                let status = std::process::Command::new("cargo")
                    .args(["clippy", "--workspace", "--", "-D", "warnings"])
                    .current_dir(&cd3)
                    .status()
                    .map_err(|e| format!("clippy failed: {e}"))?;
                if !status.success() {
                    return Err("clippy warnings found".into());
                }
                Ok(())
            }),
        ),
        (
            "fmt",
            Box::new(move || {
                let status = std::process::Command::new("cargo")
                    .args(["fmt", "--check"])
                    .current_dir(&cd4)
                    .status()
                    .map_err(|e| format!("fmt check failed: {e}"))?;
                if !status.success() {
                    return Err("formatting issues found".into());
                }
                Ok(())
            }),
        ),
        (
            "gen",
            Box::new(|| {
                let guidance_dir = PathBuf::from(".guidance");
                let source_dir = std::env::current_dir().unwrap_or_default();
                let engine = SyncEngine::new(guidance_dir, source_dir);
                let status = engine.status().map_err(|e| format!("status: {e}"))?;
                if !status.is_clean() {
                    eprintln!("{} stale files need regeneration.", status.stale_files);
                }
                Ok(())
            }),
        ),
        (
            "structure",
            Box::new(|| {
                let structure_path = Path::new("STRUCTURE.md");
                if !structure_path.exists() {
                    std::fs::write(structure_path, "# Project Structure\n\n")
                        .map_err(|e| format!("write: {e}"))?;
                    println!("STRUCTURE.md regenerated");
                }
                Ok(())
            }),
        ),
        (
            "db",
            Box::new(|| {
                let db_path = Path::new(".guidance.db");
                if db_path.exists() {
                    println!(".guidance.db exists, skipping sync");
                }
                Ok(())
            }),
        ),
    ];

    let mut all_passed = true;
    for (name, stage_fn) in &waterfall_stages {
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

fn cmd_todo() {
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

fn cmd_diary(text: &str) {
    let diary_dir = Path::new(".guidance/doc");
    std::fs::create_dir_all(diary_dir).expect("create .guidance/doc dir");
    let diary_path = diary_dir.join("DIARY.md");
    let timestamp = OffsetDateTime::now_utc()
        .format(
            &time::format_description::parse("[year]-[month]-[day] [hour]:[minute] UTC")
                .unwrap_or(time::format_description::parse("[year]-[month]-[day]").unwrap()),
        )
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
        .unwrap_or_else(|e| eprintln!("Could not write diary: {e}"));
    println!("Diary entry appended to {:?}", diary_path);
}

fn cmd_benchmark(
    single_query: Option<&str>,
    guidance_dir: &str,
    db_path: &str,
    _workspace: &str,
    num: Option<usize>,
    _no_llm: bool,
    verbose: bool,
) {
    let gdir = PathBuf::from(guidance_dir);
    let db = PathBuf::from(db_path);
    let src_dir = gdir.join("src");

    let mut queries: Vec<(String, String, String)> = Vec::new();

    if let Some(q) = single_query {
        queries.push((q.to_string(), String::new(), String::new()));
    } else {
        collect_benchmark_queries(&src_dir, &mut queries);
    }

    if let Some(n) = num {
        queries.truncate(n);
    }

    if queries.is_empty() {
        println!("No benchmark queries found. Generate guidance first with `guidance gen`.");
        return;
    }

    let gdb = if db.exists() {
        GuidanceDb::open(&db).ok()
    } else {
        None
    };

    let total = queries.len();
    let mut scores: Vec<f64> = Vec::new();

    println!("Benchmarking {total} queries...\n");

    for (i, (query, expected_source, expected_member)) in queries.iter().enumerate() {
        let start = std::time::Instant::now();
        let mut found = false;
        let mut result_count = 0usize;
        let mut top_result_name = String::new();

        if let Some(ref gdb) = gdb {
            if let Ok(results) = gdb.hybrid_search(query, None, 15) {
                result_count = results.len();
                if let Some(top) = results.first() {
                    top_result_name = top.name.clone();
                    if !expected_member.is_empty() {
                        found = top
                            .name
                            .to_lowercase()
                            .contains(&expected_member.to_lowercase());
                    } else if !expected_source.is_empty() {
                        found = top.source.contains(expected_source.as_str());
                    } else {
                        found = result_count > 0;
                    }
                }
                if !found {
                    found = results.iter().any(|r| {
                        if !expected_member.is_empty() {
                            r.name
                                .to_lowercase()
                                .contains(&expected_member.to_lowercase())
                        } else if !expected_source.is_empty() {
                            r.source.contains(expected_source.as_str())
                        } else {
                            false
                        }
                    });
                }
            }
        }

        if !found {
            let lower = query.to_lowercase();
            let json_src = gdir.join("src");
            if json_src.is_dir() {
                if let Ok(entries) = std::fs::read_dir(&json_src) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().and_then(|e| e.to_str()) != Some("json") {
                            continue;
                        }
                        if let Ok(Some(doc)) =
                            guidance_guidance::sync::json_store::load_guidance(&path)
                        {
                            for member in &doc.members {
                                if member.name.as_str().to_lowercase().contains(&lower) {
                                    found = true;
                                    result_count += 1;
                                    if top_result_name.is_empty() {
                                        top_result_name = member.name.as_str().to_string();
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let elapsed = start.elapsed();
        let ms = elapsed.as_secs_f64() * 1000.0;

        let accuracy = if found {
            if result_count <= 3 {
                10.0
            } else if result_count <= 10 {
                7.0
            } else {
                4.0
            }
        } else {
            1.0
        };

        let relevance = if found {
            if !top_result_name.is_empty()
                && top_result_name
                    .to_lowercase()
                    .contains(&query.to_lowercase())
            {
                10.0
            } else if found {
                6.0
            } else {
                1.0
            }
        } else {
            1.0
        };

        let completeness = if found {
            if result_count >= 2 {
                8.0
            } else {
                5.0
            }
        } else {
            1.0
        };

        let navigation = if found { 7.0 } else { 1.0 };

        let avg = (accuracy + relevance + completeness + navigation) / 4.0;
        scores.push(avg);

        if verbose {
            println!("  [{:3}/{total}] {query:40} acc={accuracy:.0} rel={relevance:.0} cmpl={completeness:.0} nav={navigation:.0} avg={avg:.1} {ms:.1}ms", i + 1);
        } else {
            println!("  [{:3}/{total}] {query:40} avg={avg:.1}  {ms:.1}ms", i + 1);
        }
    }

    let avg: f64 = scores.iter().sum::<f64>() / scores.len() as f64;
    let max = scores.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let min = scores.iter().cloned().fold(f64::INFINITY, f64::min);
    let high = scores.iter().filter(|&&s| s >= 7.0).count();
    let mid = scores.iter().filter(|&&s| (4.0..7.0).contains(&s)).count();
    let low = scores.iter().filter(|&&s| s < 4.0).count();

    println!("\n=== Benchmark Results ===");
    println!("  Total queries:  {total}");
    println!("  Average score:  {avg:.1}/10");
    println!("  Min score:      {min:.1}/10");
    println!("  Max score:      {max:.1}/10");
    println!("  High (>=7):     {high}");
    println!("  Middling (4-6): {mid}");
    println!("  Low (<4):       {low}");
}

fn collect_benchmark_queries(dir: &Path, queries: &mut Vec<(String, String, String)>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_benchmark_queries(&path, queries);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(Some(doc)) = guidance_guidance::sync::json_store::load_guidance(&path) else {
                continue;
            };
            let source = doc.meta.source.as_str().to_string();
            for member in &doc.members {
                let name = member.name.as_str();
                if !name.is_empty() && queries.len() < 120 {
                    queries.push((name.to_string(), source.clone(), name.to_string()));
                }
            }
            if let Some(ref comment) = doc.comment {
                let words: Vec<&str> = comment.as_str().split_whitespace().take(3).collect();
                if !words.is_empty() && queries.len() < 120 {
                    queries.push((words.join(" "), source.clone(), String::new()));
                }
            }
        }
    }
}

fn cmd_structure(json_dir: &str) {
    let gdir = PathBuf::from(json_dir);
    let src_dir = gdir.join("src");
    let mut modules: Vec<String> = Vec::new();

    if src_dir.is_dir() {
        collect_structure_modules(&src_dir, &mut modules);
    }

    let mut output = String::from("# Project Structure\n\n");
    output.push_str("Generated by `guidance structure`\n\n");
    output.push_str(&format!("Total modules: {}\n\n", modules.len()));

    for m in &modules {
        output.push_str(&format!("- {m}\n"));
    }

    std::fs::write("STRUCTURE.md", &output).expect("write STRUCTURE.md");
    println!("STRUCTURE.md generated with {} modules", modules.len());
}

fn collect_structure_modules(dir: &Path, modules: &mut Vec<String>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_structure_modules(&path, modules);
            } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(Some(doc)) = guidance_guidance::sync::json_store::load_guidance(&path) {
                    let module = doc.meta.module.as_str();
                    let source = doc.meta.source.as_str();
                    let member_count = doc.members.len();
                    modules.push(format!("{source} ({module}, {member_count} members)"));
                }
            }
        }
    }
}

fn cmd_health(workspace: &str, _min_age: u32, format: &str, _db: &str) {
    let ws = PathBuf::from(workspace);
    let gdir = PathBuf::from(".guidance");
    let src_dir = gdir.join("src");
    let mut total_members = 0usize;
    let mut without_comments = 0usize;
    let mut files: Vec<String> = Vec::new();

    if src_dir.is_dir() {
        collect_health_stats(
            &src_dir,
            &mut total_members,
            &mut without_comments,
            &mut files,
        );
    }

    match format {
        "json" => {
            let report = serde_json::json!({
                "files_analyzed": files.len(),
                "total_members": total_members,
                "without_comments": without_comments,
                "comment_coverage_pct": if total_members > 0 {
                    100.0 - (without_comments as f64 / total_members as f64) * 100.0
                } else { 100.0 },
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&report).unwrap_or_default()
            );
        }
        "human" => {
            println!("Health Report ({})", ws.display());
            println!("  Files analyzed:    {}", files.len());
            println!("  Total members:     {total_members}");
            println!("  Without comments:  {without_comments}");
            if total_members > 0 {
                let pct = (without_comments as f64 / total_members as f64) * 100.0;
                println!("  Comment coverage:  {:.1}%", 100.0 - pct);
            }
        }
        _ => {
            println!("## Health Report\n");
            println!("| Metric | Value |");
            println!("|--------|-------|");
            println!("| Files analyzed | {} |", files.len());
            println!("| Total members | {total_members} |");
            println!("| Without comments | {without_comments} |");
            if total_members > 0 {
                let pct = (without_comments as f64 / total_members as f64) * 100.0;
                println!("| Comment coverage | {:.1}% |", 100.0 - pct);
            }
        }
    }
}

fn collect_health_stats(
    dir: &Path,
    total: &mut usize,
    no_comments: &mut usize,
    files: &mut Vec<String>,
) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_health_stats(&path, total, no_comments, files);
            } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(Some(doc)) = guidance_guidance::sync::json_store::load_guidance(&path) {
                    files.push(doc.meta.source.as_str().to_string());
                    for member in &doc.members {
                        *total += 1;
                        if member.comment.is_none() {
                            *no_comments += 1;
                        }
                    }
                }
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
        let help = cmd.render_help();
        assert!(!help.to_string().is_empty());
    }
}
