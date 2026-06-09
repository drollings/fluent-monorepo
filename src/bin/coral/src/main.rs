use clap::{Parser, Subcommand};
use guidance_coral::db::Library;
use guidance_coral::mcp::serve_stdio_from_path;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "coral", about = "Context-graph database & MCP server")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    #[command(name = "mcp")]
    Mcp {
        #[arg(short = 'o', long, default_value = ".guidance.db")]
        db: String,
    },
    Ingest {
        #[arg(short, long)]
        file: String,
        #[arg(short, long, default_value = "10000")]
        batch_size: usize,
        #[arg(long)]
        skip_errors: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    match &cli.command {
        Commands::Mcp { db } => cmd_mcp(db),
        Commands::Ingest {
            file,
            batch_size,
            skip_errors,
        } => cmd_ingest(file, *batch_size, *skip_errors),
    }
}

fn cmd_mcp(db_path: &str) {
    let db = PathBuf::from(db_path);
    eprintln!("Coral MCP server started (STDIO)");
    if let Err(e) = serve_stdio_from_path(&db) {
        eprintln!("MCP server error: {e}");
        std::process::exit(1);
    }
}

fn cmd_ingest(file: &str, batch_size: usize, _skip_errors: bool) {
    let path = std::path::Path::new(file);
    if !path.exists() {
        eprintln!("error: file not found: {file}");
        std::process::exit(1);
    }
    match Library::open_in_memory() {
        Ok(lib) => {
            let config = guidance_coral::ingest::IngestionConfig {
                batch_size,
                yago_whitelist_only: false,
                preferred_lang: "en".to_string(),
            };
            let mut ingestor =
                guidance_coral::ingest::BatchIngestor::with_config(Arc::new(lib), config);
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
