//! WebSocket listener: the same contract over a second carrier.
//!
//! One dispatch core ([`crate::socket::serve_call`]), one method table,
//! one envelope shape — only the carrier changes (Unix stream vs WS
//! message). Connections are long-lived with many calls each: the
//! server pings, and a connection silent past the inactivity bound is
//! reaped. Loopback only; every handshake carries the bearer token and
//! tokenless or wrong-token peers get a 401, never a downgrade.

use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use orator::SocketFrame;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        handshake::server::{ErrorResponse, Request, Response},
        Message,
    },
};

use crate::voice::VoiceOwner;

/// Bound of silence before a connection is reaped (production).
/// Prefer [`WsTuning::from_config`]; this const is the documented
/// Moshi-adapted default the config leaf agrees with.
pub const WS_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(10);
/// Keepalive ping cadence (production default; see [`WsTuning`]).
pub const WS_PING_EVERY: Duration = Duration::from_secs(5);
/// Connection lifetime cap (production default; see [`WsTuning`]).
/// Zero disables.
pub const WS_CONNECTION_CAP: Duration = Duration::from_secs(360);
/// Watchdog poll for the accept loop (shutdown/stop checks).
pub const WS_WATCHDOG_POLL: Duration = Duration::from_millis(500);
/// Per-message send bound, mirroring the socket 5 s timeouts.
const WS_SEND_TIMEOUT: Duration = Duration::from_secs(5);
/// Handler cap, mirroring the socket listener.
const WS_HANDLER_CAP: usize = 32;
/// Fixed route path.
pub const WS_PATH: &str = "/ws";

/// WS liveness tuning: ping cadence, silence bound, lifetime cap.
/// Built from [`DaemonConfig`](orator::DaemonConfig) in
/// production; tests pass explicit values.
#[derive(Debug, Clone, Copy)]
pub struct WsTuning {
    /// Silence bound: reap connections with no inbound message longer.
    pub inactivity: Duration,
    /// Keepalive ping cadence for quiet connections.
    pub ping_every: Duration,
    /// Max connection age; `None` disables (tests).
    pub cap: Option<Duration>,
}

impl Default for WsTuning {
    /// Production defaults: 5 s ping, 10 s silence kill, 360 s cap.
    fn default() -> Self {
        Self {
            inactivity: WS_INACTIVITY_TIMEOUT,
            ping_every: WS_PING_EVERY,
            cap: Some(WS_CONNECTION_CAP),
        }
    }
}

impl WsTuning {
    /// Tuning from daemon config. Zero cap disables; ping clamps to
    /// the silence bound so a call always gets a keepalive first.
    #[must_use]
    pub fn from_config(daemon: &orator::config::DaemonConfig) -> Self {
        let inactivity = Duration::from_secs(daemon.ws_inactivity_secs.max(1));
        let ping_every = Duration::from_secs(daemon.ws_ping_secs.max(1))
            .min(inactivity)
            .max(Duration::from_millis(50));
        let cap = (daemon.ws_cap_secs > 0).then(|| Duration::from_secs(daemon.ws_cap_secs));
        Self {
            inactivity,
            ping_every,
            cap,
        }
    }
}

/// Bind `addr` and serve until stopped: same methods, same envelopes
/// as the socket listener. Refuses to start without a token
/// (fail-closed, never unauthenticated) and off loopback.
pub async fn serve_ws(
    addr: &str,
    token: String,
    voice: Arc<VoiceOwner>,
    tuning: WsTuning,
) -> std::io::Result<()> {
    validate_ws_token(&token)
        .map_err(|message| std::io::Error::new(std::io::ErrorKind::InvalidInput, message))?;
    let listener = bind_ws(addr).await?;
    let never = AtomicBool::new(false);
    serve_ws_on_tuned(listener, &token, voice, tuning, &never).await;
    Ok(())
}

/// Minimum bearer-token length. Short tokens refuse to serve
/// (fail closed, never unauthenticated).
pub const MIN_WS_TOKEN_LEN: usize = 32;

/// Validate a WS bearer token: non-empty and at least 32 chars.
/// Short tokens refuse at startup with a named remedy.
pub fn validate_ws_token(token: &str) -> Result<(), String> {
    if token.is_empty() {
        return Err(
            "refusing to serve WebSocket without a bearer token (set daemon.ws_token)".to_string(),
        );
    }
    if token.len() < MIN_WS_TOKEN_LEN {
        return Err(format!(
            "refusing short WS token ({} chars, min {MIN_WS_TOKEN_LEN}; generate 32+ random chars)",
            token.len()
        ));
    }
    Ok(())
}

/// Constant-time bearer comparison: length leaks, content does not.
/// Rejects early on length (tokens share the minimum), then xors every
/// byte so a wrong token costs the same as a right one.
#[must_use]
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Bind a loopback-only TCP listener. Anything else fails closed with
/// the remedy named.
async fn bind_ws(addr: &str) -> std::io::Result<TcpListener> {
    let socket_addr: SocketAddr = addr.parse().map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid WS listen address '{addr}': {error}"),
        )
    })?;
    if !socket_addr.ip().is_loopback() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "refusing non-loopback WS listen address '{addr}' (loopback only; reach it over ssh forwarding)"
            ),
        ));
    }
    TcpListener::bind(socket_addr).await
}

/// Core loop over a bound listener. Stops on the global shutdown flag
/// or the scoped `stop` (tests stop here without touching the global
/// flag other suites observe), then drains the in-flight handlers.
/// Public for embedding and hermetic tests; production serves through
/// [`serve_ws`].
pub async fn serve_ws_on(
    listener: TcpListener,
    token: &str,
    voice: Arc<VoiceOwner>,
    inactivity: Duration,
    stop: &AtomicBool,
) {
    serve_ws_on_tuned(
        listener,
        token,
        voice,
        WsTuning {
            inactivity,
            ping_every: (inactivity / 3).max(Duration::from_millis(50)),
            cap: None,
        },
        stop,
    )
    .await;
}

/// Tuned core loop: explicit ping cadence and lifetime cap on top of
/// the silence bound. Production passes [`WsTuning::from_config`].
pub async fn serve_ws_on_tuned(
    listener: TcpListener,
    token: &str,
    voice: Arc<VoiceOwner>,
    tuning: WsTuning,
    stop: &AtomicBool,
) {
    use std::collections::HashMap;
    use std::net::IpAddr;
    let in_flight = Arc::new(tokio::sync::Semaphore::new(WS_HANDLER_CAP));
    // Per-IP handshake-failure backoff: wrong-token peers wait longer
    // each time (100ms * 2^failures, capped at 5s) inside their own
    // handler, never blocking the accept loop or honest peers.
    let backoff: Arc<Mutex<HashMap<IpAddr, u32>>> = Arc::new(Mutex::new(HashMap::new()));
    let mut handlers = tokio::task::JoinSet::new();
    loop {
        if crate::shutdown_requested() || stop.load(Ordering::SeqCst) {
            break;
        }
        let Ok(accept) = timeout(WS_WATCHDOG_POLL, listener.accept()).await else {
            continue;
        };
        let Ok((mut stream, peer)) = accept else {
            continue;
        };
        let peer_ip = peer.ip();
        {
            let failures = common_core::sync::lock(&backoff)
                .get(&peer_ip)
                .copied()
                .unwrap_or(0);
            if failures > 0 {
                // Shared schedule home: `common_core::retry` — the 5 s cap
                // stays here (per-IP tuning, pinned by
                // `capped_exp_matches_the_ws_per_ip_backoff_table`).
                let wait_ms = common_core::retry::capped_exp_backoff_ms(100, failures, 6).min(5000);
                tokio::time::sleep(Duration::from_millis(wait_ms)).await;
            }
        }
        let Ok(permit) = Arc::clone(&in_flight).try_acquire_owned() else {
            crate::socket::LISTENER_REFUSALS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            use tokio::io::AsyncWriteExt as _;
            let body = serde_json::json!({
                "error": "admission saturated: listener at cap (32 handlers) — retry when a turn completes",
                "kind": "Saturated",
            });
            let text = serde_json::to_string(&body).unwrap_or_default();
            let reply = format!(
                "HTTP/1.1 503 Service Unavailable\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                text.len(),
                text
            );
            let _ = stream.write_all(reply.as_bytes()).await;
            continue;
        };
        let voice = Arc::clone(&voice);
        let token = token.to_string();
        let backoff = Arc::clone(&backoff);
        handlers.spawn(async move {
            let _permit = permit;
            let peer_ip = peer.ip();
            if handle_ws(stream, &token, voice, tuning).await {
                common_core::sync::lock(&backoff).remove(&peer_ip);
            } else {
                let mut map = common_core::sync::lock(&backoff);
                *map.entry(peer_ip).or_insert(0) += 1;
            }
        });
    }
    while handlers.join_next().await.is_some() {}
}

/// Handshake gate: fixed path plus bearer token.
struct Gate {
    expected: String,
}

impl tokio_tungstenite::tungstenite::handshake::server::Callback for Gate {
    // The `Callback` trait fixes the `Result<Response, ErrorResponse>`
    // shape, so the large-error lint is silenced here — the 136-byte
    // refusal cannot box without breaking the handshake contract.
    #[allow(clippy::result_large_err)]
    fn on_request(self, request: &Request, response: Response) -> Result<Response, ErrorResponse> {
        if request.uri().path() != WS_PATH {
            return Err(status_response(404));
        }
        let bearer = request
            .headers()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        // Constant-time compare: a wrong token costs the same as the
        // right one, so timing never oracles the secret.
        if constant_time_eq(bearer, self.expected.as_str()) {
            Ok(response)
        } else {
            Err(status_response(401))
        }
    }
}

/// One connection: handshake-gate, then calls until silence, cap, or
/// shutdown. Every incoming message resets the silence clock. Returns
/// whether the handshake succeeded (for per-IP backoff).
async fn handle_ws(
    stream: TcpStream,
    token: &str,
    voice: Arc<VoiceOwner>,
    tuning: WsTuning,
) -> bool {
    let gate = Gate {
        expected: format!("Bearer {token}"),
    };
    let Ok(mut ws) = accept_hdr_async(stream, gate).await else {
        return false;
    };
    let mut last_active = Instant::now();
    let born = Instant::now();
    loop {
        if crate::shutdown_requested() {
            let _ = ws.close(None).await;
            break;
        }
        if let Some(cap) = tuning.cap {
            if born.elapsed() >= cap {
                let _ = ws.close(None).await;
                break;
            }
        }
        match timeout(tuning.ping_every, ws.next()).await {
            Err(_) => {
                if last_active.elapsed() >= tuning.inactivity {
                    let _ = ws.close(None).await;
                    break;
                }
                if ws.send(Message::Ping(vec![].into())).await.is_err() {
                    break;
                }
            }
            Ok(None | Some(Err(_))) => break,
            Ok(Some(Ok(message))) => match message {
                Message::Binary(bytes) => {
                    last_active = Instant::now();
                    let reply = serve_one(&voice, &bytes).await;
                    if timeout(WS_SEND_TIMEOUT, ws.send(Message::Binary(reply.into())))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Message::Text(_) => {
                    // Strict carrier: replies are binary frames; a text
                    // message names the carrier instead of parsing.
                    last_active = Instant::now();
                    let reply = error_bytes(0, "expected binary frame");
                    if timeout(WS_SEND_TIMEOUT, ws.send(Message::Binary(reply.into())))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Message::Ping(data) => {
                    last_active = Instant::now();
                    if ws.send(Message::Pong(data)).await.is_err() {
                        break;
                    }
                }
                Message::Pong(_) => {
                    last_active = Instant::now();
                }
                Message::Close(_) => break,
                Message::Frame(_) => {}
            },
        }
    }
    true
}

/// One call on a blocking thread (voice boots may take minutes):
/// decode, dispatch through the shared core, encode.
async fn serve_one(voice: &Arc<VoiceOwner>, bytes: &[u8]) -> Vec<u8> {
    let voice = Arc::clone(voice);
    let owned: Vec<u8> = bytes.to_vec();
    tokio::task::spawn_blocking(move || {
        let (id, ok, body) = match SocketFrame::decode(&owned) {
            Err(error) => (0, false, serde_json::json!({ "error": error.to_string() })),
            Ok(frame) => match crate::socket::parse_request_frame(&frame) {
                Err(error) => (0, false, serde_json::json!({ "error": error })),
                Ok((id, method, params)) => {
                    let (ok, body) = crate::socket::serve_call(&voice, &method, &params);
                    (id, ok, body)
                }
            },
        };
        crate::socket::encode_reply_frame(id, ok, &body)
            .map_or_else(|error| error_bytes(0, &error), |frame| frame.encode())
    })
    .await
    .unwrap_or_else(|_| error_bytes(0, "handler panicked"))
}

/// Reply bytes for a carrier-level refusal (no parsable id). The inner
/// envelope is a JSON `Value`, whose serialization never fails; the
/// fallback names the failure instead of sending an empty frame.
fn error_bytes(id: u64, error: &str) -> Vec<u8> {
    crate::socket::encode_reply_frame(
        id,
        false,
        &serde_json::json!({ "error": error.to_string() }),
    )
    .map_or_else(
        |encode_error| {
            // Last resort stays a valid frame: the body names the failure
            // instead of an empty payload desyncing the carrier.
            SocketFrame {
                kind: orator::MessageType::Response,
                body: format!("refusal encode failed ({encode_error}): {error}").into_bytes(),
            }
            .encode()
        },
        |frame| frame.encode(),
    )
}

/// Handshake refusal with a status and no body.
fn status_response(status: u16) -> ErrorResponse {
    tokio_tungstenite::tungstenite::http::Response::builder()
        .status(status)
        .body(None)
        .unwrap_or_else(|_| tokio_tungstenite::tungstenite::http::Response::new(None))
}

#[cfg(test)]
#[path = "../tests/ws.rs"]
mod tests;
