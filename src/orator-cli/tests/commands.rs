//! Unit tests for the CLI surface: canonical names, no dupes.

use std::collections::HashSet;

use crate::{render_doctor, OutboundRequest, COMMANDS};

#[test]
fn commands_are_unique_and_complete() {
    let set: HashSet<&str> = COMMANDS.into_iter().collect();
    assert_eq!(set.len(), COMMANDS.len(), "no duplicate command names");
    assert_eq!(COMMANDS.len(), 19);
    assert!(set.contains("dictate"));
    assert!(set.contains("doctor"));
    assert!(set.contains("tui"));
}

#[test]
fn outbound_requests_validate_against_the_contract() {
    OutboundRequest::new("Ask").expect("contract method builds");
    OutboundRequest::new("StopDaemon").expect("lifecycle method builds");
    OutboundRequest::new("Reload").expect("lifecycle method builds");
    OutboundRequest::new("Bogus").expect_err("non-contract method fails");
}

#[test]
fn not_implemented_refusal_is_error() {
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
            "body": {"error": "not implemented: Say"},
        }))
        .expect("json");
        let frame = orator_core::SocketFrame {
            kind: orator_core::MessageType::Response,
            body: reply,
        };
        stream.write_all(&frame.encode()).expect("reply");
    });
    let error = crate::socket::daemon_call_at(&path, "Say", serde_json::json!({}))
        .expect_err("not-implemented must fail");
    assert_eq!(error, crate::ExitCode::Error);
    server.join().expect("server done");
}

#[test]
fn doctor_rendering_names_every_section() {
    let report = orator_audio::doctor_report();
    let text = render_doctor(&report);
    assert!(text.contains("devices:"));
    assert!(text.contains("opus:"));
    assert!(text.contains("models:"));
}
