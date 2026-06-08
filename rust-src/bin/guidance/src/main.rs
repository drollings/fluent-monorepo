use clap::{Parser, Subcommand};

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

#[derive(Subcommand)]
enum Commands {
    /// Explain a keyword or query
    Explain {
        query: String,

        /// Path to guidance directory
        #[arg(short, long, default_value = ".guidance")]
        guidance_dir: String,
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
        Commands::Explain { query, guidance_dir } => {
            println!("Explain: {query} (guidance_dir: {guidance_dir})");
        }
        Commands::Show { file } => {
            println!("Show: {file}");
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
            println!("MCP server mode (STDIO)...");
        }
        Commands::Init { dir } => {
            println!("Initializing guidance in {dir}");
        }
        Commands::Gen { file } => match file {
            Some(path) => println!("Generating guidance for {path}"),
            None => println!("Generating guidance for all source files"),
        },
        Commands::Status => {
            println!("Sync status:");
        }
        Commands::Clean => {
            println!("Cleaning generated files...");
        }
        Commands::Commit { message } => {
            println!("Committing with message: {message}");
        }
        Commands::Check => {
            println!("Checking for stale files...");
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
