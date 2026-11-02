//! Socket framing tests: round-trip, truncation, cap, type gate, peer rule.

use crate::protocol::MessageType;
use crate::socket::{check_peer_uid, socket_path, SocketFrame, FrameError, MAX_BODY, SOCKET_MODE, SOCKET_NAME};

#[test]
fn round_trip_preserves_type_and_body() {
    for kind in MessageType::ALL {
        let frame = SocketFrame {
            kind,
            body: br#"{"id":1}"#.to_vec(),
        };
        let bytes = frame.encode();
        assert_eq!(SocketFrame::decode(&bytes).expect("round trip"), frame);
    }
}

#[test]
fn empty_body_frames() {
    let frame = SocketFrame {
        kind: MessageType::Ping,
        body: Vec::new(),
    };
    let bytes = frame.encode();
    assert_eq!(SocketFrame::decode(&bytes).expect("empty body"), frame);
}

#[test]
fn short_and_trailing_and_unknown_fail() {
    assert_eq!(SocketFrame::decode(&[1, 0]).unwrap_err(), FrameError::Short);
    let frame = SocketFrame {
        kind: MessageType::Ping,
        body: vec![9],
    };
    let mut bytes = frame.encode();
    bytes.push(0);
    assert_eq!(SocketFrame::decode(&bytes).unwrap_err(), FrameError::Trailing(1));
    let mut bad = frame.encode();
    bad[4] = 200;
    assert_eq!(SocketFrame::decode(&bad).unwrap_err(), FrameError::UnknownType(200));
}

#[test]
fn oversized_bodies_refuse_before_allocating() {
    let mut header = (MAX_BODY as u32 + 1 + 1).to_le_bytes().to_vec();
    header.push(MessageType::Ping.to_u8());
    assert!(matches!(
        SocketFrame::decode(&header).unwrap_err(),
        FrameError::TooLarge(_)
    ));
}

#[test]
fn peer_gate_serves_only_same_uid() {
    assert!(check_peer_uid(1000, 1000));
    assert!(!check_peer_uid(1000, 1001));
    assert!(!check_peer_uid(0, 1000));
}

#[test]
fn socket_constants_hold() {
    assert_eq!(SOCKET_NAME, "orator.sock");
    assert_eq!(SOCKET_MODE, 0o600);
}

#[test]
fn stream_helpers_round_trip_byte_identically() {
    for kind in MessageType::ALL {
        for body in [Vec::new(), br#"{"id":1}"#.to_vec()] {
            let frame = SocketFrame {
                kind,
                body: body.clone(),
            };
            let mut buf = Vec::new();
            frame.write_to(&mut buf).expect("write");
            assert_eq!(buf, frame.encode());
            let mut cursor = std::io::Cursor::new(buf);
            assert_eq!(SocketFrame::read_from(&mut cursor).expect("read"), frame);
        }
    }
}

#[test]
fn stream_helpers_reject_short_oversized_and_unknown() {
    let mut empty = std::io::Cursor::new(Vec::new());
    assert!(matches!(
        SocketFrame::read_from(&mut empty).unwrap_err(),
        FrameError::Io(_)
    ));
    let mut zero_len = std::io::Cursor::new(0u32.to_le_bytes().to_vec());
    assert_eq!(
        SocketFrame::read_from(&mut zero_len).unwrap_err(),
        FrameError::Short
    );
    let mut huge = std::io::Cursor::new((MAX_BODY as u32 + 1 + 1).to_le_bytes().to_vec());
    assert!(matches!(
        SocketFrame::read_from(&mut huge).unwrap_err(),
        FrameError::TooLarge(_)
    ));
    let mut bad_type = (1u32.to_le_bytes()).to_vec();
    bad_type.push(200);
    let mut bad = std::io::Cursor::new(bad_type);
    assert_eq!(
        SocketFrame::read_from(&mut bad).unwrap_err(),
        FrameError::UnknownType(200)
    );
}

#[test]
fn stream_helpers_cover_empty_and_cap_bodies() {
    let empty = SocketFrame {
        kind: MessageType::Ping,
        body: Vec::new(),
    };
    let mut buf = Vec::new();
    empty.write_to(&mut buf).expect("write empty");
    assert_eq!(
        SocketFrame::read_from(&mut std::io::Cursor::new(buf)).expect("read empty"),
        empty
    );
    let big = SocketFrame {
        kind: MessageType::Audio,
        body: vec![7u8; MAX_BODY],
    };
    let mut buf = Vec::new();
    big.write_to(&mut buf).expect("write cap");
    let back =
        SocketFrame::read_from(&mut std::io::Cursor::new(buf)).expect("read cap");
    assert_eq!(back, big);
}

#[test]
fn socket_path_prefers_runtime_dir_then_home() {
    let _guard = common_core::sync::lock(&crate::ENV_LOCK);
    let prior_runtime = std::env::var("XDG_RUNTIME_DIR").ok();
    let prior_home = std::env::var("HOME").ok();
    let dir = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("XDG_RUNTIME_DIR", dir.path());
    std::env::set_var("HOME", home.path());
    let path = socket_path().expect("path resolves");
    assert_eq!(
        path,
        dir.path().join("orator").join(SOCKET_NAME),
        "runtime dir wins"
    );
    std::env::remove_var("XDG_RUNTIME_DIR");
    let path = socket_path().expect("home fallback resolves");
    assert_eq!(
        path,
        home.path().join(".local/share").join("orator").join(SOCKET_NAME),
        "home fallback keeps the format"
    );
    std::env::remove_var("HOME");
    assert!(socket_path().is_none(), "no fallback without a base");
    match prior_runtime {
        Some(value) => std::env::set_var("XDG_RUNTIME_DIR", value),
        None => std::env::remove_var("XDG_RUNTIME_DIR"),
    }
    match prior_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
}
