//! Prototype HTTP server: history in, REST/WebSocket out, no Python.
#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use protopyne_server::{
    load_history_path, open_tape_reader, run_live_loop, serve_with_tape, BacktestConfig,
    ChainPolicy, LiveLoop, LiveLoopOutcome, LoopGate, RunnerConfig, ServerConfig,
};
use protopyne_strategy::{RiskEngine, RiskLimits};
use protopyne_types::{GapPolicy, Money, SeriesContext, Timeframe};
use protopyne_venue::{LiveAttachments, LiveBackfillSpec, QuoteSource, StreamConfig};

/// Startup arguments. Parsing lives here (never in library code): malformed
/// flags exit loudly instead of binding half-configured.
struct Args {
    config: Option<PathBuf>,
    bind: Option<String>,
    history: Option<PathBuf>,
    tape: Option<PathBuf>,
    live: bool,
    live_journal: Option<PathBuf>,
    live_alerts: Option<PathBuf>,
}

fn parse_args(raw: &[String]) -> Result<Args, String> {
    let mut args = Args {
        config: None,
        bind: None,
        history: None,
        tape: None,
        live: false,
        live_journal: None,
        live_alerts: None,
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
            "--live-alerts" => {
                args.live_alerts = Some(PathBuf::from(
                    rest.next().ok_or("--live-alerts needs a path")?,
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
                   venue and run the trading loop only when the journaled
                   authority permits it (an armed grant plus a loop-start
                   command). Boot alone places nothing. Reads
                   TASTYTRADE_BASE_URL, TASTYTRADE_ACCOUNT, TASTYTRADE_LOGIN,
                   TASTYTRADE_PASSWORD, optional TASTYTRADE_UNDERLYING (SPY),
                   --live-journal PATH, and --history for the bar feed.
  --live-journal PATH  durable journal file for --live (required with it)
  --live-alerts PATH   out-of-band alert file for --live (optional; breaker
                   trips and gate halts append one JSON object per line)
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
    let handle = serve_with_tape(config, history, reader, None)
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
    // The bar feed shapes the backfill sensor's series, so history loads
    // before the boot attaches it: a gap is judged against the feed it
    // guards, never a placeholder timeframe.
    let history_path = args
        .history
        .clone()
        .ok_or("--live needs --history for the bar feed")?;
    let history_bars = load_history_path(&history_path, None)
        .map_err(|e| format!("live history failed to load: {e}"))?;
    let timeframe = Timeframe::infer(&history_bars).unwrap_or(Timeframe::D1);
    let underlying = std::env::var("TASTYTRADE_UNDERLYING").unwrap_or_else(|_| "SPY".to_owned());
    let boot = protopyne_venue::LiveBoot::new()
        .session(protopyne_venue::SessionConfig::default())
        .transport(
            protopyne_venue::TastytradeConfig::new()
                .base_url(base_url)
                .build(),
        )
        .account(account.clone())
        .journal_path(journal_path.clone())
        .maybe_alerts_path(args.live_alerts.clone())
        .build();
    // The supervised producer set: a stream breaker, journal-vs-venue
    // reconciliation, and reconnect backfill over the feed's series. A
    // requested producer that cannot attach refuses the boot itself.
    let attachments = LiveAttachments::none()
        .with_stream(StreamConfig::new().build())
        .with_reconcile()
        .with_backfill(LiveBackfillSpec::new(
            SeriesContext::new(underlying.clone(), timeframe, GapPolicy::Sparse),
            Vec::new(),
            0,
        ));
    let net = std::sync::Arc::new(fluent_concurrency::io::net::NetCapability::new());
    let caps =
        fluent_wvr::CapabilitySet::new().with(fluent_concurrency::io::net::NetCapability::new());
    let live = fluent_wvr::CURRENT_CAPS
        .scope(
            caps,
            protopyne_venue::boot_live(&boot, net, &username, &password, attachments, None),
        )
        .await
        .map_err(|e| format!("live boot refused: {e}"))?;
    println!(
        "live session ready: account {account}, journal {}, producers {:?}",
        journal_path.display(),
        live.producers(),
    );
    // The broker directory names what the boot already owns: the same venue
    // object the session registry routes, plus its account, feed, and
    // credential handles. Registration is additive — execution routing is
    // untouched.
    let directory = protopyne_venue::BrokerRegistry::new();
    let bundle = live
        .register_broker_bundle(&directory)
        .map_err(|e| format!("live broker directory refused: {e}"))?;
    println!(
        "live broker directory: {:?} (streams={})",
        directory.ids(),
        bundle.capabilities.streams,
    );
    let mut live = live;
    // Journaled authority first: an arm alone is not a start. Nothing moves
    // unless the operator also journaled the loop-start command.
    let gate =
        protopyne_server::loop_gate(&live.journal, &live.storage).map_err(|e| e.to_string())?;
    if let LoopGate::Ready(_) = gate {
        // The arm and the start are both journaled: run the one bar body.
    } else {
        let reason = match gate {
            LoopGate::Unarmed => "no armed grant is journaled",
            LoopGate::NotStarted => "a grant is armed but no loop-start command is journaled",
            LoopGate::Ready(_) => unreachable!(),
        };
        println!("live loop not started: {reason}; boot placed nothing");
        live.session.shutdown().await;
        return Ok(());
    }
    // Authority without the producer set is the same refusal: the loop runs
    // only when every supervised producer is attached.
    let missing = live.missing_producers();
    if !missing.is_empty() {
        println!(
            "live loop not started: missing supervised producers: {}; boot placed nothing",
            missing.join(", ")
        );
        live.session.shutdown().await;
        return Ok(());
    }
    let bars = history_bars;
    let strategy =
        protopyne_server::rehearsal::rehearsal_strategy(None).map_err(|e| e.to_string())?;
    let risk = RiskEngine::new(RiskLimits::new().build());
    let quote_config = BacktestConfig::new()
        .underlying(underlying.clone())
        .data_version("live".to_owned())
        .build();
    let runner = RunnerConfig::new(
        underlying.clone(),
        "ribbon".to_owned(),
        Money::from(400),
        Money::from(2_000),
        30,
        0,
    );
    // The authenticated transport is the quote ladder; a dark ladder halts
    // entries fail-closed rather than inventing a price.
    let sources: Vec<Arc<dyn QuoteSource>> =
        vec![Arc::clone(live.venue.transport()) as Arc<dyn QuoteSource>];
    let outcome = run_live_loop(
        &mut live,
        LiveLoop {
            bars,
            sources,
            strategy: strategy.as_ref(),
            risk: &risk,
            quote_config: &quote_config,
            runner_config: &runner,
            timeframe,
            chains: ChainPolicy::Unscreened,
            cadence: Duration::ZERO,
            seed: 0,
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    match outcome {
        LiveLoopOutcome::Ran(decisions) => println!(
            "live loop complete: {} bars decided, {} submitted",
            decisions.len(),
            decisions
                .iter()
                .filter(|decision| decision.submitted)
                .count()
        ),
        LiveLoopOutcome::Refused(gate) => println!("live loop not started: {gate:?}"),
        LiveLoopOutcome::MissingProducers(missing) => println!(
            "live loop not started: missing supervised producers: {}; boot placed nothing",
            missing.join(", ")
        ),
    }
    live.session.shutdown().await;
    Ok(())
}
