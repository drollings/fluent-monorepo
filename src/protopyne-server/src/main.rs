//! Prototype HTTP server: history in, REST/WebSocket out, no Python.
#![forbid(unsafe_code)]

use std::path::PathBuf;

use protopyne_server::{ServerConfig, load_bars_with_fallback, serve};

/// Startup arguments. Parsing lives here (never in library code): malformed
/// flags exit loudly instead of binding half-configured.
struct Args {
    config: Option<PathBuf>,
    bind: Option<String>,
    history: Option<PathBuf>,
}

fn parse_args(raw: &[String]) -> Result<Args, String> {
    let mut args = Args { config: None, bind: None, history: None };
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
  protopyne-server [--config PATH] [--bind ADDR] --history PATH

Flags:
  --config PATH    JSON server config (defaults for every field when absent)
  --bind ADDR      override the bind address (default 127.0.0.1:9060)
  --history PATH   bar-history CSV (also accepted as history_path in --config)
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
  GET  /chart/overlays       indicator overlays (?kind=sma&period=14)
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
    let history_path = args
        .history
        .or_else(|| config.history_path.clone().map(PathBuf::from))
        .ok_or("no history: pass --history or set history_path in the config")?;
    let history = load_bars_with_fallback(std::slice::from_ref(&history_path))
        .map_err(|e| format!("history failed to load: {e}"))?;
    let handle = serve(config, history).await.map_err(|e| e.to_string())?;
    println!("protopyne-server listening on {}", handle.addr);
    let _ = tokio::signal::ctrl_c().await;
    handle.shutdown().await;
    Ok(())
}
