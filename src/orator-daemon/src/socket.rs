//! Credentialed socket serving: one accept, one peer check, one call.
//!
//! The listener lives on an owner-only socket file; each accepted peer
//! passes the uid gate before its first frame is read. Request bodies
//! are `{id, method, params}` JSON; unknown methods get an `ok: false`
//! reply, never a default execution. Content never lands in logs.

#[cfg(test)]
use std::os::unix::net::UnixListener;
#[cfg(test)]
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;

use orator_core::{check_peer_uid, MessageType, SocketFrame};

use crate::voice::VoiceOwner;

/// Serializes serve-loop tests across modules: the shutdown flag is
/// process-global, so any test that runs a listener (socket or WS)
/// holds this while the loop is live.
#[cfg(test)]
pub(crate) static SERVE_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Hold the serve-loop lock for one test.
#[cfg(test)]
pub(crate) fn serve_serial() -> std::sync::MutexGuard<'static, ()> {
    SERVE_SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Test-only single-shot shape: gate the peer, accept, dispatch,
/// reply, return. The production loop below accepts through tokio and
/// reads the credential itself; these helpers keep the pure gate
/// pinnable with injected uids.
#[cfg(test)]
pub fn serve_once(
    listener: &UnixListener,
    own_uid: u32,
    peer_uid: u32,
    voice: &VoiceOwner,
) -> Result<(), String> {
    if !check_peer_uid(own_uid, peer_uid) {
        return Err("peer uid mismatch".to_string());
    }
    let (mut stream, _) = listener.accept().map_err(|error| error.to_string())?;
    handle(&mut stream, voice)
}

/// Test-only already-accepted shape with an injected peer uid, so the
/// pure gate stays pinnable without a live credential.
#[cfg(test)]
pub fn serve_stream(
    stream: &mut UnixStream,
    own_uid: u32,
    peer_uid: u32,
    voice: &VoiceOwner,
) -> Result<(), String> {
    if !check_peer_uid(own_uid, peer_uid) {
        return Err("peer uid mismatch".to_string());
    }
    handle(stream, voice)
}

/// Bind `path` and serve until stopped: accept, gate the peer,
/// answer one call, repeat, then drain the in-flight handlers before
/// returning. The daemon's own uid is read once from the
/// socket file it just created and reused for every peer check.
/// At most 32 handlers run concurrently; connections past the cap are
/// closed without a handler. Each accepted stream carries 5 s
/// read/write timeouts so a slow peer cannot hold a handler forever.
/// `voice` answers `VoiceEndpoint` for every connection; share one
/// owner across the loop so concurrent first callers serialize on a
/// single boot.
/// Listener-level refusals past the handler cap or on slow-peer
/// setup failures. Bumped on every named refusal before the
/// connection drops, so overload stays countable.
pub(crate) static LISTENER_REFUSALS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Current listener-level refusal count.
#[cfg(test)]
#[must_use]
pub(crate) fn listener_refusals() -> u64 {
    LISTENER_REFUSALS.load(std::sync::atomic::Ordering::SeqCst)
}

/// Named overload reply: `ok: false` with the `Saturated` kind so the
/// CLI renders a retryable line instead of a bare close.
fn saturated_body() -> serde_json::Value {
    serde_json::json!({
        "error": "admission saturated: listener at cap (32 handlers) — retry when a turn completes",
        "kind": "Saturated",
    })
}

/// Named slow-peer reply: `ok: false` with the `SlowPeer` kind so the
/// CLI retries instead of suppressing its supervised boot. Slow peers
/// never bump the saturation counter and never report saturation.
fn slow_peer_body() -> serde_json::Value {
    serde_json::json!({
        "error": "slow peer: handler budget elapsed before the frame completed — retry the call",
        "kind": "SlowPeer",
    })
}

/// Single-frame bound for daemon `Say` audio: 1.2M f32 samples (50 s
/// at 24 kHz) encode to about 6.4 MiB of base64, inside the 8 MiB
/// frame cap. Longer turns refuse with `Overflow` instead of
/// truncating mid-sentence.
const MAX_SAY_SAMPLES: usize = 1_200_000;

/// Base64 (standard) encode for the `Say` audio payload.
fn b64(bytes: &[u8]) -> String {
    use base64::prelude::*;
    BASE64_STANDARD.encode(bytes)
}

/// Decode a `Transcribe` audio payload: base64 i16 LE mono at 16 kHz
/// into f32 samples. Wrong rates refuse with the remedy (resample
/// before sending); over-bound inputs refuse before any inference.
fn decode_transcribe_audio(params: &serde_json::Value) -> Result<Vec<f32>, String> {
    let encoded = params
        .get("audio")
        .and_then(|audio| audio.as_str())
        .ok_or_else(|| "Transcribe takes an `audio` param (base64 i16 LE mono)".to_string())?;
    let rate = params
        .get("rate")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(16_000);
    if rate != 16_000 {
        return Err(format!(
            "Transcribe needs 16 kHz mono audio (got {rate} Hz); resample before sending"
        ));
    }
    use base64::prelude::*;
    let raw = BASE64_STANDARD
        .decode(encoded.trim())
        .map_err(|error| format!("Transcribe audio is not base64: {error}"))?;
    if raw.len() % 2 != 0 {
        return Err("Transcribe audio length is not a whole i16 count".to_string());
    }
    if raw.len() / 2 > crate::voice::MAX_TRANSCRIBE_SAMPLES {
        return Err(format!(
            "Transcribe turn of {} samples exceeds the single-frame bound ({} samples); send shorter utterances",
            raw.len() / 2,
            crate::voice::MAX_TRANSCRIBE_SAMPLES,
        ));
    }
    Ok(raw
        .chunks_exact(2)
        .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32768.0)
        .collect())
}

pub async fn serve(path: &Path, voice: Arc<VoiceOwner>) -> std::io::Result<()> {
    let listener = bind(path)?;
    let own_uid = own_uid_of(path);
    let in_flight = Arc::new(tokio::sync::Semaphore::new(32));
    let mut handlers = tokio::task::JoinSet::new();
    loop {
        if crate::shutdown_requested() {
            break;
        }
        let (stream, _) = listener.accept().await?;
        let Ok(permit) = Arc::clone(&in_flight).try_acquire_owned() else {
            LISTENER_REFUSALS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let body = saturated_body();
            let Ok(frame) = encode_reply_frame(0, false, &body) else {
                eprintln!("oratord: saturated refusal encode failed; dropping the peer");
                continue;
            };
            let frame = frame.encode();
            use tokio::io::AsyncWriteExt as _;
            let mut stream = stream;
            let _ = stream.write_all(&frame).await;
            let _ = stream.shutdown().await;
            continue;
        };
        let voice = Arc::clone(&voice);
        handlers.spawn(async move {
            let _permit = permit;
            if let Err(error) = handle_tokio_connection(stream, own_uid, voice).await {
                eprintln!("oratord: refused one call: {error}");
            }
        });
    }
    // Bounded drain: handlers are timeout-bounded, but a degraded
    // voice boot can still outrun shutdown; abandon stragglers with a
    // log line instead of hanging the accept loop forever.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        if handlers.is_empty() {
            break;
        }
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .unwrap_or_default();
        if remaining.is_zero() {
            eprintln!("oratord: serve drain timed out with handlers in flight; exiting anyway");
            break;
        }
        if tokio::time::timeout(remaining, handlers.join_next())
            .await
            .is_err()
        {
            eprintln!("oratord: serve drain timed out with handlers in flight; exiting anyway");
            break;
        }
    }
    Ok(())
}

/// Production connection handler: reads its own peer credential
/// (fail-closed), gates, then reads, dispatches, and replies — all
/// with timeouts so a slow peer never holds a handler. The dispatch
/// itself runs on a blocking thread (same shape as the WS listener),
/// so minutes-long voice boots never park this I/O worker. Callers
/// never feed the uid in.
async fn handle_tokio_connection(
    mut stream: tokio::net::UnixStream,
    own_uid: u32,
    voice: Arc<VoiceOwner>,
) -> Result<(), String> {
    let peer_uid = stream
        .peer_cred()
        .map_or(u32::MAX, |cred| cred.uid());
    if !check_peer_uid(own_uid, peer_uid) {
        return Err("peer uid mismatch".to_string());
    }
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut header = [0u8; 5];
    let read = tokio::time::timeout(std::time::Duration::from_secs(5), stream.read_exact(&mut header)).await;
    let read_ok = matches!(read, Ok(Ok(_)));
    if !read_ok {
        let body = slow_peer_body();
        let frame = encode_reply_frame(0, false, &body)?.encode();
        let _ = stream.write_all(&frame).await;
        return Err("slow peer".to_string());
    }
    let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    if len == 0 || len > (orator_core::MAX_BODY + 1) {
        return Err("bad frame length".to_string());
    }
    let mut rest = vec![0u8; len - 1];
    let read_body = tokio::time::timeout(std::time::Duration::from_secs(5), stream.read_exact(&mut rest)).await;
    if !matches!(read_body, Ok(Ok(_))) {
        let body = slow_peer_body();
        let frame = encode_reply_frame(0, false, &body)?.encode();
        let _ = stream.write_all(&frame).await;
        return Err("slow peer".to_string());
    }
    let mut bytes = header.to_vec();
    bytes.extend_from_slice(&rest);
    let frame = SocketFrame::decode(&bytes).map_err(|error| error.to_string())?;
    let (id, method, params) = parse_request_frame(&frame)?;
    let (ok, body) = tokio::task::spawn_blocking(move || serve_call(&voice, &method, &params))
        .await
        .map_err(|error| format!("serving task failed: {error}"))?;
    let reply = encode_reply_frame(id, ok, &body)?.encode();
    let write = tokio::time::timeout(std::time::Duration::from_secs(5), stream.write_all(&reply)).await;
    if let Ok(Ok(())) = write {
        Ok(())
    } else {
        Err("slow peer on write".to_string())
    }
}

/// Bind the socket at `path`: the parent directory must already be
/// safe (created owner-only on first start, strictly validated
/// afterwards), symlinks refuse, a stale file reclaimed after a
/// connect proves no live holder, the bound file left owner-only. A
/// live holder keeps its address: the bind error returns as-is.
/// Refuses to start when neither `XDG_RUNTIME_DIR` nor `HOME` is set.
/// Synchronous: the std bind hands a non-blocking socket to tokio.
/// Hardening failures never fail open — chmod errors refuse to serve.
fn bind(path: &Path) -> std::io::Result<tokio::net::UnixListener> {
    if std::env::var("XDG_RUNTIME_DIR").is_err() && std::env::var("HOME").is_err() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "neither XDG_RUNTIME_DIR nor HOME is set; refusing to bind outside a private runtime directory",
        ));
    }
    // Every component lstat-ed: a symlink parent refuses instead of
    // serving through it.
    if orator_core::paths::has_symlink_parent(path) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "socket path {} is a symlink or under one; refusing to serve on a link (remove it)",
                path.display()
            ),
        ));
    }
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        check_parent(parent, orator_core::paths::current_uid())?;
    }
    // Bounded probe → unlink → bind retry: a racing starter may win
    // the unlink window, so retry instead of failing the first boot.
    let mut attempts = 0;
    loop {
        match std::os::unix::net::UnixListener::bind(path) {
            Ok(bound) => {
                restrict(path)?;
                bound.set_nonblocking(true)?;
                return tokio::net::UnixListener::from_std(bound);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse && attempts < 3 => {
                if std::os::unix::net::UnixStream::connect(path).is_ok() {
                    return Err(error);
                }
                std::fs::remove_file(path).map_err(|remove| {
                    std::io::Error::new(
                        remove.kind(),
                        format!("stale socket {}: remove failed: {remove}", path.display()),
                    )
                })?;
                // Re-lstat after unlink: a swapped-in link refuses
                // instead of riding the retry.
                if orator_core::paths::has_symlink_parent(path)
                    || std::fs::symlink_metadata(path).is_ok()
                {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!(
                            "socket path {} reappeared as a link after unlink; refusing",
                            path.display()
                        ),
                    ));
                }
                attempts += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Owner-only mode on the bound socket file. Failures refuse to serve
/// instead of failing open.
fn restrict(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Validate the socket parent: owned by `expected_uid` with mode `0700`.
/// A world-accessible or foreign-owned directory fails closed with an
/// actionable message naming the fix.
fn check_parent(parent: &Path, expected_uid: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let metadata = std::fs::metadata(parent)?;
        let owner = metadata.uid();
        if owner != expected_uid {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "socket directory {} owned by uid {owner}, expected uid {expected_uid}; refusing to serve on a foreign directory",
                    parent.display()
                ),
            ));
        }
        let mode = metadata.permissions().mode() & 0o777;
        if mode != 0o700 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "socket directory {} has mode {mode:o}, expected 700; run `chmod 700 {}` or remove it",
                    parent.display(),
                    parent.display()
                ),
            ));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (parent, expected_uid);
        Ok(())
    }
}

/// The binder's own uid, read from the socket file it just created.
/// Unreadable metadata fails closed (`u32::MAX` matches no peer).
fn own_uid_of(path: &Path) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(path).map_or(u32::MAX, |metadata| metadata.uid())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        u32::MAX
    }
}

/// Read one request, dispatch it, write the reply. `VoiceEndpoint`
/// resolves through `voice` (booting on demand); `StopDaemon` signals
/// the accept loop to stop; session methods execute against the
/// registry; `Reload` stays unimplemented by design.
#[cfg(test)]
fn handle(stream: &mut UnixStream, voice: &VoiceOwner) -> Result<(), String> {
    let (id, method, params) = read_request(stream)?;
    let (ok, body) = serve_call(voice, &method, &params);
    write_reply(stream, id, ok, &body)
}

/// Transport-neutral dispatch core: one validated method plus params
/// against the live registry, returning the reply shape. Both
/// listeners call exactly this — same methods, same envelopes, same
/// exit semantics. Must be callable from a blocking thread: voice
/// boots bridge without a per-call runtime.
pub(crate) fn serve_call(
    voice: &VoiceOwner,
    method: &str,
    params: &serde_json::Value,
) -> (bool, serde_json::Value) {
    if method == "VoiceEndpoint" {
        // Serving path with concurrent voice calls: the in-place
        // bridge on runtime workers and blocking threads, the shared
        // runtime where no runtime is active. Never a per-call runtime
        // and never the serializing fallback for concurrent callers;
        // single-flight lives in the boot flag.
        match bridge_concurrent(voice.ensure()) {
            Ok(url) => (true, serde_json::json!({ "url": url })),
            Err(error) => (
                false,
                serde_json::json!({ "error": error.to_string(), "kind": error.kind() }),
            ),
        }
    } else if method == "StopDaemon" {
        match crate::dispatch(method) {
            Ok(()) => {
                crate::request_shutdown();
                (true, serde_json::json!({ "stopping": true }))
            }
            Err(error) => (false, serde_json::json!({ "error": error.to_string() })),
        }
    } else {
        route(voice, method, params)
    }
}

/// Route one validated call against the live registry. Unknown methods
/// fail closed; `Reload` reports its honest refusal; every other
/// contract method does the work.
fn route(voice: &VoiceOwner, method: &str, params: &serde_json::Value) -> (bool, serde_json::Value) {
    if let Err(error) = crate::dispatch(method) {
        return (false, serde_json::json!({ "error": error.to_string() }));
    }
    if let Err(error) = reject_unknown_params(method, params) {
        return (false, serde_json::json!({ "error": error }));
    }
    // Every dispatchable method has an arm below except `VoiceEndpoint`
    // and `StopDaemon`, which `serve_call` answers before routing, and
    // `Reload`, which `dispatch` already refused above. The trailing arm
    // fires only for those three on direct `route` calls (tests and
    // embedding): never silent, never a second dispatch table.
    match method {
        "StartSession" => {
            let kind = params.get("kind").and_then(|value| value.as_str()).unwrap_or("assistant");
            match voice.start_session(kind) {
                Ok(id) => (true, serde_json::json!({ "session": id })),
                Err(error) => (false, serde_json::json!({ "error": error })),
            }
        }
        "StopSession" => {
            let id = params
                .get("session")
                .or_else(|| params.get("id"))
                .and_then(|value| value.as_str())
                .unwrap_or("");
            match voice.stop_session(id) {
                Ok(()) => (true, serde_json::json!({ "stopped": true })),
                Err(error) => (false, serde_json::json!({ "error": error.to_string() })),
            }
        }
        "Interrupt" => {
            let id = params
                .get("session")
                .or_else(|| params.get("id"))
                .and_then(|value| value.as_str())
                .unwrap_or("");
            match voice.interrupt_session(id) {
                Ok(()) => (true, serde_json::json!({ "interrupted": true })),
                Err(error) => (false, serde_json::json!({ "error": error.to_string() })),
            }
        }
        "Say" => {
            let text = params.get("text").and_then(|value| value.as_str()).unwrap_or("");
            let session = params.get("session").and_then(|value| value.as_str());
            match voice.say_blocking(text, session) {
                Ok((samples, rate)) => {
                    if samples.len() > MAX_SAY_SAMPLES {
                        (
                            false,
                            serde_json::json!({
                                "error": format!(
                                    "voice turn of {} samples exceeds the single-frame bound ({} samples); use the audio-server path for long-form speech",
                                    samples.len(),
                                    MAX_SAY_SAMPLES,
                                ),
                                "kind": "Overflow",
                            }),
                        )
                    } else {
                        let mut raw = Vec::with_capacity(samples.len() * 4);
                        for sample in &samples {
                            raw.extend_from_slice(&sample.to_le_bytes());
                        }
                        (true, serde_json::json!({ "audio": b64(&raw), "rate": rate, "samples": samples.len() }))
                    }
                }
                Err(error) => (
                    false,
                    serde_json::json!({ "error": error.to_string(), "kind": error.kind() }),
                ),
            }
        }
        "Ask" => {
            let question = params
                .get("question")
                .or_else(|| params.get("text"))
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let session = params.get("session").and_then(|value| value.as_str());
            match voice.ask_blocking(question, session) {
                Ok(reply) => (true, serde_json::json!({ "reply": reply })),
                Err(error) => (
                    false,
                    serde_json::json!({ "error": error.to_string(), "kind": error.kind() }),
                ),
            }
        }
        "Transcribe" => {
            let session = params.get("session").and_then(|value| value.as_str());
            match decode_transcribe_audio(params) {
                Ok(samples) => match voice.transcribe_blocking(&samples, session) {
                    Ok(text) => (true, serde_json::json!({ "text": text })),
                    Err(error) => (
                        false,
                        serde_json::json!({ "error": error.to_string(), "kind": error.kind() }),
                    ),
                },
                Err(error) => (false, serde_json::json!({ "error": error })),
            }
        }
        "ToggleDictation" => {
            let enabled = params.get("enabled").and_then(serde_json::Value::as_bool);
            let (active, session) = voice.toggle_dictation(enabled);
            match session {
                Some(id) => (true, serde_json::json!({ "enabled": active, "session": id })),
                None => (true, serde_json::json!({ "enabled": active })),
            }
        }
        "ListSessions" => {
            let sessions = voice.list_sessions();
            (true, serde_json::json!({ "sessions": sessions }))
        }
        "GetState" => {
            let count = voice.session_count();
            let server = voice.has_server();
            (true, serde_json::json!({ "sessions": count, "voice_server": server }))
        }
        "Approve" => {
            let id = params.get("id").and_then(|value| value.as_str()).unwrap_or("");
            let approve = params.get("approve").and_then(serde_json::Value::as_bool).unwrap_or(false);
            if id.is_empty() {
                return (false, serde_json::json!({ "error": "Approve takes an `id` param" }));
            }
            if voice.answer_approval(id, approve) {
                (true, serde_json::json!({ "approved": approve }))
            } else {
                (false, serde_json::json!({ "error": format!("unknown approval: {id}") }))
            }
        }
        _ => (false, serde_json::json!({ "error": format!("unknown method: {method}") })),
    }
}

/// Reject params the method never reads. Unknown keys fail closed
/// instead of absorbing silently.
fn reject_unknown_params(method: &str, params: &serde_json::Value) -> Result<(), String> {
    let allowed: &[&str] = match method {
        "StartSession" => &["kind"],
        "StopSession" | "Interrupt" => &["session", "id"],
        "Say" => &["text", "session"],
        "Ask" => &["question", "text", "session"],
        "Transcribe" => &["audio", "rate", "session"],
        "ToggleDictation" => &["enabled"],
        "Approve" => &["id", "approve"],
        "ListSessions" | "GetState" | "VoiceEndpoint" | "StopDaemon" => &[],
        _ => return Ok(()),
    };
    let Some(map) = params.as_object() else {
        if params.is_null() {
            return Ok(());
        }
        return Err(format!("{method} params must be an object"));
    };
    for key in map.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(format!("{method} takes no `{key}` param"));
        }
    }
    Ok(())
}

/// Test seam for the param-rejection golden.
#[cfg(test)]
pub(crate) fn route_for_tests(
    voice: &VoiceOwner,
    method: &str,
    params: &serde_json::Value,
) -> (bool, serde_json::Value) {
    route(voice, method, params)
}

#[cfg(test)]
fn read_request(stream: &mut UnixStream) -> Result<(u64, String, serde_json::Value), String> {
    let frame = SocketFrame::read_from(stream).map_err(|error| error.to_string())?;
    parse_request_frame(&frame)
}

/// Split one wire frame into its call: the JSON envelope both
/// listeners share. Unknown shapes fail closed, never defaulted.
pub(crate) fn parse_request_frame(
    frame: &SocketFrame,
) -> Result<(u64, String, serde_json::Value), String> {
    if frame.kind != MessageType::Request {
        return Err(format!("expected Request, got {:?}", frame.kind));
    }
    let request: serde_json::Value =
        serde_json::from_slice(&frame.body).map_err(|error| error.to_string())?;
    let id = request.get("id").and_then(serde_json::Value::as_u64).unwrap_or(0);
    let method = request
        .get("method")
        .and_then(|method| method.as_str())
        .unwrap_or("")
        .to_string();
    let params = request.get("params").cloned().unwrap_or(serde_json::Value::Null);
    Ok((id, method, params))
}

/// Bridge a serving-path future without a per-call runtime: on a
/// runtime worker or blocking thread use the in-place bridge (never
/// starves the worker); on a thread with no runtime use the
/// process-wide shared runtime (callers do not serialize). Single-flight
/// still lives in the boot flag.
///
/// Thread evidence (verified with a scratch probe, not assumed):
/// `Handle::try_current` succeeds on both async workers and
/// `spawn_blocking` threads, so no heuristic can split them — and none
/// needs to, because `block_in_place` serves both. The serializing
/// single-thread fallback below only affects runtime-less threads.
pub(crate) fn bridge_concurrent<F: std::future::Future>(fut: F) -> F::Output {
    if tokio::runtime::Handle::try_current().is_ok() {
        common_core::runtime::block_on(fut)
    } else {
        common_core::runtime::block_on_shared(fut)
    }
}

#[cfg(test)]
fn write_reply(
    stream: &mut UnixStream,
    id: u64,
    ok: bool,
    body: &serde_json::Value,
) -> Result<(), String> {
    encode_reply_frame(id, ok, body)?
        .write_to(stream)
        .map_err(|error| error.to_string())
}

/// One reply frame: the JSON envelope both listeners share. Encoding
/// a JSON `Value` never fails, but the error propagates instead of an
/// empty body desyncing the frame.
pub(crate) fn encode_reply_frame(
    id: u64,
    ok: bool,
    body: &serde_json::Value,
) -> Result<SocketFrame, String> {
    let reply = serde_json::json!({ "id": id, "ok": ok, "body": body });
    let bytes = serde_json::to_vec(&reply)
        .map_err(|error| format!("orator: reply encode failed: {error}"))?;
    Ok(SocketFrame {
        kind: MessageType::Response,
        body: bytes,
    })
}

#[cfg(test)]
#[path = "../tests/socket.rs"]
mod tests;
