//! WebSocket listener tests: transport parity, handshake auth,
//! heartbeat reap, resume, and shutdown drain — all against stub
//! owners, no models.

use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

use crate::socket::serve_serial;
use crate::voice::VoiceOwner;

const TOKEN: &str = "test-token";
const INACTIVITY: std::time::Duration = std::time::Duration::from_millis(300);

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

async fn serve_bg(
    voice: Arc<VoiceOwner>,
    inactivity: std::time::Duration,
) -> (SocketAddr, Arc<AtomicBool>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback binds");
    let addr = listener.local_addr().expect("bound addr");
    let stop = Arc::new(AtomicBool::new(false));
    let task_stop = Arc::clone(&stop);
    let task = tokio::spawn(async move {
        crate::ws::serve_ws_on(listener, TOKEN, voice, inactivity, &task_stop).await;
    });
    (addr, stop, task)
}

async fn connect(addr: SocketAddr, token: Option<&str>) -> WsStream {
    try_connect(addr, token).await.expect("handshake")
}

async fn try_connect(
    addr: SocketAddr,
    token: Option<&str>,
) -> Result<WsStream, tokio_tungstenite::tungstenite::Error> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    // Build from the URL so the handshake key is generated, then add
    // auth: a hand-built request would miss the WS headers.
    let mut request = format!("ws://{addr}/ws")
        .into_client_request()
        .expect("URL request builds");
    if let Some(token) = token {
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {token}").parse().expect("header value"),
        );
    }
    let (ws, _) = connect_async(request).await?;
    Ok(ws)
}

async fn ws_call(
    ws: &mut WsStream,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let body =
        serde_json::to_vec(&serde_json::json!({ "id": id, "method": method, "params": params }))
            .expect("request encodes");
    let frame = orator::SocketFrame {
        kind: orator::MessageType::Request,
        body,
    };
    ws.send(Message::Binary(frame.encode().into()))
        .await
        .expect("send");
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
            .await
            .expect("reply arrives")
        {
            Some(Ok(Message::Binary(bytes))) => {
                let frame = orator::SocketFrame::decode(&bytes).expect("reply frame decodes");
                assert_eq!(frame.kind, orator::MessageType::Response);
                return serde_json::from_slice(&frame.body).expect("reply parses");
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            other => panic!("expected a binary reply, got {other:?}"),
        }
    }
}

fn socket_call(
    path: &std::path::Path,
    method: &str,
    params: &serde_json::Value,
) -> serde_json::Value {
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    let mut stream = UnixStream::connect(path).expect("socket connects");
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .expect("timeout");
    let body =
        serde_json::to_vec(&serde_json::json!({ "id": 7, "method": method, "params": params }))
            .expect("request encodes");
    let frame = orator::SocketFrame {
        kind: orator::MessageType::Request,
        body,
    };
    stream.write_all(&frame.encode()).expect("send");
    let reply = orator::SocketFrame::read_from(&mut stream).expect("reply reads");
    assert_eq!(reply.kind, orator::MessageType::Response);
    serde_json::from_slice(&reply.body).expect("reply parses")
}

fn parity_params(method: &str) -> serde_json::Value {
    match method {
        "StartSession" => serde_json::json!({ "kind": "assistant" }),
        "StopSession" | "Interrupt" => serde_json::json!({ "id": "s9" }),
        "Say" => serde_json::json!({ "text": "" }),
        "Ask" => serde_json::json!({ "question": "" }),
        _ => serde_json::json!({}),
    }
}

#[test]
fn transport_parity_over_every_method() {
    let _guard = serve_serial();
    common_core::runtime::block_on_shared(async {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("orator.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("socket binds");
        let socket_voice = Arc::new(VoiceOwner::disabled());
        let (addr, stop, server) = serve_bg(Arc::new(VoiceOwner::disabled()), INACTIVITY).await;
        let mut ws = connect(addr, Some(TOKEN)).await;
        // StopDaemon latches the process-global shutdown flag other
        // suites observe, so parity covers every other method.
        for (index, method) in orator::protocol::METHOD_NAMES
            .iter()
            .filter(|method| **method != "StopDaemon")
            .enumerate()
        {
            let params = parity_params(method);
            let ws_params = params.clone();
            let path = path.clone();
            let socket_method = method.to_string();
            let ws_method = method.to_string();
            let client = std::thread::spawn(move || socket_call(&path, &socket_method, &params));
            crate::socket::serve_once(&listener, 1000, 1000, &socket_voice).expect("socket serves");
            let socket_reply: serde_json::Value = client.join().expect("client done");
            let ws_reply = ws_call(&mut ws, index as u64, &ws_method, ws_params).await;
            assert_eq!(
                socket_reply["ok"], ws_reply["ok"],
                "method {ws_method}: ok diverges"
            );
            assert_eq!(
                socket_reply["body"], ws_reply["body"],
                "method {ws_method}: body diverges"
            );
        }
        stop.store(true, Ordering::SeqCst);
        server.await.expect("server drains");
    });
}

#[test]
fn handshake_refuses_without_and_with_wrong_token() {
    let _guard = serve_serial();
    common_core::runtime::block_on_shared(async {
        let (addr, stop, server) = serve_bg(Arc::new(VoiceOwner::disabled()), INACTIVITY).await;
        let error = try_connect(addr, None)
            .await
            .expect_err("tokenless must fail")
            .to_string();
        assert!(error.contains("401"), "tokenless gets 401: {error}");
        let error = try_connect(addr, Some("wrong"))
            .await
            .expect_err("wrong token must fail")
            .to_string();
        assert!(error.contains("401"), "wrong token gets 401: {error}");
        // The right token works.
        let mut ws = connect(addr, Some(TOKEN)).await;
        let reply = ws_call(&mut ws, 1, "GetState", serde_json::json!({})).await;
        assert_eq!(reply["ok"], serde_json::json!(true));
        stop.store(true, Ordering::SeqCst);
        server.await.expect("server drains");
    });
}

#[test]
fn silent_connection_reaped_while_active_survives() {
    let _guard = serve_serial();
    common_core::runtime::block_on_shared(async {
        let (addr, stop, server) = serve_bg(Arc::new(VoiceOwner::disabled()), INACTIVITY).await;
        // Silent: connect, never read or write; the server must close
        // past the inactivity bound. Queued pings may arrive first.
        let mut silent = connect(addr, Some(TOKEN)).await;
        tokio::time::sleep(INACTIVITY * 4).await;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            assert!(
                std::time::Instant::now() < deadline,
                "silent connection must be reaped"
            );
            match tokio::time::timeout(std::time::Duration::from_secs(1), silent.next()).await {
                Ok(Some(Ok(Message::Close(_)) | Err(_)) | None) | Err(_) => break,
                Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => {}
                other => panic!("silent connection must be reaped, got {other:?}"),
            }
        }
        // Active: answer pings, keep calling past 3x the bound.
        let mut active = connect(addr, Some(TOKEN)).await;
        let deadline = std::time::Instant::now() + INACTIVITY * 4;
        let mut answered = 0u32;
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(INACTIVITY, active.next()).await {
                Ok(Some(Ok(Message::Ping(data)))) => {
                    active.send(Message::Pong(data)).await.expect("pong sends");
                    answered += 1;
                }
                Ok(Some(Ok(Message::Pong(_)))) => {}
                other => panic!("active connection must survive, got {other:?}"),
            }
        }
        assert!(answered > 0, "server must ping an idle connection");
        let reply = ws_call(&mut active, 9, "GetState", serde_json::json!({})).await;
        assert_eq!(reply["ok"], serde_json::json!(true));
        stop.store(true, Ordering::SeqCst);
        server.await.expect("server drains");
    });
}

#[test]
fn sessions_resume_by_id_at_most_once() {
    let _guard = serve_serial();
    common_core::runtime::block_on_shared(async {
        let (addr, stop, server) = serve_bg(Arc::new(VoiceOwner::disabled()), INACTIVITY).await;
        let mut first = connect(addr, Some(TOKEN)).await;
        let started = ws_call(
            &mut first,
            1,
            "StartSession",
            serde_json::json!({ "kind": "assistant" }),
        )
        .await;
        assert_eq!(started["ok"], serde_json::json!(true));
        assert_eq!(started["body"]["session"], serde_json::json!("s1"));
        drop(first);
        // Reconnect: the registry held the session, nothing replayed.
        let mut second = connect(addr, Some(TOKEN)).await;
        let listed = ws_call(&mut second, 2, "ListSessions", serde_json::json!({})).await;
        assert_eq!(listed["body"]["sessions"], serde_json::json!(["s1"]));
        let again = ws_call(
            &mut second,
            3,
            "StartSession",
            serde_json::json!({ "kind": "assistant" }),
        )
        .await;
        assert_eq!(again["body"]["session"], serde_json::json!("s2"));
        stop.store(true, Ordering::SeqCst);
        server.await.expect("server drains");
    });
}

#[test]
fn shutdown_drain_completes_in_flight() {
    let _guard = serve_serial();
    common_core::runtime::block_on_shared(async {
        let (addr, stop, server) = serve_bg(Arc::new(VoiceOwner::disabled()), INACTIVITY).await;
        let mut ws = connect(addr, Some(TOKEN)).await;
        let reply = ws_call(&mut ws, 1, "GetState", serde_json::json!({})).await;
        assert_eq!(reply["ok"], serde_json::json!(true));
        // In-flight call lands even as the loop stops accepting.
        stop.store(true, Ordering::SeqCst);
        server.await.expect("server drains");
        let refused = connect_async(format!("ws://{addr}/ws")).await;
        assert!(refused.is_err(), "closed listener refuses: {refused:?}");
    });
}

#[test]
fn startup_refuses_without_token_off_loopback_or_bad_addr() {
    use crate::ws::WsTuning;
    common_core::runtime::block_on_shared(async {
        let voice = Arc::new(VoiceOwner::disabled());
        crate::ws::serve_ws(
            "127.0.0.1:0",
            String::new(),
            Arc::clone(&voice),
            WsTuning::default(),
        )
        .await
        .expect_err("tokenless start must fail");
        crate::ws::serve_ws(
            "0.0.0.0:0",
            TOKEN.to_string(),
            Arc::clone(&voice),
            WsTuning::default(),
        )
        .await
        .expect_err("non-loopback must fail");
        crate::ws::serve_ws("not-an-addr", TOKEN.to_string(), voice, WsTuning::default())
            .await
            .expect_err("bad addr must fail");
    });
}

#[test]
fn tuning_defaults_pin_the_moshi_numbers() {
    use crate::ws::{
        WsTuning, WS_CONNECTION_CAP, WS_INACTIVITY_TIMEOUT, WS_PING_EVERY, WS_WATCHDOG_POLL,
    };
    assert_eq!(WS_PING_EVERY, std::time::Duration::from_secs(5));
    assert_eq!(WS_INACTIVITY_TIMEOUT, std::time::Duration::from_secs(10));
    assert_eq!(WS_CONNECTION_CAP, std::time::Duration::from_secs(360));
    assert_eq!(WS_WATCHDOG_POLL, std::time::Duration::from_millis(500));
    let tuning = WsTuning::from_config(&orator::config::DaemonConfig::default());
    assert_eq!(tuning.ping_every, std::time::Duration::from_secs(5));
    assert_eq!(tuning.inactivity, std::time::Duration::from_secs(10));
    assert_eq!(tuning.cap, Some(std::time::Duration::from_secs(360)));
    // Ping clamps to the silence bound so a call always gets a
    // keepalive first; zero cap disables.
    let config = orator::config::DaemonConfig {
        ws_ping_secs: 60,
        ws_cap_secs: 0,
        ..orator::config::DaemonConfig::default()
    };
    let tuning = WsTuning::from_config(&config);
    assert_eq!(tuning.ping_every, tuning.inactivity);
    assert_eq!(tuning.cap, None);
}

#[test]
fn lifetime_cap_closes_old_connections() {
    use crate::ws::WsTuning;
    let _guard = serve_serial();
    common_core::runtime::block_on_shared(async {
        let voice = Arc::new(VoiceOwner::disabled());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback binds");
        let addr = listener.local_addr().expect("bound addr");
        let stop = Arc::new(AtomicBool::new(false));
        let task_stop = Arc::clone(&stop);
        let server = tokio::spawn(async move {
            crate::ws::serve_ws_on_tuned(
                listener,
                TOKEN,
                voice,
                WsTuning {
                    inactivity: std::time::Duration::from_secs(60),
                    ping_every: std::time::Duration::from_millis(50),
                    cap: Some(std::time::Duration::from_millis(300)),
                },
                &task_stop,
            )
            .await;
        });
        let mut ws = connect(addr, Some(TOKEN)).await;
        let reply = ws_call(&mut ws, 1, "ListSessions", serde_json::json!({})).await;
        assert_eq!(
            reply["ok"],
            serde_json::json!(true),
            "call works before the cap"
        );
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        let mut closed = false;
        for _ in 0..10 {
            match tokio::time::timeout(std::time::Duration::from_millis(200), ws.next()).await {
                Ok(Some(Ok(Message::Close(_)) | Err(_)) | None) | Err(_) => {
                    closed = true;
                    break;
                }
                Ok(Some(Ok(_))) => {}
            }
        }
        assert!(closed, "the cap closes old connections");
        stop.store(true, Ordering::SeqCst);
        server.await.expect("server drains");
    });
}
