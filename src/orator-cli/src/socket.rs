//! Daemon socket client: one credentialed request, one reply.
//!
//! The socket file is owner-only in a private runtime directory; the
//! daemon serves only same-uid peers (the pure gate in `orator`
//! decides, the accept path feeds it). Bodies are JSON through the
//! length+type framing; unknown methods never leave this process.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use orator::{MessageType, SocketFrame};

use crate::{ExitCode, OutboundRequest};

/// Request envelope on the wire.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
/// Request envelope on the wire.
pub struct WireRequest {
    /// Correlation id.
    pub id: u64,
    /// Contract method name.
    pub method: String,
    /// Method parameters.
    pub params: serde_json::Value,
}

/// Daemon transport: the Unix socket (default) or the WebSocket
/// listener (same envelopes, same methods, loopback only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Daemon transport: the Unix socket (default) or the WebSocket
/// listener (same envelopes, same methods, loopback only).
pub enum Transport {
    /// Owner-only socket file, same-uid peers.
    Socket,
    /// Loopback WebSocket with a bearer token.
    Ws,
}

/// Parse a `--transport` flag: `socket` or `ws`, anything else is a
/// usage error naming both.
pub fn parse_transport(flag: &str) -> Result<Transport, String> {
    match flag {
        "socket" => Ok(Transport::Socket),
        "ws" => Ok(Transport::Ws),
        _ => Err(format!("orator: unknown transport '{flag}' (socket or ws)")),
    }
}

/// Reply envelope on the wire.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
/// Reply envelope on the wire.
pub struct WireReply {
    /// Correlation id echoed back.
    pub id: u64,
    /// Whether the call succeeded.
    pub ok: bool,
    /// Result or error text.
    pub body: serde_json::Value,
}

/// Failure from a daemon call: the exit code plus the exact line the
/// caller shows. `text` is `None` where the call failed before any
/// wording applies (bad method, dead connection) — the caller names
/// the remedy from the shared constructors below.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Failure from a daemon call: the exit code plus the exact line the
/// caller shows. `text` is `None` where the call failed before any
/// wording applies (bad method, dead connection) — the caller names
/// the remedy from the shared constructors below.
pub struct CallFailure {
    /// Process exit for the failure.
    pub code: ExitCode,
    /// Exact line to show, when the failure has one.
    pub text: Option<String>,
}

impl CallFailure {
    /// Failure with a line to show.
    fn lined(code: ExitCode, text: String) -> Self {
        Self {
            code,
            text: Some(text),
        }
    }

    /// Failure the caller narrates (absence, usage): no line attached.
    fn silent(code: ExitCode) -> Self {
        Self { code, text: None }
    }
}

/// No socket path: neither `XDG_RUNTIME_DIR` nor `HOME` is set.
#[must_use]
/// No socket path: neither `XDG_RUNTIME_DIR` nor `HOME` is set.
pub fn no_socket_path_text() -> String {
    "orator: no socket path: neither XDG_RUNTIME_DIR nor HOME is set".to_string()
}

/// Daemon unreachable over the socket: names the path and the remedy.
#[must_use]
/// Daemon unreachable over the socket: names the path and the remedy.
pub fn unavailable_socket_text(path: Option<&std::path::Path>) -> String {
    match path {
        Some(path) => format!(
            "orator: daemon unavailable at {} (run oratord)",
            path.display()
        ),
        None => {
            "orator: daemon unavailable (no socket path: neither XDG_RUNTIME_DIR nor HOME is set)"
                .to_string()
        }
    }
}

/// WS configured without a listen address.
#[must_use]
/// WS configured without a listen address.
pub fn no_ws_listen_text() -> String {
    "orator: no WS listen address (set daemon.ws_listen to serve it)".to_string()
}

/// WS configured without a bearer token.
#[must_use]
/// WS configured without a bearer token.
pub fn no_ws_token_text() -> String {
    "orator: daemon.ws_token is unset; the listener refuses tokenless peers".to_string()
}

/// Daemon unreachable over the WebSocket listener.
#[must_use]
/// Daemon unreachable over the WebSocket listener.
pub fn unavailable_ws_text(ws_listen: &str) -> String {
    format!(
        "orator: daemon unavailable at ws://{ws_listen}/ws (run oratord with daemon.ws_listen set)"
    )
}

/// Daemon answered `ok: false`: names the method and the body.
#[must_use]
/// Daemon answered `ok: false`: names the method and the body.
pub fn refused_text(method: &str, body: &serde_json::Value) -> String {
    format!("orator: daemon refused {method}: {body}")
}

/// WS handshake rejected: the daemon is there and refused the token.
#[must_use]
/// WS handshake rejected: the daemon is there and refused the token.
pub fn handshake_refused_text() -> String {
    "orator: daemon refused the WS handshake (token rejected)".to_string()
}

/// Daemon write failed mid-call.
#[must_use]
/// Daemon write failed mid-call.
pub fn write_failed_text(error: &str) -> String {
    format!("orator: daemon write failed: {error}")
}

/// Daemon read failed mid-call.
#[must_use]
/// Daemon read failed mid-call.
pub fn read_failed_text(error: &str) -> String {
    format!("orator: daemon read failed: {error}")
}

/// Daemon reply failed to decode.
#[must_use]
/// Daemon reply failed to decode.
pub fn reply_failed_text(error: &str) -> String {
    format!("orator: daemon reply failed: {error}")
}

/// Permission-denied connect: the daemon is there but the socket is
/// not ours to use. Names the chmod/ownership remedy, never absence.
#[must_use]
/// Permission-denied connect: the daemon is there but the socket is
/// not ours to use. Names the chmod/ownership remedy, never absence.
pub fn permission_denied_text(path: &std::path::Path) -> String {
    format!(
        "orator: permission denied on {} (socket owned by another uid or mode is not 0600; check `ls -l` and `chmod 600`, or remove the stale file)",
        path.display()
    )
}

/// WS reply timed out: the daemon is there but the call outran its
/// budget.
#[must_use]
/// WS reply timed out: the daemon is there but the call outran its
/// budget.
pub fn ws_timeout_text(method: &str) -> String {
    format!(
        "orator: daemon timed out on {method} (past its budget; retry or raise the voice budget)"
    )
}

/// Socket-file preflight: warn when the file is not uid-owned or not
/// owner-only, before dialing it.
#[must_use]
/// Socket-file preflight: warn when the file is not uid-owned or not
/// owner-only, before dialing it.
pub fn socket_preflight_warning(path: &std::path::Path) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let metadata = std::fs::metadata(path).ok()?;
        let mode = metadata.permissions().mode() & 0o777;
        let owner = metadata.uid();
        let expected = orator::paths::current_uid();
        if owner != expected || mode != 0o600 {
            return Some(format!(
                "orator: socket {} owned by uid {owner} with mode {mode:o} (expected uid {expected} mode 600)",
                path.display()
            ));
        }
        None
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Encode one request frame. A `WireRequest` (string/number fields
/// only) always serializes; the error propagates instead of an empty
/// body desyncing the frame.
pub fn encode_request(request: &WireRequest) -> Result<Vec<u8>, String> {
    let body = serde_json::to_vec(request)
        .map_err(|error| format!("orator: request encode failed: {error}"))?;
    Ok(SocketFrame {
        kind: MessageType::Request,
        body,
    }
    .encode())
}

/// Decode one reply frame from exact wire bytes.
pub fn decode_reply(bytes: &[u8]) -> Result<WireReply, String> {
    let frame = SocketFrame::decode(bytes).map_err(|error| error.to_string())?;
    if frame.kind != MessageType::Response {
        return Err(format!("expected Response, got {:?}", frame.kind));
    }
    serde_json::from_slice(&frame.body).map_err(|error| error.to_string())
}

/// Call the daemon once. Absence is `DaemonUnavailable`, never a hang:
/// connect and read both time out. A dead connection or a daemon
/// refusal is `Error` (the daemon was there, the call failed) so the
/// caller never reports a running daemon as missing.
pub fn daemon_call(method: &str, params: serde_json::Value) -> Result<serde_json::Value, ExitCode> {
    let Some(path) = orator::socket_path() else {
        eprintln!("{}", no_socket_path_text());
        return Err(ExitCode::DaemonUnavailable);
    };
    report(call_socket_at(&path, method, params))
}

/// Print the failure line (when the failure carries one) and yield the
/// code. The plain CLI renders here; the TUI renders the same text
/// from the quiet cores into its transcript.
fn report(result: Result<serde_json::Value, CallFailure>) -> Result<serde_json::Value, ExitCode> {
    result.map_err(|failure| {
        if let Some(text) = failure.text {
            eprintln!("{text}");
        }
        failure.code
    })
}

/// Call the daemon over the selected transport. The socket path dials
/// the socket file; the WS path resolves the listen address and token
/// from the loaded configuration (the same file the daemon reads).
pub fn daemon_call_transport(
    transport: Transport,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, ExitCode> {
    report(call_transport(transport, method, params))
}

/// Quiet core of [`daemon_call_transport`]: same envelopes, same
/// validation, no printing — the caller renders `text` (stderr for
/// the CLI, a transcript line for the TUI).
pub fn call_transport(
    transport: Transport,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, CallFailure> {
    match transport {
        Transport::Socket => {
            let Some(path) = orator::socket_path() else {
                return Err(CallFailure::lined(
                    ExitCode::DaemonUnavailable,
                    no_socket_path_text(),
                ));
            };
            call_socket_at(&path, method, params)
        }
        Transport::Ws => {
            let (config, _) = orator::config::load_hierarchical();
            let addr = config.daemon.ws_listen;
            if addr.is_empty() {
                return Err(CallFailure::lined(
                    ExitCode::DaemonUnavailable,
                    no_ws_listen_text(),
                ));
            }
            let token = config.daemon.ws_token;
            if token.is_empty() {
                return Err(CallFailure::lined(ExitCode::Error, no_ws_token_text()));
            }
            call_ws_at(&format!("ws://{addr}/ws"), &token, method, params)
        }
    }
}

/// One WS call: handshake with the bearer token, one binary frame in,
/// one binary frame out. Absence is `DaemonUnavailable`; a failed
/// handshake or a daemon refusal is `Error`.
pub fn daemon_call_ws(
    url: &str,
    token: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, ExitCode> {
    report(call_ws_at(url, token, method, params))
}

/// Quiet core of [`daemon_call_ws`]: same handshake, same envelopes,
/// no printing — the caller renders `text`.
pub fn call_ws_at(
    url: &str,
    token: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, CallFailure> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};
    let valid = OutboundRequest::new(method).map_err(|_| CallFailure::silent(ExitCode::Usage))?;
    let mut request = url
        .into_client_request()
        .map_err(|_| CallFailure::silent(ExitCode::Usage))?;
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {token}")
            .parse()
            .map_err(|_| CallFailure::silent(ExitCode::Usage))?,
    );
    let request_id = 1u64;
    let timeout = socket_timeout_for(valid.method.as_str());
    let envelope = WireRequest {
        id: request_id,
        method: valid.method,
        params,
    };
    let bytes =
        encode_request(&envelope).map_err(|error| CallFailure::lined(ExitCode::Error, error))?;
    common_core::runtime::block_on_shared(async {
        let (mut ws, _) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|error| {
                // A 401/403 means the daemon is there and refused (bad
                // token): `Error`, not absence.
                use tokio_tungstenite::tungstenite::Error as WsError;
                match &error {
                    WsError::Http(response)
                        if response.status() == 401 || response.status() == 403 =>
                    {
                        CallFailure::lined(ExitCode::Error, handshake_refused_text())
                    }
                    _ => CallFailure::silent(ExitCode::DaemonUnavailable),
                }
            })?;
        ws.send(Message::Binary(bytes.into()))
            .await
            .map_err(|error| {
                CallFailure::lined(ExitCode::Error, write_failed_text(&error.to_string()))
            })?;
        loop {
            let reply = tokio::time::timeout(timeout, ws.next())
                .await
                .map_err(|_| CallFailure::lined(ExitCode::Error, ws_timeout_text(method)))?
                .ok_or(CallFailure::lined(ExitCode::Error, ws_timeout_text(method)))?
                .map_err(|error| {
                    CallFailure::lined(ExitCode::Error, read_failed_text(&error.to_string()))
                })?;
            match reply {
                Message::Binary(reply_bytes) => {
                    let decoded = decode_reply(&reply_bytes).map_err(|error| {
                        CallFailure::lined(ExitCode::Error, reply_failed_text(&error))
                    })?;
                    if decoded.ok {
                        return Ok(decoded.body);
                    }
                    return Err(CallFailure::lined(
                        ExitCode::Error,
                        refused_text(method, &decoded.body),
                    ));
                }
                Message::Ping(_) | Message::Pong(_) => {}
                _ => return Err(CallFailure::silent(ExitCode::Error)),
            }
        }
    })
}

/// Call the daemon at `path` once. Split from [`daemon_call`] so
/// tests can bind a scratch socket instead of the live one.
pub fn daemon_call_at(
    path: &std::path::Path,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, ExitCode> {
    report(call_socket_at(path, method, params))
}

/// Map a Unix connect failure to its code: absence stays absence,
/// permission failure names the chmod remedy, anything else is a
/// lined read failure. Callers never report a running daemon as
/// missing.
pub(crate) fn map_connect_error(path: &std::path::Path, error: &std::io::Error) -> CallFailure {
    match error.kind() {
        std::io::ErrorKind::NotFound => CallFailure::silent(ExitCode::DaemonUnavailable),
        std::io::ErrorKind::PermissionDenied => {
            CallFailure::lined(ExitCode::Error, permission_denied_text(path))
        }
        _ => CallFailure::lined(ExitCode::Error, read_failed_text(&error.to_string())),
    }
}

/// Quiet core of [`daemon_call_at`]: same framing, same envelopes, no
/// printing — the caller renders `text`. Fails closed on a preflight
/// mismatch (wrong uid/mode) unless the explicit `ORATOR_INSECURE=1`
/// override (or the `--insecure` flag that sets it) is present; the
/// warning text stays the remedy.
pub fn call_socket_at(
    path: &std::path::Path,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, CallFailure> {
    let valid = OutboundRequest::new(method).map_err(|_| CallFailure::silent(ExitCode::Usage))?;
    if let Some(warning) = socket_preflight_warning(path) {
        if std::env::var("ORATOR_INSECURE").unwrap_or_default() != "1" {
            return Err(CallFailure::lined(ExitCode::Error, warning));
        }
        eprintln!("{warning} (override ORATOR_INSECURE=1)");
    }
    let timeout = socket_timeout_for(valid.method.as_str());
    let mut stream = UnixStream::connect(path).map_err(|error| map_connect_error(path, &error))?;
    stream.set_read_timeout(Some(timeout)).map_err(|error| {
        CallFailure::lined(ExitCode::Error, read_failed_text(&error.to_string()))
    })?;
    stream.set_write_timeout(Some(timeout)).map_err(|error| {
        CallFailure::lined(ExitCode::Error, write_failed_text(&error.to_string()))
    })?;
    let request = WireRequest {
        id: 1,
        method: valid.method,
        params,
    };
    let bytes =
        encode_request(&request).map_err(|error| CallFailure::lined(ExitCode::Error, error))?;
    stream.write_all(&bytes).map_err(|error| {
        CallFailure::lined(ExitCode::Error, write_failed_text(&error.to_string()))
    })?;
    let reply = read_reply(&mut stream)
        .map_err(|error| CallFailure::lined(ExitCode::Error, read_failed_text(&error)))?;
    if reply.ok {
        Ok(reply.body)
    } else {
        Err(CallFailure::lined(
            ExitCode::Error,
            refused_text(method, &reply.body),
        ))
    }
}

/// Per-method socket budgets: control calls stay at 10 s; voice
/// calls (`VoiceEndpoint`/`Say`/`Ask`) get a longer bound covering a
/// cold supervised boot plus one turn (progress renders through
/// `BootContention` for siblings, but the first caller holds the
/// handler for the whole boot). One overall chain budget in the caller
/// bounds stacked worker/stream/boot timeouts.
#[must_use]
/// Per-method socket budgets: control calls stay at 10 s; voice
/// calls (`VoiceEndpoint`/`Say`/`Ask`) get a longer bound covering a
/// cold supervised boot plus one turn (progress renders through
/// `BootContention` for siblings, but the first caller holds the
/// handler for the whole boot). One overall chain budget in the caller
/// bounds stacked worker/stream/boot timeouts.
pub fn socket_timeout_for(method: &str) -> Duration {
    match method {
        "VoiceEndpoint" | "Say" | "Ask" => Duration::from_secs(300),
        _ => Duration::from_secs(10),
    }
}

fn read_reply(stream: &mut UnixStream) -> Result<WireReply, String> {
    let frame = SocketFrame::read_from(stream).map_err(|error| error.to_string())?;
    if frame.kind != MessageType::Response {
        return Err(format!("expected Response, got {:?}", frame.kind));
    }
    serde_json::from_slice(&frame.body).map_err(|error| error.to_string())
}

#[cfg(test)]
#[path = "../tests/socket.rs"]
mod tests;