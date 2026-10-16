//! Prototype HTTP server: history in, REST/WebSocket out, no Python.
#![forbid(unsafe_code)]

use std::path::PathBuf;

use protopyne_server::{ServerConfig, load_history_path, open_tape_reader, serve_with_tape};

/// Startup arguments. Parsing lives here (never in library code): malformed
/// flags exit loudly instead of binding half-configured.
struct Args {
    config: Option<PathBuf>,
    bind: Option<String>,
    history: Option<PathBuf>,
    tape: Option<PathBuf>,
}

fn parse_args(raw: &[String]) -> Result<Args, String> {
    let mut args = Args { config: None, bind: None, history: None, tape: None };
    let mut rest = raw.iter().skip(1);
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--config" => {
                args.config = Some(PathBuf::from(
                    rest.next().ok_or("--config needs a path")?,
                ));
            }
            "--bind" => {
                args.bind = Some(rest.next().ok_or("--bind needs an address")?.clone());
            }
            "--history" => {
                args.history = Some(PathBuf::from(
                    rest.next().ok_or("--history needs a path")?,
                ));
            }
            "--tape" => {
                args.tape = Some(PathBuf::from(
                    rest.next().ok_or("--tape needs a store root")?,
                ));
            }
            unknown => return Err(format!("unknown flag {unknown}")),
        }
    }
    Ok(args)
}

#[tokio::main]
async fn main() {
    let raw: Vec<String> = std::env::args().collect();
    if raw.iter().any(|flag| flag == "--help" || flag == "-h") {
        println!("{USAGE}");
        return;
    }
    if raw.iter().any(|flag| flag == "--version" || flag == "-V") {
        println!("protopyne-server {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if let Err(reason) = run(&raw).await {
        eprintln!("protopyne-server: {reason}");
        std::process::exit(2);
    }
}

/// One-screen usage printed by `--help`.
const USAGE: &str = "\
protopyne-server: paper-trading HTTP surface over versioned bar history.

Usage:
  protopyne-server [--config PATH] [--bind ADDR] --history PATH [--tape ROOT]

Flags:
  --config PATH    JSON server config (defaults for every field when absent)
  --bind ADDR      override the bind address (default 127.0.0.1:9060)
  --history PATH   bar-history CSV or Parquet file, or a history-root
                   directory (TICKER/YYYY/MM/*.parquet with *.csv
                   supplements; serves SPY, and history_timeframe reads
                   1M). Also accepted as history_path in --config.
  --tape ROOT      durable tape store root (also accepted as tape_path in --config);
                   symbol-scoped chart reads plan through the tape, falling back
                   to the boot history per read
  --help, -h       print this message
  --version, -V    print the version

Endpoints once serving:
  GET  /health               liveness + version
  POST /proposals/preview    run the decision stack over history ({\"seed\": 7})
  GET  /proposals            proposal inbox from the latest preview
  POST /grants/arm           arm a grant (tighten-only; loosening is 409)
  GET  /grants/current       the armed grant, if any
  GET  /portfolio            positions, fills, and fees from the latest preview
  GET  /chart/bars           Lightweight-Charts candle rows
  GET  /chart/overlays       indicator overlays (?kind=sma|ema|bb|trima&period=14)
  GET  /feed                 WebSocket rehearsal bar feed

Example:
  protopyne-server --history src/protopyne-server/tests/data/SPY_15M.csv
  curl -s http://127.0.0.1:9060/health";

async fn run(raw: &[String]) -> Result<(), String> {
    let args = parse_args(raw)?;
    let mut config = match args.config {
        Some(path) => common_core::config::load_json_or_default(&path),
        None => ServerConfig::default(),
    };
    if let Some(bind) = args.bind {
        config.bind = bind;
    }
    if let Some(tape) = args.tape {
        config.tape_path = Some(tape.to_string_lossy().into_owned());
    }
    let history_path = args
        .history
        .or_else(|| config.history_path.clone().map(PathBuf::from))
        .ok_or("no history: pass --history or set history_path in the config")?;
    let history = load_history_path(&history_path, None)
        .map_err(|e| format!("history failed to load: {e}"))?;
    if history_path.is_dir() {
        // The tree contract is one-minute months; the boot base follows it
        // so symbol- and timeframe-scoped reads fold by whole multiples.
        config.history_timeframe = String::from("1M");
    }
    let reader = config
        .tape_path
        .as_deref()
        .map(|root| open_tape_reader(std::path::Path::new(root)))
        .transpose()
        .map_err(|e| e.to_string())?;
    let handle = serve_with_tape(config, history, reader).await.map_err(|e| e.to_string())?;
    println!("protopyne-server listening on {}", handle.addr);
    let _ = tokio::signal::ctrl_c().await;
    handle.shutdown().await;
    Ok(())
}
