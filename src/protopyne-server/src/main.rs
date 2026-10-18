//! Prototype HTTP server: history in, REST/WebSocket out, no Python.
#![forbid(unsafe_code)]

use std::path::PathBuf;

use protopyne_server::{load_history_path, open_tape_reader, serve_with_tape, ServerConfig};

/// Startup arguments. Parsing lives here (never in library code): malformed
/// flags exit loudly instead of binding half-configured.
struct Args {
    config: Option<PathBuf>,
    bind: Option<String>,
    history: Option<PathBuf>,
    tape: Option<PathBuf>,
    live: bool,
    live_journal: Option<PathBuf>,
}

fn parse_args(raw: &[String]) -> Result<Args, String> {
    let mut args = Args {
        config: None,
        bind: None,
        history: None,
        tape: None,
        live: false,
        live_journal: None,
    };
    let mut rest = raw.iter().skip(1);
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--config" => {
                args.config = Some(PathBuf::from(rest.next().ok_or("--config needs a path")?));
            }
            "--bind" => {
                args.bind = Some(rest.next().ok_or("--bind needs an address")?.clone());
            }
            "--history" => {
                args.history = Some(PathBuf::from(rest.next().ok_or("--history needs a path")?));
            }
            "--tape" => {
                args.tape = Some(PathBuf::from(
                    rest.next().ok_or("--tape needs a store root")?,
                ));
            }
            "--live" => {
                args.live = true;
            }
            "--live-journal" => {
                args.live_journal = Some(PathBuf::from(
                    rest.next().ok_or("--live-journal needs a path")?,
                ));
            }
            unknown => return Err(format!("unknown flag {unknown}")),
        }
    }
    if args.live && args.live_journal.is_none() {
        return Err("--live needs --live-journal PATH for the durable journal".to_owned());
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
  --live               boot the production trading session against the live
                   venue and exit (no serving): reads TASTYTRADE_BASE_URL,
                   TASTYTRADE_ACCOUNT, TASTYTRADE_LOGIN, TASTYTRADE_PASSWORD
                   from the environment plus --live-journal PATH; refuses
                   without all five. Live decision serving stays gated on the
                   approval inbox and the cert-sandbox soak.
  --live-journal PATH  durable journal file for --live (required with it)
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
    if args.live {
        return run_live(&args).await;
    }
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
        // Record the root in the config so symbol discovery (the gateway's
        // `symbols` read) sees the tree the history was loaded from.
        config.history_path = Some(history_path.to_string_lossy().into_owned());
    }
    let reader = config
        .tape_path
        .as_deref()
        .map(|root| open_tape_reader(std::path::Path::new(root)))
        .transpose()
        .map_err(|e| e.to_string())?;
    let handle = serve_with_tape(config, history, reader)
        .await
        .map_err(|e| e.to_string())?;
    println!("protopyne-server listening on {}", handle.addr);
    let _ = tokio::signal::ctrl_c().await;
    handle.shutdown().await;
    Ok(())
}

/// Boot the production trading session against the live venue: environment
/// credentials in, authenticated session plus durable journal out, then a
/// clean shutdown. This exercises the production boot path in the real
/// binary (config → login → session → shutdown); it places no orders and
/// serves nothing — live decision flow stays gated on the approval inbox
/// and the cert-sandbox soak. Every missing input refuses before anything
/// dials.
async fn run_live(args: &Args) -> Result<(), String> {
    // Empty secrets are missing secrets: each variable must be present and
    // non-empty, else nothing dials.
    fn env(name: &str) -> Result<String, String> {
        let value =
            std::env::var(name).map_err(|_| format!("--live needs {name} in the environment"))?;
        if value.is_empty() {
            return Err(format!("--live needs {name} in the environment"));
        }
        Ok(value)
    }
    let base_url = env("TASTYTRADE_BASE_URL")?;
    let account = env("TASTYTRADE_ACCOUNT")?;
    let username = env("TASTYTRADE_LOGIN")?;
    let password = env("TASTYTRADE_PASSWORD")?;
    let journal_path = args
        .live_journal
        .clone()
        .ok_or("--live needs --live-journal PATH for the durable journal")?;
    let boot = protopyne_venue::LiveBoot::new()
        .session(protopyne_venue::SessionConfig::default())
        .transport(
            protopyne_venue::TastytradeConfig::new()
                .base_url(base_url)
                .build(),
        )
        .account(account.clone())
        .journal_path(journal_path.clone())
        .build();
    let net = std::sync::Arc::new(fluent_concurrency::io::net::NetCapability::new());
    let caps =
        fluent_wvr::CapabilitySet::new().with(fluent_concurrency::io::net::NetCapability::new());
    let live = fluent_wvr::CURRENT_CAPS
        .scope(
            caps,
            protopyne_venue::boot_live(&boot, net, &username, &password),
        )
        .await
        .map_err(|e| format!("live boot refused: {e}"))?;
    println!(
        "live session ready: account {account}, journal {}",
        journal_path.display()
    );
    live.session.shutdown().await;
    Ok(())
}
