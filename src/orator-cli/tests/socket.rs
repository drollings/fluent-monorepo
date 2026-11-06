//! Socket client tests: envelope round-trip, absent daemon, bad method.

use crate::socket::{
    call_socket_at, call_ws_at, daemon_call, daemon_call_ws, decode_reply, encode_request,
    no_socket_path_text, no_ws_listen_text, no_ws_token_text, parse_transport, refused_text,
    unavailable_socket_text, unavailable_ws_text, CallFailure, Transport, WireReply, WireRequest,
};

#[test]
fn transports_parse_and_reject_garbage() {
    assert_eq!(
        parse_transport("socket").expect("socket"),
        Transport::Socket
    );
    assert_eq!(parse_transport("ws").expect("ws"), Transport::Ws);
    parse_transport("carrier-pigeon").expect_err("garbage must fail");
}

#[test]
fn request_envelope_round_trips_framing() {
    let request = WireRequest {
        id: 7,
        method: "GetState".to_string(),
        params: serde_json::json!({}),
    };
    let bytes = encode_request(&request).expect("request encodes");
    let frame = orator_core::SocketFrame::decode(&bytes).expect("framing holds");
    assert_eq!(frame.kind, orator_core::MessageType::Request);
    let back: WireRequest = serde_json::from_slice(&frame.body).expect("json holds");
    assert_eq!(back, request);
}

#[test]
fn reply_decode_rejects_wrong_family() {
    let frame = orator_core::SocketFrame {
        kind: orator_core::MessageType::Ping,
        body: b"{}".to_vec(),
    };
    assert!(decode_reply(&frame.encode()).is_err());
    let reply = WireReply {
        id: 1,
        ok: true,
        body: serde_json::json!({"state": "idle"}),
    };
    let frame = orator_core::SocketFrame {
        kind: orator_core::MessageType::Response,
        body: serde_json::to_vec(&reply).expect("json"),
    };
    assert_eq!(decode_reply(&frame.encode()).expect("reply"), reply);
}

#[test]
fn bogus_method_is_usage_not_dial() {
    let error = daemon_call("Bogus", serde_json::json!({})).expect_err("bogus fails");
    assert_eq!(error, crate::ExitCode::Usage);
}

#[test]
fn outcome_wording_is_shared_and_exact() {
    // One wording: both renderers format through these constructors,
    // so the CLI and the TUI can never drift apart.
    assert_eq!(
        refused_text("Say", &serde_json::json!({"error": "busy"})),
        "orator: daemon refused Say: {\"error\":\"busy\"}"
    );
    assert_eq!(
        unavailable_socket_text(Some(std::path::Path::new("/run/x/orator.sock"))),
        "orator: daemon unavailable at /run/x/orator.sock (run oratord)"
    );
    assert_eq!(
        unavailable_socket_text(None),
        "orator: daemon unavailable (no socket path: neither XDG_RUNTIME_DIR nor HOME is set)"
    );
    assert_eq!(
        unavailable_ws_text("127.0.0.1:18081"),
        "orator: daemon unavailable at ws://127.0.0.1:18081/ws (run oratord with daemon.ws_listen set)"
    );
    assert_eq!(
        no_socket_path_text(),
        "orator: no socket path: neither XDG_RUNTIME_DIR nor HOME is set"
    );
    assert_eq!(
        no_ws_listen_text(),
        "orator: no WS listen address (set daemon.ws_listen to serve it)"
    );
    assert_eq!(
        no_ws_token_text(),
        "orator: daemon.ws_token is unset; the listener refuses tokenless peers"
    );
}

#[test]
fn quiet_socket_core_returns_text_instead_of_printing() {
    use std::io::{Read as _, Write as _};
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("orator.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
    // Production sockets are owner-only (0600 via `restrict`); the
    // preflight fails closed otherwise, so the mock mimics production
    // or the client correctly refuses before dialing (and the server
    // would hang on accept).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("mock socket owner-only");
    }
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut header = [0u8; 5];
        stream.read_exact(&mut header).expect("header");
        let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let mut rest = vec![0u8; len - 1];
        stream.read_exact(&mut rest).expect("body");
        let reply = serde_json::to_vec(&serde_json::json!({
            "id": 1,
            "ok": false,
            "body": {"error": "busy"},
        }))
        .expect("reply");
        let frame = orator_core::SocketFrame {
            kind: orator_core::MessageType::Response,
            body: reply,
        };
        stream.write_all(&frame.encode()).expect("send");
    });
    let failure: CallFailure =
        call_socket_at(&path, "Say", serde_json::json!({"text": "hi"})).expect_err("refusal fails");
    assert_eq!(failure.code, crate::ExitCode::Error);
    assert_eq!(
        failure.text.as_deref(),
        Some("orator: daemon refused Say: {\"error\":\"busy\"}")
    );
    server.join().expect("server done");
}

#[test]
fn quiet_socket_core_maps_absence_to_shared_wording() {
    // Truly missing socket path is absence (ENOENT → DaemonUnavailable).
    let path = std::path::Path::new("/nonexistent-orator-test-dir-9f3c/nobody.sock");
    let failure: CallFailure =
        call_socket_at(path, "GetState", serde_json::json!({})).expect_err("absent fails");
    assert_eq!(failure.code, crate::ExitCode::DaemonUnavailable);
    assert!(
        failure.text.is_none(),
        "absence carries no text (the caller names the path)"
    );
    // A stale file with no holder refuses with a lined error, never
    // silent absence.
    let dir = tempfile::tempdir().expect("tempdir");
    let stale = dir.path().join("nobody.sock");
    let listener = std::os::unix::net::UnixListener::bind(&stale).expect("bind");
    drop(listener);
    let failure: CallFailure =
        call_socket_at(&stale, "GetState", serde_json::json!({})).expect_err("stale fails");
    assert_eq!(failure.code, crate::ExitCode::Error);
    assert!(failure.text.is_some(), "stale names its failure");
    // The caller-side wording is the shared constructor output.
    let text = unavailable_socket_text(Some(path));
    assert!(
        text.starts_with("orator: daemon unavailable at "),
        "absence names its remedy: {text}"
    );
}

#[test]
fn bogus_method_over_ws_is_usage_not_dial() {
    let error = daemon_call_ws(
        "ws://127.0.0.1:9/ws",
        "token",
        "Bogus",
        serde_json::json!({}),
    )
    .expect_err("bogus fails before dial");
    assert_eq!(error, crate::ExitCode::Usage);
}

#[test]
fn ws_call_round_trips_against_the_real_listener() {
    // The CLI is synchronous: the listener runs on its own runtime
    // thread while the sync client dials from this thread.
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let task_stop = std::sync::Arc::clone(&stop);
    let (addr_tx, addr_rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        common_core::runtime::block_on_shared(async {
            let voice = std::sync::Arc::new(orator_daemon::voice::VoiceOwner::disabled());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("loopback binds");
            addr_tx
                .send(listener.local_addr().expect("bound addr"))
                .expect("addr sends");
            orator_daemon::ws::serve_ws_on(
                listener,
                "cli-test-token",
                voice,
                std::time::Duration::from_secs(30),
                &task_stop,
            )
            .await;
        });
    });
    let addr: std::net::SocketAddr = addr_rx.recv().expect("addr arrives");
    let body = daemon_call_ws(
        &format!("ws://{addr}/ws"),
        "cli-test-token",
        "GetState",
        serde_json::json!({}),
    )
    .expect("round trip");
    assert_eq!(
        body,
        serde_json::json!({"sessions": 0, "voice_server": false})
    );
    let error = daemon_call_ws(
        &format!("ws://{addr}/ws"),
        "wrong-token",
        "GetState",
        serde_json::json!({}),
    )
    .expect_err("wrong token must fail");
    assert_eq!(error, crate::ExitCode::Error);
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    server.join().expect("server drains");
}

#[test]
fn dropped_connection_is_error_not_unavailable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("orator.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("mock socket owner-only");
    }
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        drop(stream);
    });
    let error = crate::socket::daemon_call_at(&path, "GetState", serde_json::json!({}))
        .expect_err("dropped reads must fail");
    assert_eq!(error, crate::ExitCode::Error);
    server.join().expect("server done");
}

#[test]
fn daemon_call_round_trips_through_shared_framing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("orator.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("mock socket owner-only");
    }
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let frame =
            orator_core::SocketFrame::read_from(&mut stream).expect("server reads one frame");
        assert_eq!(frame.kind, orator_core::MessageType::Request);
        let reply = serde_json::to_vec(&serde_json::json!({
            "id": 1,
            "ok": true,
            "body": {"state": "idle"},
        }))
        .expect("json");
        orator_core::SocketFrame {
            kind: orator_core::MessageType::Response,
            body: reply,
        }
        .write_to(&mut stream)
        .expect("server replies");
    });
    let body = crate::socket::daemon_call_at(&path, "GetState", serde_json::json!({}))
        .expect("round trip");
    assert_eq!(body, serde_json::json!({"state": "idle"}));
    server.join().expect("server done");
}

#[test]
fn daemon_refusal_is_error_not_unavailable() {
    use std::io::{Read as _, Write as _};
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("orator.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("mock socket owner-only");
    }
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut header = [0u8; 5];
        stream.read_exact(&mut header).expect("header");
        let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let mut rest = vec![0u8; len - 1];
        stream.read_exact(&mut rest).expect("body");
        let reply = serde_json::to_vec(&serde_json::json!({
            "id": 1,
            "ok": false,
            "body": {"error": "not wired yet"},
        }))
        .expect("json");
        let frame = orator_core::SocketFrame {
            kind: orator_core::MessageType::Response,
            body: reply,
        };
        stream.write_all(&frame.encode()).expect("reply");
    });
    let error = crate::socket::daemon_call_at(&path, "GetState", serde_json::json!({}))
        .expect_err("refusals must fail");
    assert_eq!(error, crate::ExitCode::Error);
    server.join().expect("server done");
}

#[test]
fn ws_listen_binds_the_configured_port() {
    // The configured listen string binds loopback and serves the
    // contract under the configured token — the value round-trips
    // from file to bound socket to answered call.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("orator.json");
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback");
    let port = probe.local_addr().expect("addr").port();
    drop(probe);
    std::fs::write(
        &path,
        format!(r#"{{"daemon": {{"ws_listen": "127.0.0.1:{port}", "ws_token": "bind-token"}}}}"#),
    )
    .expect("config writes");
    let config: orator_core::OratorConfig =
        common_core::config::load_json(&path).expect("configured value loads");
    assert_eq!(config.daemon.ws_listen, format!("127.0.0.1:{port}"));
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let task_stop = std::sync::Arc::clone(&stop);
    let addr: std::net::SocketAddr = config.daemon.ws_listen.parse().expect("listen parses");
    let (bound_tx, bound_rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        common_core::runtime::block_on_shared(async {
            let voice = std::sync::Arc::new(orator_daemon::voice::VoiceOwner::disabled());
            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .expect("configured string binds");
            bound_tx.send(()).expect("bound signals");
            orator_daemon::ws::serve_ws_on(
                listener,
                "bind-token",
                voice,
                std::time::Duration::from_secs(30),
                &task_stop,
            )
            .await;
        });
    });
    bound_rx.recv().expect("listener binds");
    let body = daemon_call_ws(
        &format!("ws://127.0.0.1:{port}/ws"),
        "bind-token",
        "GetState",
        serde_json::json!({}),
    )
    .expect("round trip over the configured port");
    assert_eq!(
        body,
        serde_json::json!({"sessions": 0, "voice_server": false})
    );
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    server.join().expect("server drains");
}

#[test]
fn control_calls_keep_the_short_budget_voice_calls_get_more() {
    use crate::socket::socket_timeout_for;
    assert_eq!(
        socket_timeout_for("GetState"),
        std::time::Duration::from_secs(10)
    );
    assert_eq!(
        socket_timeout_for("ListSessions"),
        std::time::Duration::from_secs(10)
    );
    for method in ["VoiceEndpoint", "Say", "Ask"] {
        assert_eq!(
            socket_timeout_for(method),
            std::time::Duration::from_secs(300),
            "{method} carries the voice budget with progress"
        );
    }
}

#[test]
fn contention_renders_as_retryable_progress_not_read_failure() {
    use crate::say_chain::{classify_daemon_call_failure, DaemonVoiceHint};
    let hint = classify_daemon_call_failure(Some(
        "orator: daemon refused VoiceEndpoint: {\"error\":\"voice server boot in progress — retry shortly (concurrent boot)\",\"kind\":\"BootContention\"}",
    ));
    match hint {
        DaemonVoiceHint::BootContention(reason) => {
            assert!(reason.to_lowercase().contains("concurrent boot"));
        }
        other => panic!("contention must route to BootContention, got {other:?}"),
    }
    let hint = classify_daemon_call_failure(Some(
        "orator: daemon refused VoiceEndpoint: {\"error\":\"voice endpoint ejected\",\"kind\":\"Ejected\"}",
    ));
    assert!(matches!(hint, DaemonVoiceHint::Ejected(_)));
}

#[test]
fn preflight_refuses_without_dialing_unless_explicitly_overridden() {
    use crate::socket::{call_socket_at, socket_preflight_warning};
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("orator.sock");
    // Plant a regular file with the wrong mode: preflight must warn,
    // and the call must refuse before dialing (no hang, no connect).
    std::fs::write(&path, b"not a socket").expect("plant");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("wrong mode");
    }
    assert!(
        socket_preflight_warning(&path).is_some(),
        "wrong mode must warn"
    );
    // Without the explicit override the call refuses with the
    // preflight line (fail closed, never dials a foreign socket).
    std::env::remove_var("ORATOR_INSECURE");
    let failure = call_socket_at(&path, "GetState", serde_json::json!({}))
        .expect_err("preflight must refuse");
    assert_eq!(failure.code, crate::ExitCode::Error);
    // Explicit override dials (and fails on the planted file, not on
    // preflight) — the escape hatch works when asked for.
    std::env::set_var("ORATOR_INSECURE", "1");
    let bypassed = call_socket_at(&path, "GetState", serde_json::json!({}));
    std::env::remove_var("ORATOR_INSECURE");
    assert!(
        bypassed.is_err(),
        "override dials past preflight (then fails on the file)"
    );
}

#[test]
fn connect_error_kinds_map_to_named_codes() {
    use crate::socket::{map_connect_error, permission_denied_text, ws_timeout_text};
    use std::io::{Error, ErrorKind};
    let path = std::path::Path::new("/run/orator.sock");
    let failure = map_connect_error(path, &Error::new(ErrorKind::NotFound, "nope"));
    assert_eq!(failure.code, crate::ExitCode::DaemonUnavailable);
    assert!(failure.text.is_none(), "absence stays silent");
    let failure = map_connect_error(path, &Error::new(ErrorKind::PermissionDenied, "denied"));
    assert_eq!(failure.code, crate::ExitCode::Error);
    assert_eq!(
        failure.text.as_deref(),
        Some(permission_denied_text(path).as_str())
    );
    let failure = map_connect_error(path, &Error::new(ErrorKind::ConnectionRefused, "refused"));
    assert_eq!(failure.code, crate::ExitCode::Error);
    assert!(failure.text.is_some(), "other failures line with remedy");
    assert!(ws_timeout_text("Say").contains("Say"));
}

#[test]
fn ws_garbage_url_is_usage_not_absence() {
    let failure = call_ws_at(":::", "token", "GetState", serde_json::json!({}))
        .expect_err("garbage URL never dials");
    assert_eq!(failure.code, crate::ExitCode::Usage);
}

#[test]
fn enoent_maps_to_absence() {
    let path = std::path::Path::new("/nonexistent-orator-test-dir-9f3c/orator.sock");
    let failure = call_socket_at(path, "GetState", serde_json::json!({}))
        .expect_err("missing socket is absence");
    assert_eq!(failure.code, crate::ExitCode::DaemonUnavailable);
}

#[test]
fn ws_timeout_and_refusal_texts_stay_distinct() {
    let timeout = crate::socket::ws_timeout_text("Ask");
    let refusal = crate::socket::refused_text("Ask", &serde_json::json!({"error": "busy"}));
    assert_ne!(timeout, refusal, "timeout and refusal never read alike");
    assert!(timeout.contains("timed out"), "{timeout}");
    assert!(timeout.contains("Ask"), "{timeout}");
    assert!(refusal.contains("refused"), "{refusal}");
}
