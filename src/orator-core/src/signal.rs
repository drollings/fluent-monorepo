//! Process shutdown signal: SIGTERM or SIGINT, whichever arrives first.
//!
//! One helper shared by the daemon and the CLI so both observe the same
//! exit conditions. Resolves once; callers release their endpoint or
//! child and exit promptly afterwards.

/// Wait until the process should shut down: SIGTERM on Unix, Ctrl-C
/// anywhere. Never resolves spuriously; a second caller waits on its
/// own receiver for the same process-wide delivery.
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let terminate = async {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut stream) => {
                    stream.recv().await;
                }
                Err(_) => {
                    std::future::pending::<()>().await;
                }
            }
        };
        let interrupt = async {
            let _ = tokio::signal::ctrl_c().await;
        };
        tokio::select! {
            () = terminate => {},
            () = interrupt => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
#[path = "../tests/signal.rs"]
mod tests;
