//! Socket serving tests: contract call succeeds, bogus fails closed,
//! peer mismatch never reads, voice endpoint serves the owner.

use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;

use orator_core::{SocketFrame, MessageType};

use crate::socket::serve_serial;
use crate::voice::VoiceOwner;

fn disabled_voice() -> Arc<VoiceOwner> {
    Arc::new(VoiceOwner::disabled())
}

fn bind_temp() -> (UnixListener, std::path::PathBuf, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("orator.sock");
    let listener = UnixListener::bind(&path).expect("bind");
    (listener, path, dir)
}

/// Scratch directory a strict `bind` accepts: owner-only, like a real
/// private runtime directory.
fn secure_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod 700");
    }
    dir
}

fn call(path: &std::path::Path, method: &str) -> serde_json::Value {
    let mut stream = UnixStream::connect(path).expect("connect");
    let body = serde_json::to_vec(&serde_json::json!({
        "id": 3,
        "method": method,
        "params": {},
    }))
    .expect("json");
    let frame = SocketFrame {
        kind: MessageType::Request,
        body,
    };
    stream.write_all(&frame.encode()).expect("write");
    let mut header = [0u8; 5];
    std::io::Read::read_exact(&mut stream, &mut header).expect("header");
    let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let mut rest = vec![0u8; len - 1];
    std::io::Read::read_exact(&mut stream, &mut rest).expect("body");
    let mut bytes = header.to_vec();
    bytes.extend_from_slice(&rest);
    let reply = SocketFrame::decode(&bytes).expect("reply frames");
    assert_eq!(reply.kind, MessageType::Response);
    serde_json::from_slice(&reply.body).expect("reply json")
}

#[test]
fn contract_calls_execute_and_bogus_fails_closed() {
    let _guard = serve_serial();
    let (listener, path, _dir) = bind_temp();
    let server = std::thread::spawn(move || {
        let voice = VoiceOwner::disabled();
        crate::socket::serve_once(&listener, 1000, 1000, &voice).expect("serve");
    });
    let reply: serde_json::Value = call(&path, "GetState");
    assert_eq!(reply["ok"], serde_json::json!(true));
    assert_eq!(reply["body"]["sessions"], serde_json::json!(0));
    server.join().expect("server done");

    let (listener, path, _dir) = bind_temp();
    let server = std::thread::spawn(move || {
        let voice = VoiceOwner::disabled();
        crate::socket::serve_once(&listener, 1000, 1000, &voice).expect("serve");
    });
    let reply: serde_json::Value = call(&path, "Say");
    assert_eq!(reply["ok"], serde_json::json!(false));
    assert!(
        reply["body"]["error"].as_str().unwrap_or_default().contains("nothing to speak"),
        "missing text names the reason: {reply}"
    );
    server.join().expect("server done");

    let (listener, path, _dir) = bind_temp();
    let server = std::thread::spawn(move || {
        let voice = VoiceOwner::disabled();
        crate::socket::serve_once(&listener, 1000, 1000, &voice).expect("serve");
    });
    let reply: serde_json::Value = call(&path, "Bogus");
    assert_eq!(reply["ok"], serde_json::json!(false));
    server.join().expect("server done");
}

#[test]
fn stop_daemon_signals_shutdown() {
    let _guard = serve_serial();
    crate::clear_shutdown_for_tests();
    let (listener, path, _dir) = bind_temp();
    let server = std::thread::spawn(move || {
        let voice = VoiceOwner::disabled();
        crate::socket::serve_once(&listener, 1000, 1000, &voice).expect("serve");
    });
    let reply: serde_json::Value = call(&path, "StopDaemon");
    assert_eq!(reply["ok"], serde_json::json!(true));
    assert_eq!(reply["body"]["stopping"], serde_json::json!(true));
    server.join().expect("server done");
    assert!(crate::shutdown_requested(), "stop must latch the signal");
    crate::clear_shutdown_for_tests();
}

#[test]
fn peer_mismatch_serves_nothing() {
    let (listener, _path, _dir) = bind_temp();
    let voice = VoiceOwner::disabled();
    let error =
        crate::socket::serve_once(&listener, 1000, 1001, &voice).expect_err("mismatch refuses");
    assert_eq!(error, "peer uid mismatch");
}

#[test]
fn voice_endpoint_fails_closed_without_an_owner() {
    let (listener, path, _dir) = bind_temp();
    let server = std::thread::spawn(move || {
        let voice = VoiceOwner::disabled();
        crate::socket::serve_once(&listener, 1000, 1000, &voice).expect("serve");
    });
    let reply: serde_json::Value = call(&path, "VoiceEndpoint");
    assert_eq!(reply["ok"], serde_json::json!(false));
    assert!(
        reply["body"]["error"].as_str().unwrap_or_default().contains("disabled"),
        "names the remedy: {reply}"
    );
    server.join().expect("server done");
}

/// Serve a real path in the background: a contract call over the
/// bound socket executes.
#[test]
fn serve_answers_over_a_real_path() {
    let _guard = serve_serial();
    crate::clear_shutdown_for_tests();
    let dir = secure_dir();
    let path = dir.path().join("orator.sock");
    std::thread::spawn(move || {
        let voice = disabled_voice();
        let _ = common_core::runtime::block_on(crate::socket::serve(&path, voice));
    });
    let reply = wait_for_reply(&dir.path().join("orator.sock"), "GetState");
    assert_eq!(reply["ok"], serde_json::json!(true));
}

/// A stale socket file does not brick serving: it is reclaimed.
#[test]
fn serve_reclaims_a_stale_socket_file() {
    let _guard = serve_serial();
    crate::clear_shutdown_for_tests();
    let dir = secure_dir();
    let path = dir.path().join("orator.sock");
    std::fs::write(&path, b"stale").expect("stale file");
    std::thread::spawn(move || {
        let voice = disabled_voice();
        let _ = common_core::runtime::block_on(crate::socket::serve(&path, voice));
    });
    let reply = wait_for_reply(&dir.path().join("orator.sock"), "Say");
    assert_eq!(reply["ok"], serde_json::json!(false));
}

/// The bound socket file is owner-only.
#[test]
#[cfg(unix)]
fn serve_binds_owner_only() {
    let _guard = serve_serial();
    crate::clear_shutdown_for_tests();
    let dir = secure_dir();
    let path = dir.path().join("orator.sock");
    std::thread::spawn(move || {
        let voice = disabled_voice();
        let _ = common_core::runtime::block_on(crate::socket::serve(&path, voice));
    });
    let mode = wait_for_mode(&dir.path().join("orator.sock"));
    assert_eq!(mode & 0o777, 0o600, "socket must be owner-only");
}

#[test]
#[cfg(unix)]
fn bind_rejects_insecure_parent_modes() {
    use std::os::unix::fs::PermissionsExt as _;
    let _guard = serve_serial();
    let dir = secure_dir();
    let sockdir = dir.path().join("sock");
    std::fs::create_dir(&sockdir).expect("mkdir");
    std::fs::set_permissions(&sockdir, std::fs::Permissions::from_mode(0o777))
        .expect("chmod 777");
    let path = sockdir.join("orator.sock");
    let error = super::bind(&path).expect_err("0777 parent must refuse");
    assert!(
        error.to_string().contains("700"),
        "names the fix: {error}"
    );
}

#[test]
#[cfg(unix)]
fn parent_check_rejects_a_foreign_owner() {
    let _guard = serve_serial();
    let dir = secure_dir();
    let foreign = orator_core::paths::current_uid().wrapping_add(1);
    let error = super::check_parent(dir.path(), foreign)
        .expect_err("foreign owner must refuse");
    assert!(
        error.to_string().contains("owned by uid"),
        "names the owner: {error}"
    );
    super::check_parent(dir.path(), orator_core::paths::current_uid())
        .expect("own directory passes");
}

#[test]
fn booting_owner_answers_contention_immediately() {
    let _guard = serve_serial();
    let (listener, path, _dir) = bind_temp();
    let voice = VoiceOwner::new(&orator_core::OratorConfig::default());
    voice.set_booting_for_tests(true);
    let server = std::thread::spawn(move || {
        crate::socket::serve_once(&listener, 1000, 1000, &voice).expect("serve");
    });
    let started = std::time::Instant::now();
    let reply: serde_json::Value = call(&path, "VoiceEndpoint");
    let elapsed = started.elapsed();
    assert_eq!(reply["ok"], serde_json::json!(false));
    let body = &reply["body"];
    let kind = body["kind"].as_str().unwrap_or_default();
    let error = body["error"].as_str().unwrap_or_default().to_lowercase();
    assert!(
        kind == "BootContention" || error.contains("concurrent boot") || error.contains("boot"),
        "contention answers retryable, never a read failure: {reply}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "contention is immediate, never a 240 s hold: {elapsed:?}"
    );
    server.join().expect("server done");
}

#[test]
fn over_cap_connections_close_without_a_handler() {    let _guard = serve_serial();
    crate::clear_shutdown_for_tests();
    let dir = secure_dir();
    let path = dir.path().join("orator.sock");
    let serve_path = path.clone();
    std::thread::spawn(move || {
        let voice = disabled_voice();
        let _ = common_core::runtime::block_on(crate::socket::serve(&serve_path, voice));
    });
    wait_for_server(&path);
    let before = crate::socket::listener_refusals();
    let mut held = Vec::new();
    for _ in 0..32 {
        let stream = UnixStream::connect(&path).expect("connect");
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(8)))
            .expect("timeout");
        held.push(stream);
    }
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut saturated = false;
    while std::time::Instant::now() < deadline {
        if probe_saturated(&path) {
            saturated = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    drop(held);
    assert!(saturated, "a connection past the cap must refuse Saturated, not queue");
    assert!(
        crate::socket::listener_refusals() > before,
        "over-cap refusal counts"
    );
}

#[test]
fn slow_header_read_times_out() {
    use std::io::Write as _;
    let _guard = serve_serial();
    crate::clear_shutdown_for_tests();
    let dir = secure_dir();
    let path = dir.path().join("orator.sock");
    let serve_path = path.clone();
    std::thread::spawn(move || {
        let voice = disabled_voice();
        let _ = common_core::runtime::block_on(crate::socket::serve(&serve_path, voice));
    });
    wait_for_server(&path);
    let before = crate::socket::listener_refusals();
    let mut stream = UnixStream::connect(&path).expect("connect");
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(9)))
        .expect("timeout");
    stream.write_all(&[1u8, 0u8]).expect("partial header");
    let mut header = [0u8; 5];
    std::io::Read::read_exact(&mut stream, &mut header).expect("saturated header arrives");
    let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    assert!(len > 1, "saturated reply carries a body");
    let mut rest = vec![0u8; len - 1];
    std::io::Read::read_exact(&mut stream, &mut rest).expect("saturated body arrives");
    let mut bytes = header.to_vec();
    bytes.extend_from_slice(&rest);
    let reply = SocketFrame::decode(&bytes).expect("saturated frame decodes");
    let value: serde_json::Value = serde_json::from_slice(&reply.body).expect("json");
    assert_eq!(value["ok"], serde_json::json!(false));
    assert_eq!(value["body"]["kind"], serde_json::json!("Saturated"));
    assert!(
        crate::socket::listener_refusals() > before,
        "slow-peer refusal counts"
    );
}

#[test]
fn shutdown_drains_in_flight_handlers() {
    use std::io::Write as _;
    let _guard = serve_serial();
    crate::clear_shutdown_for_tests();
    let dir = secure_dir();
    let path = dir.path().join("orator.sock");
    let serve_path = path.clone();
    let started = std::time::Instant::now();
    let server = std::thread::spawn(move || {
        let voice = disabled_voice();
        common_core::runtime::block_on(crate::socket::serve(&serve_path, voice))
    });
    wait_for_server(&path);
    let mut slow = UnixStream::connect(&path).expect("slow connects");
    slow.write_all(&[1u8, 0u8]).expect("partial header");
    let reply = call(&path, "StopDaemon");
    assert_eq!(reply["body"]["stopping"], serde_json::json!(true));
    // Wake the accept loop if it parked after the stop landed: a
    // served call returns fast, an already-stopped loop refuses it.
    let _ = call_once(&path, "GetState");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !server.is_finished() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(server.is_finished(), "serve stops and drains on shutdown");
    assert!(
        started.elapsed() >= std::time::Duration::from_secs(4),
        "serve awaits the stalled handler instead of abandoning it"
    );
    server
        .join()
        .expect("server thread done")
        .expect("serve returns clean");
    crate::clear_shutdown_for_tests();
}

fn wait_for_reply(path: &std::path::Path, method: &str) -> serde_json::Value {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match call_once(path, method) {
            Ok(reply) => return reply,
            Err(_) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(error) => panic!("serve never answered: {error}"),
        }
    }
}

fn call_once(path: &std::path::Path, method: &str) -> std::io::Result<serde_json::Value> {
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
    Ok(call_stream(&mut stream, method))
}

/// Wait until `serve` answers one call at `path`.
fn wait_for_server(path: &std::path::Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match call_once(path, "GetState") {
            Ok(_) => return,
            Err(_) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(error) => panic!("serve never answered: {error}"),
        }
    }
}

/// Whether a fresh connection past the cap refuses with the named
/// `Saturated` kind instead of queueing or closing bare.
fn probe_saturated(path: &std::path::Path) -> bool {
    let Ok(mut stream) = UnixStream::connect(path) else {
        return false;
    };
    if stream
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .is_err()
    {
        return false;
    }
    let body = serde_json::to_vec(&serde_json::json!({
        "id": 3,
        "method": "GetState",
        "params": {},
    }))
    .expect("json");
    let frame = SocketFrame {
        kind: MessageType::Request,
        body,
    };
    use std::io::Write as _;
    if stream.write_all(&frame.encode()).is_err() {
        return false;
    }
    let mut header = [0u8; 5];
    if std::io::Read::read_exact(&mut stream, &mut header).is_err() {
        return false;
    }
    let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    if len == 0 {
        return false;
    }
    let mut rest = vec![0u8; len - 1];
    if std::io::Read::read_exact(&mut stream, &mut rest).is_err() {
        return false;
    }
    let mut bytes = header.to_vec();
    bytes.extend_from_slice(&rest);
    let Ok(reply) = SocketFrame::decode(&bytes) else {
        return false;
    };
    let Ok(value): Result<serde_json::Value, _> = serde_json::from_slice(&reply.body) else {
        return false;
    };
    value["body"]["kind"] == serde_json::json!("Saturated")
        || value["body"]["error"]
            .as_str()
            .unwrap_or_default()
            .to_lowercase()
            .contains("saturated")
}

fn call_stream(stream: &mut UnixStream, method: &str) -> serde_json::Value {
    let body = serde_json::to_vec(&serde_json::json!({
        "id": 3,
        "method": method,
        "params": {},
    }))
    .expect("json");
    let frame = SocketFrame {
        kind: MessageType::Request,
        body,
    };
    stream.write_all(&frame.encode()).expect("write");
    let mut header = [0u8; 5];
    std::io::Read::read_exact(&mut *stream, &mut header).expect("header");
    let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let mut rest = vec![0u8; len - 1];
    std::io::Read::read_exact(&mut *stream, &mut rest).expect("body");
    let mut bytes = header.to_vec();
    bytes.extend_from_slice(&rest);
    let reply = SocketFrame::decode(&bytes).expect("reply frames");
    assert_eq!(reply.kind, MessageType::Response);
    serde_json::from_slice(&reply.body).expect("reply json")
}

#[cfg(unix)]
fn wait_for_mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Ok(metadata) = std::fs::metadata(path) {
            if metadata.file_type().is_socket() {
                return metadata.permissions().mode();
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "socket never appeared"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

#[test]
fn unreadable_credential_never_matches() {
    assert!(!orator_core::check_peer_uid(1000, u32::MAX));
    assert!(orator_core::check_peer_uid(1000, 1000));
    assert!(!orator_core::check_peer_uid(1000, 1001));
}

#[test]
#[cfg(unix)]
fn symlink_socket_path_refuses() {
    let _guard = serve_serial();
    let dir = secure_dir();
    let target = dir.path().join("real.sock");
    let link = dir.path().join("link.sock");
    std::os::unix::fs::symlink(&target, &link).expect("symlink");
    let error = super::bind(&link).expect_err("symlink must refuse");
    assert!(
        error.to_string().contains("symlink"),
        "names the link: {error}"
    );
}

#[test]
fn live_holder_keeps_its_address() {
    let _guard = serve_serial();
    crate::clear_shutdown_for_tests();
    let dir = secure_dir();
    let path = dir.path().join("orator.sock");
    let serve_path = path.clone();
    std::thread::spawn(move || {
        let voice = disabled_voice();
        let _ = common_core::runtime::block_on(crate::socket::serve(&serve_path, voice));
    });
    wait_for_server(&path);
    let error = super::bind(&path).expect_err("live holder refuses rebind");
    assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);
    crate::clear_shutdown_for_tests();
}

#[test]
fn hardening_chmod_failure_refuses_to_serve() {
    let missing = std::path::Path::new("/nonexistent-orator-test-dir-9f3c/orator.sock");
    assert!(
        super::restrict(missing).is_err(),
        "chmod on a missing socket refuses instead of failing open"
    );
}
