use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use common_core::shell::run_command;
use guidance_core::config;
use guidance_core::runtime;
use guidance_core::sync::json_store::walk_guidance_docs;
use guidance_core::sync_engine::SyncEngine;
use guidance_search_vector::GuidanceDb;
use time::OffsetDateTime;
use tokio::sync::oneshot;

use notify::{Config as NotifyConfig, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::sync::mpsc;

mod structure;

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
    Init {
        #[arg(default_value = ".")]
        dir: String,

        #[arg(short = 'g', long, default_value = ".guidance")]
        guidance_dir: String,

        #[arg(short = 'o', long, default_value = ".guidance.db")]
        db: String,
    },
    Sync {
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

        #[arg(long)]
        force: bool,

        #[arg(long)]
        no_db: bool,

        #[arg(long)]
        dry_run: bool,

        #[arg(long)]
        verbose: bool,

        #[arg(long)]
        watch: bool,

        #[arg(long, default_value_t = 500)]
        watch_debounce_ms: u64,
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
    Check {
        #[arg(short = 'w', long, default_value = ".")]
        workspace: String,
    },
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

#[tokio::main]
async fn main() {
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
        Commands::Telemetry { db, .. } => cmd_db_stats(db, "Telemetry stats"),
        Commands::CacheStats { db } => cmd_db_stats(db, "Cache statistics"),
        Commands::Init {
            dir,
            guidance_dir: _,
            db: _,
        } => cmd_init(dir),
        Commands::Sync {
            file,
            scan,
            workspace,
            json_dir,
            db,
            force,
            no_db,
            dry_run,
            verbose,
            watch,
            watch_debounce_ms,
        } => {
            cmd_sync(
                file.as_deref(),
                scan.as_deref(),
                workspace,
                json_dir,
                db,
                *force,
                *no_db,
                *dry_run,
                *verbose,
                *watch,
                *watch_debounce_ms,
            )
            .await;
        }
        Commands::Status { guidance_dir } => cmd_status(guidance_dir),
        Commands::Clean { json_dir, db } => cmd_clean(json_dir, db),
        Commands::Commit { message, dry_run } => cmd_commit(message, *dry_run),
        Commands::Check { workspace } => cmd_check(workspace),
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

fn collect_extensions(dirs: &[PathBuf]) -> std::collections::HashSet<String> {
    let mut exts = std::collections::HashSet::new();
    for dir in dirs {
        if !dir.is_dir() {
            continue;
        }
        collect_extensions_recursive(dir, &mut exts);
    }
    exts
}

fn collect_extensions_recursive(dir: &Path, exts: &mut std::collections::HashSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.starts_with('.') && name != "target" && name != "fixtures" {
                collect_extensions_recursive(&path, exts);
            }
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            exts.insert(format!(".{ext}"));
        }
    }
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

    let mut results: Vec<guidance_search_vector::db::SearchResult> = Vec::new();

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
    results: &mut Vec<guidance_search_vector::db::SearchResult>,
) {
    for (_path, doc) in walk_guidance_docs(dir) {
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
                name_lower.contains(&tl) || sig_lower.contains(&tl) || comment_lower.contains(&tl)
            });

            if exact {
                results.push(guidance_search_vector::db::SearchResult {
                    id: 0,
                    name: member.name.as_str().to_string(),
                    source: doc.meta.source.as_str().to_string(),
                    signature: member.signature.as_ref().map(|s| s.as_str().to_string()),
                    similarity: 1.0,
                });
            } else if name_match {
                results.push(guidance_search_vector::db::SearchResult {
                    id: 0,
                    name: member.name.as_str().to_string(),
                    source: doc.meta.source.as_str().to_string(),
                    signature: member.signature.as_ref().map(|s| s.as_str().to_string()),
                    similarity: 0.8,
                });
            } else if token_match {
                results.push(guidance_search_vector::db::SearchResult {
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

fn cmd_test() {
    println!("Running tests...");
    let args: Vec<&str> = vec!["cargo", "test", "--workspace"];
    let ok = run_command(&args);
    if !ok {
        std::process::exit(1);
    }
}

fn cmd_db_stats(db_path: &str, label: &str) {
    println!("{label}:");
    let db = PathBuf::from(db_path);
    if db.exists() {
        match GuidanceDb::open(&db) {
            Ok(gdb) => {
                match gdb.get_node_count() {
                    Ok(count) => println!("  Nodes: {count}"),
                    Err(_) => println!("  Could not query node count"),
                }
                match gdb.get_embedding_count() {
                    Ok(count) => println!("  Embeddings: {count}"),
                    Err(_) => println!("  Could not query embedding count"),
                }
            }
            Err(e) => println!("  Could not open database: {e}"),
        }
    } else {
        println!("  No database found at {db_path}");
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
async fn cmd_sync(
    file: Option<&str>,
    scan: Option<&str>,
    workspace: &str,
    json_dir: &str,
    db_path: &str,
    force: bool,
    no_db: bool,
    dry_run: bool,
    verbose: bool,
    watch: bool,
    watch_debounce_ms: u64,
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

        if source_path.is_dir() {
            let engine = SyncEngine::new(guidance_dir.clone(), source_dir.clone());
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
            if !force && !guidance_core::sync::staleness::should_generate(&json_path, source_path) {
                if verbose {
                    println!("  skip (up to date): {path}");
                }
                return;
            }
            let (tx, rx) = oneshot::channel();
            runtime::AST_POOL
                .submit(runtime::AstGenJob {
                    source_path: source_path.to_path_buf(),
                    source_dir,
                    guidance_dir: guidance_dir.clone(),
                    config: guidance_core::sync_engine::GenConfig::default(),
                    result_tx: tx,
                })
                .await
                .expect("queue closed during gen");
            match rx.await {
                Ok(Ok(doc)) => {
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
                Ok(Err(e)) => eprintln!("error generating {path}: {e}"),
                Err(_) => eprintln!("error: pool response canceled for {path}"),
            }
        }
    } else if let Some(scan_dir) = scan {
        let scan_path = PathBuf::from(scan_dir);
        std::fs::create_dir_all(&guidance_dir).expect("create guidance dir");
        let generated =
            walk_and_gen_async(guidance_dir.clone(), scan_path, force, verbose, &[]).await;
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
                generated +=
                    walk_and_gen_async(guidance_dir.clone(), src_dir.clone(), force, verbose, &[])
                        .await;
            }
        }

        println!("Syncing {total_files} total files ({stale_files} stale)...");
        if generated > 0 {
            println!("Generated {generated} files.");
        }

        if !no_db && !dry_run {
            let json_src = guidance_dir.join("src");
            if json_src.is_dir() {
                let (tx, rx) = oneshot::channel();
                runtime::DB_POOL
                    .submit(runtime::DbSyncJob {
                        json_dir: json_src,
                        db_path: db,
                        result_tx: tx,
                    })
                    .await
                    .expect("db sync queue closed");
                match rx.await {
                    Ok(Ok(count)) => println!("Synced {count} nodes to {db_path}"),
                    Ok(Err(e)) => eprintln!("Warning: db sync failed: {e}"),
                    Err(_) => eprintln!("Warning: db sync canceled"),
                }
            }
        }
        println!("Sync complete.");
    }

    if watch {
        let src_dirs = src_dirs_from_config(&workspace_path);
        start_watcher(
            guidance_dir,
            &src_dirs,
            workspace_path,
            force,
            verbose,
            watch_debounce_ms,
        )
        .await;
    }
}

fn src_dirs_from_config(workspace_path: &Path) -> Vec<PathBuf> {
    let cfg = load_project_config(workspace_path);
    if cfg.src_dirs.is_empty() {
        vec![workspace_path.to_path_buf()]
    } else {
        cfg.src_dirs
            .iter()
            .map(|d| workspace_path.join(d))
            .collect()
    }
}

fn collect_source_files(dir: &Path, exts: &[&str]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if !dir.is_dir() {
        return files;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return files;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.starts_with('.') && name != "target" {
                files.extend(collect_source_files(&path, exts));
            }
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if exts.is_empty() || exts.contains(&ext) {
                files.push(path);
            }
        }
    }
    files
}

async fn walk_and_gen_async(
    guidance_dir: PathBuf,
    source_dir: PathBuf,
    force: bool,
    verbose: bool,
    filter_exts: &[&str],
) -> usize {
    let exts = if filter_exts.is_empty() {
        vec!["zig", "zon", "py", "rs", "md"]
    } else {
        filter_exts.to_vec()
    };
    let files = collect_source_files(&source_dir, &exts);
    if files.is_empty() {
        return 0;
    }

    let pool = &*runtime::AST_POOL;
    let mut handles = Vec::with_capacity(files.len());
    let mut generated = 0usize;

    for path in &files {
        let rel = path.strip_prefix(&source_dir).unwrap_or(path);
        let json_path = guidance_dir
            .join("src")
            .join(format!("{}.json", rel.display()));
        let should_gen = force || guidance_core::sync::staleness::should_generate(&json_path, path);
        if !should_gen {
            if verbose {
                println!("  skip: {}", rel.display());
            }
            continue;
        }

        let (tx, rx) = oneshot::channel();
        pool.submit(runtime::AstGenJob {
            source_path: path.clone(),
            source_dir: source_dir.clone(),
            guidance_dir: guidance_dir.clone(),
            config: guidance_core::sync_engine::GenConfig::default(),
            result_tx: tx,
        })
        .await
        .expect("queue closed during gen");
        handles.push((rel.display().to_string(), rx));
    }

    let mut gen_failed = 0usize;
    for (rel_path, rx) in handles {
        match rx.await {
            Ok(Ok(doc)) => {
                generated += 1;
                if verbose {
                    println!("  gen: {rel_path} ({} members)", doc.members.len());
                }
            }
            Ok(Err(e)) => {
                gen_failed += 1;
                if verbose {
                    eprintln!("  warn: {rel_path} — {e}");
                }
            }
            Err(_) => {
                gen_failed += 1;
                if verbose {
                    eprintln!("  warn: {rel_path} — pool response canceled");
                }
            }
        }
    }

    if gen_failed > 0 {
        eprintln!("Warning: {gen_failed} files failed to generate");
    }

    generated
}

#[allow(clippy::needless_pass_by_value)]
async fn start_watcher(
    guidance_dir: PathBuf,
    src_dirs: &[PathBuf],
    workspace_path: PathBuf,
    _force: bool,
    verbose: bool,
    debounce_ms: u64,
) {
    if src_dirs.is_empty() {
        return;
    }

    let (tx, rx) = mpsc::channel();
    let mut watcher = RecommendedWatcher::new(tx, NotifyConfig::default())
        .expect("failed to create file watcher");
    for dir in src_dirs {
        if dir.is_dir() {
            watcher
                .watch(dir, RecursiveMode::Recursive)
                .unwrap_or_else(|e| eprintln!("Warning: could not watch {:?}: {e}", dir));
        }
    }

    println!("Watching for changes (debounce: {debounce_ms}ms)...");
    let supported_exts = ["zig", "zon", "py", "rs", "md"];

    loop {
        match rx.recv() {
            Ok(Ok(Event { paths, .. })) => {
                for path in &paths {
                    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                    if !supported_exts.contains(&ext) {
                        continue;
                    }
                    let source_dir = find_source_dir(path, src_dirs);

                    if verbose {
                        println!("  change detected: {}", path.display());
                    }

                    let (tx_job, rx_job) = oneshot::channel();
                    runtime::AST_POOL
                        .submit(runtime::AstGenJob {
                            source_path: path.clone(),
                            source_dir: source_dir.unwrap_or_else(|| workspace_path.clone()),
                            guidance_dir: guidance_dir.clone(),
                            config: guidance_core::sync_engine::GenConfig::default(),
                            result_tx: tx_job,
                        })
                        .await
                        .expect("queue closed during gen");
                    if let Ok(Ok(doc)) = rx_job.await {
                        if verbose {
                            println!(
                                "  regenerated: {} ({} members)",
                                path.display(),
                                doc.members.len()
                            );
                        }
                    }
                }
            }
            Ok(Err(e)) => {
                eprintln!("Watch error: {e}");
            }
            Err(_) => break,
        }
    }
}

fn find_source_dir(path: &Path, src_dirs: &[PathBuf]) -> Option<PathBuf> {
    for dir in src_dirs {
        if path.starts_with(dir) {
            return Some(dir.clone());
        }
    }
    None
}

fn cmd_status(guidance_dir: &str) {
    let gdir = PathBuf::from(guidance_dir);
    let workspace_path = std::env::current_dir().unwrap_or_default();
    let cfg = load_project_config(&workspace_path);
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
    let mut up_to_date = 0usize;
    for src_dir in &src_dirs {
        if !src_dir.is_dir() {
            continue;
        }
        let engine = SyncEngine::new(gdir.clone(), src_dir.clone());
        match engine.status() {
            Ok(status) => {
                total_files += status.total_files;
                stale_files += status.stale_files;
                up_to_date += status.up_to_date;
            }
            Err(e) => eprintln!("error: {e}"),
        }
    }
    println!("Sync Status:");
    println!("  Total files: {total_files}");
    println!("  Stale files: {stale_files}");
    println!("  Up to date:  {up_to_date}");
    println!("  Clean:       {}", stale_files == 0);
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

fn cmd_check(workspace: &str) {
    let workspace_path = PathBuf::from(workspace);
    let cfg = load_project_config(&workspace_path);
    let guidance_dir = workspace_path.join(".guidance");
    let mut all_passed = true;

    let src_dirs: Vec<PathBuf> = if cfg.src_dirs.is_empty() {
        vec![workspace_path.clone()]
    } else {
        cfg.src_dirs
            .iter()
            .map(|d| workspace_path.join(d))
            .collect()
    };
    let present_exts = collect_extensions(&src_dirs);

    type StageFn = Box<dyn Fn() -> Result<(), String>>;
    let mut stages: Vec<(&str, StageFn)> = Vec::new();

    for (ext, argv) in &cfg.test_commands {
        if !present_exts.contains(ext.as_str()) {
            continue;
        }
        let argv: Vec<String> = argv.clone();
        let ext = ext.clone();
        stages.push((
            "test",
            Box::new(move || {
                let argv_refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
                if run_command(&argv_refs) {
                    Ok(())
                } else {
                    Err(format!("{ext}: test command failed"))
                }
            }),
        ));
    }

    for (ext, argv) in &cfg.lint_commands {
        if !present_exts.contains(ext.as_str()) {
            continue;
        }
        let argv: Vec<String> = argv.clone();
        let ext = ext.clone();
        stages.push((
            "lint",
            Box::new(move || {
                let argv_refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
                if run_command(&argv_refs) {
                    Ok(())
                } else {
                    Err(format!("{ext}: lint command failed"))
                }
            }),
        ));
    }

    for (ext, argv) in &cfg.fmt_commands {
        if !present_exts.contains(ext.as_str()) {
            continue;
        }
        let argv: Vec<String> = argv.clone();
        let ext = ext.clone();
        stages.push((
            "fmt",
            Box::new(move || {
                let argv_refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
                if run_command(&argv_refs) {
                    Ok(())
                } else {
                    Err(format!("{ext}: fmt command failed"))
                }
            }),
        ));
    }

    let cfg_gen = cfg.clone();
    let ws_gen = workspace_path.clone();
    let gd_gen = guidance_dir.clone();
    stages.push((
        "gen",
        Box::new(move || {
            let src_dirs: Vec<PathBuf> = if cfg_gen.src_dirs.is_empty() {
                vec![ws_gen.clone()]
            } else {
                cfg_gen.src_dirs.iter().map(|d| ws_gen.join(d)).collect()
            };
            let mut total_stale = 0usize;
            for src_dir in &src_dirs {
                if !src_dir.is_dir() {
                    continue;
                }
                let engine = SyncEngine::new(gd_gen.clone(), src_dir.clone());
                let status = engine.status().map_err(|e| format!("status: {e}"))?;
                total_stale += status.stale_files;
            }
            if total_stale > 0 {
                Err(format!("{total_stale} stale files need regeneration"))
            } else {
                Ok(())
            }
        }),
    ));

    let structure_path = workspace_path.join("STRUCTURE.md");
    stages.push((
        "structure",
        Box::new(move || {
            if structure_path.exists() {
                Ok(())
            } else {
                Err("STRUCTURE.md not found".into())
            }
        }),
    ));

    let db_path = workspace_path.join(".guidance.db");
    stages.push((
        "db",
        Box::new(move || {
            if db_path.exists() {
                Ok(())
            } else {
                Err(".guidance.db not found".into())
            }
        }),
    ));

    for (name, stage_fn) in &stages {
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
        println!("\nAll checks passed");
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
                        if let Ok(Some(doc)) = guidance_core::sync::json_store::load_guidance(&path)
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
    for (_path, doc) in walk_guidance_docs(dir) {
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

fn cmd_structure(json_dir: &str) {
    let gdir = PathBuf::from(json_dir);
    match structure::generate(&gdir) {
        Ok(output) => {
            std::fs::write("STRUCTURE.md", &output).expect("write STRUCTURE.md");
            let line_count = output.lines().count();
            println!("STRUCTURE.md generated ({} lines)", line_count);
        }
        Err(e) => {
            eprintln!("error generating structure: {e}");
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
    for (_path, doc) in walk_guidance_docs(dir) {
        files.push(doc.meta.source.as_str().to_string());
        for member in &doc.members {
            *total += 1;
            if member.comment.is_none() {
                *no_comments += 1;
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
