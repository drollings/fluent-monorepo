#![forbid(unsafe_code)]

//! `oratord`: the Orator daemon. Builds the owned scope tree, serves until
//! an exit signal, then drains every scope within a bound.

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use orator_daemon::{run_loopback, RootScope};

/// Daemon entry points. The serving path grows here; the loopback flag
/// exercises capture→archive without hardware.
#[derive(Debug, Parser)]
#[command(name = "oratord")]
struct Args {
    /// Run a synthetic capture→archive loopback into DIR and exit.
    #[arg(long)]
    loopback: Option<PathBuf>,
    /// Loopback duration in seconds.
    #[arg(long, default_value_t = 5)]
    seconds: u64,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let args = Args::parse();
    if args.seconds != 5 && args.loopback.is_none() {
        eprintln!("oratord: --seconds without --loopback does nothing (pass --loopback DIR)");
        std::process::exit(2);
    }
    if let Some(dir) = args.loopback {
        match run_loopback(&dir, args.seconds).await {
            Ok(manifest) => {
                eprintln!(
                    "oratord: loopback wrote {} segment(s)",
                    manifest.segments.len()
                );
            }
            Err(error) => {
                eprintln!("oratord: loopback failed: {error}");
                std::process::exit(1);
            }
        }
        return;
    }
    let mut root = RootScope::build();
    let Some(path) = orator_core::socket_path() else {
        eprintln!(
            "oratord: no socket path: neither XDG_RUNTIME_DIR nor HOME is set; refusing to start"
        );
        std::process::exit(1);
    };
    let (daemon_config, _) = orator_core::config::load_hierarchical();
    let voice = std::sync::Arc::new(orator_daemon::voice::VoiceOwner::new(&daemon_config));
    let serve_path = path.clone();
    let server = {
        let voice = std::sync::Arc::clone(&voice);
        tokio::spawn(async move { orator_daemon::socket::serve(&serve_path, voice).await })
    };
    // Additive WebSocket listener: same contract, second carrier.
    // Socket-only unless configured; a configured listener without a
    // token refuses to start (fail closed, never unauthenticated).
    let ws_server = match daemon_config.daemon.ws_listen.is_empty() {
        true => None,
        false if daemon_config.daemon.ws_token.is_empty() => {
            eprintln!("oratord: daemon.ws_listen is set without daemon.ws_token; refusing to serve unauthenticated (unset ws_listen or set a token)");
            std::process::exit(1);
        }
        false => {
            let addr = daemon_config.daemon.ws_listen.clone();
            let token = daemon_config.daemon.ws_token.clone();
            let tuning = orator_daemon::ws::WsTuning::from_config(&daemon_config.daemon);
            let voice = std::sync::Arc::clone(&voice);
            Some(tokio::spawn(async move {
                orator_daemon::ws::serve_ws(&addr, token, voice, tuning).await
            }))
        }
    };
    orator_core::shutdown_signal().await;
    eprintln!("oratord: shutting down");
    orator_daemon::request_shutdown();
    voice.shutdown();
    // Wake the accept loop if it parked after the stop landed, then
    // await the graceful drain (handlers are timeout-bounded).
    let _ = std::os::unix::net::UnixStream::connect(&path);
    match tokio::time::timeout(Duration::from_secs(15), server).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(error))) => eprintln!("oratord: serve failed: {error}"),
        Ok(Err(error)) => eprintln!("oratord: serve task failed: {error}"),
        Err(_) => eprintln!("oratord: serve did not drain in time; exiting anyway"),
    }
    if let Some(ws_server) = ws_server {
        // The WS accept loop polls the same flag; its handlers drain
        // like the socket ones (same JoinSet path).
        match tokio::time::timeout(Duration::from_secs(15), ws_server).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(error))) => eprintln!("oratord: WS serve failed: {error}"),
            Ok(Err(error)) => eprintln!("oratord: WS serve task failed: {error}"),
            Err(_) => eprintln!("oratord: WS serve did not drain in time; exiting anyway"),
        }
    }
    eprintln!("oratord: draining scopes");
    root.shutdown(Duration::from_secs(5)).await;
    // No shutdown-time unlink: the stale reclaim in `bind` already
    // covers restarts, and an unconditional remove races a parallel
    // starter's reclaim. The socket file left behind is reclaimed on
    // the next start after a connect proves no live holder.
    eprintln!("oratord: shutdown complete");
}
