//! Voice readiness probes over loopback stubs: closed ports fail, an
//! OPTIONS 200 stub reads ready.
//!
//! Integration tier (`make test`, never `make smoke`): real sockets and a
//! multi-second timeout budget. The unit contract stays in the fast gate.

use std::time::Duration;

#[test]
fn readiness_against_a_closed_port_fails_fast() {
    let error = common_core::runtime::block_on_shared(orator_s2s::voice::wait_ready(
        "http://127.0.0.1:1",
        Duration::from_secs(2),
    ))
    .expect_err("closed port never readies");
    assert!(error.to_string().contains("not ready"), "{error}");
}

#[test]
fn readiness_accepts_an_options_200_stub() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        use std::io::{Read as _, Write as _};
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
            .expect("reply");
    });
    common_core::runtime::block_on_shared(orator_s2s::voice::wait_ready(
        &format!("http://127.0.0.1:{port}"),
        Duration::from_secs(10),
    ))
    .expect("stub answers OPTIONS");
    server.join().expect("server done");
}
