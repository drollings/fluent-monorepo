//! Voice-server tests: resolution order, arg shaping, readiness
//! semantics — all hermetic, no binary, no weights.

use std::path::PathBuf;
use std::time::Duration;

use orator_core::AudioBundle;
use crate::voice::{
    audio_server_home, audio_supervisor_config, managed_binary, resolve_audio_server,
    voice_server_args, wait_ready, AUDIO_SERVER_BIN, AUDIO_SERVER_ENV,
};

fn bundle() -> AudioBundle {
    AudioBundle {
        backbone: std::path::PathBuf::from("/models/backbone.gguf"),
        mmproj: std::path::PathBuf::from("/models/mmproj.gguf"),
        vocoder: std::path::PathBuf::from("/models/vocoder.gguf"),
        tokenizer: std::path::PathBuf::from("/models/tokenizer.gguf"),
    }
}

/// Process env is shared across test threads: the tests below
/// mutate it, so they serialize on the crate-owned lock.
use crate::ENV_LOCK;

#[test]
fn args_carry_the_reference_file_mapping() {
    let args = voice_server_args(&bundle(), 18083);
    let flag = |name: &str| {
        args.iter()
            .position(|arg| arg == name)
            .map(|pos| args[pos + 1].clone())
    };
    assert_eq!(flag("-m").as_deref(), Some("/models/backbone.gguf"));
    assert_eq!(flag("-mm").as_deref(), Some("/models/mmproj.gguf"));
    assert_eq!(flag("-mv").as_deref(), Some("/models/vocoder.gguf"));
    assert_eq!(
        flag("--tts-speaker-file").as_deref(),
        Some("/models/tokenizer.gguf")
    );
    assert_eq!(flag("--host").as_deref(), Some("127.0.0.1"));
    assert_eq!(flag("--port").as_deref(), Some("18083"));
    assert!(!args.iter().any(|arg| arg == "--alias"));
}

#[test]
fn supervisor_argv_matches_the_reference_file_mapping() {
    // The supervised child must boot with byte-identical argv to the
    // retired direct spawn: same bundle, same port, same order.
    let port = 18083;
    let config = audio_supervisor_config(&bundle(), port);
    assert_eq!(
        orator_llm::build_args(&config, None),
        voice_server_args(&bundle(), port)
    );
}

/// Writable fixture base outside `/tmp`: the ownership gate
/// rejects anything under `/tmp`, so fixtures live under the test
/// binary's own directory instead.
fn fixture_dir() -> tempfile::TempDir {
    let base = std::env::current_exe()
        .expect("exe path")
        .parent()
        .expect("exe parent")
        .to_path_buf();
    tempfile::tempdir_in(base).expect("fixture dir")
}

#[cfg(unix)]
fn write_candidate(dir: &std::path::Path, mode: u32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let candidate = dir.join(AUDIO_SERVER_BIN);
    std::fs::write(&candidate, "#!/bin/sh\nexit 0\n").expect("stub writes");
    std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(mode))
        .expect("stub chmod");
    candidate
}

/// Prepend `dir` to `PATH`, returning the previous value for restore.
fn prepend_path(dir: &std::path::Path) -> Option<std::ffi::OsString> {
    let previous = std::env::var_os("PATH");
    let mut paths = vec![dir.to_path_buf()];
    paths.extend(std::env::split_paths(&previous.clone().unwrap_or_default()));
    std::env::set_var("PATH", std::env::join_paths(paths).expect("join"));
    previous
}

#[test]
#[cfg(unix)]
fn resolve_accepts_owner_only_binary() {
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior_server = std::env::var_os(AUDIO_SERVER_ENV);
    let prior_path = std::env::var_os("PATH");
    let prior_home = std::env::var_os("HOME");
    std::env::remove_var(AUDIO_SERVER_ENV);
    let dir = fixture_dir();
    let candidate = write_candidate(dir.path(), 0o755);
    let home = fixture_dir();
    std::env::set_var("HOME", home.path());
    prepend_path(dir.path());
    assert_eq!(resolve_audio_server(), Some(candidate));
    match prior_server {
        Some(value) => std::env::set_var(AUDIO_SERVER_ENV, value),
        None => std::env::remove_var(AUDIO_SERVER_ENV),
    }
    match prior_path {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }
    match prior_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
#[cfg(unix)]
fn resolve_rejects_world_writable_binary() {
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior_server = std::env::var_os(AUDIO_SERVER_ENV);
    let prior_path = std::env::var_os("PATH");
    let prior_home = std::env::var_os("HOME");
    std::env::remove_var(AUDIO_SERVER_ENV);
    let dir = fixture_dir();
    write_candidate(dir.path(), 0o777);
    let home = fixture_dir();
    std::env::set_var("HOME", home.path());
    prepend_path(dir.path());
    assert_eq!(
        resolve_audio_server(),
        None,
        "group/other-writable binaries are refused"
    );
    match prior_server {
        Some(value) => std::env::set_var(AUDIO_SERVER_ENV, value),
        None => std::env::remove_var(AUDIO_SERVER_ENV),
    }
    match prior_path {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }
    match prior_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
#[cfg(unix)]
fn resolve_rejects_world_writable_parent() {
    use std::os::unix::fs::PermissionsExt as _;
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior_server = std::env::var_os(AUDIO_SERVER_ENV);
    let prior_path = std::env::var_os("PATH");
    let prior_home = std::env::var_os("HOME");
    std::env::remove_var(AUDIO_SERVER_ENV);
    let dir = fixture_dir();
    let sub = dir.path().join("wwdir");
    std::fs::create_dir(&sub).expect("subdir");
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o777))
        .expect("subdir chmod");
    write_candidate(&sub, 0o755);
    let home = fixture_dir();
    std::env::set_var("HOME", home.path());
    prepend_path(&sub);
    assert_eq!(
        resolve_audio_server(),
        None,
        "binaries in world-writable directories are refused"
    );
    match prior_server {
        Some(value) => std::env::set_var(AUDIO_SERVER_ENV, value),
        None => std::env::remove_var(AUDIO_SERVER_ENV),
    }
    match prior_path {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }
    match prior_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
#[cfg(unix)]
fn resolve_rejects_tmp_even_when_trusted() {
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior_server = std::env::var_os(AUDIO_SERVER_ENV);
    let prior_path = std::env::var_os("PATH");
    let prior_home = std::env::var_os("HOME");
    std::env::remove_var(AUDIO_SERVER_ENV);
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(
        dir.path().starts_with("/tmp"),
        "this gate only makes sense under /tmp"
    );
    write_candidate(dir.path(), 0o755);
    let home = fixture_dir();
    std::env::set_var("HOME", home.path());
    prepend_path(dir.path());
    assert_eq!(
        resolve_audio_server(),
        None,
        "an owner-only mode does not save a /tmp binary"
    );
    match prior_server {
        Some(value) => std::env::set_var(AUDIO_SERVER_ENV, value),
        None => std::env::remove_var(AUDIO_SERVER_ENV),
    }
    match prior_path {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }
    match prior_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn explicit_override_bypasses_the_gate() {
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior = std::env::var_os(AUDIO_SERVER_ENV);
    std::env::set_var(AUDIO_SERVER_ENV, "/tmp/definitely-not-a-server");
    assert_eq!(
        resolve_audio_server(),
        Some(PathBuf::from("/tmp/definitely-not-a-server")),
        "the user override is the user's responsibility"
    );
    match prior {
        Some(value) => std::env::set_var(AUDIO_SERVER_ENV, value),
        None => std::env::remove_var(AUDIO_SERVER_ENV),
    }
}

#[test]
fn managed_home_follows_the_home_base() {
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let prior_home = std::env::var_os("HOME");
    let prior_data = std::env::var_os("XDG_DATA_HOME");
    let home = fixture_dir();
    std::env::set_var("HOME", home.path());
    std::env::remove_var("XDG_DATA_HOME");
    assert_eq!(
        audio_server_home(),
        Some(home.path().join(".local/share/orator/servers/liquid-audio"))
    );
    assert_eq!(
        managed_binary(),
        audio_server_home().map(|home| home.join(AUDIO_SERVER_BIN))
    );
    std::env::remove_var("HOME");
    assert_eq!(audio_server_home(), None);
    assert_eq!(managed_binary(), None);
    match prior_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    match prior_data {
        Some(value) => std::env::set_var("XDG_DATA_HOME", value),
        None => std::env::remove_var("XDG_DATA_HOME"),
    }
}

#[test]
fn env_override_wins_resolution() {
    let _guard = common_core::sync::lock(&ENV_LOCK);
    std::env::set_var("LLAMA_AUDIO_SERVER", "/opt/audio/bin/server");
    assert_eq!(
        resolve_audio_server(),
        Some(std::path::PathBuf::from("/opt/audio/bin/server"))
    );
    std::env::remove_var("LLAMA_AUDIO_SERVER");
}

#[test]
#[cfg(unix)]
fn path_search_finds_the_binary_name() {
    use std::os::unix::fs::PermissionsExt as _;
    let _guard = common_core::sync::lock(&ENV_LOCK);
    std::env::remove_var("LLAMA_AUDIO_SERVER");
    // Owner-only fixture outside /tmp: the gate refuses /tmp hits.
    let dir = fixture_dir();
    let candidate = dir.path().join(AUDIO_SERVER_BIN);
    std::fs::write(&candidate, "#!/bin/sh\n").expect("stub writes");
    std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o755))
        .expect("stub chmod");
    let path = std::env::join_paths(
        std::iter::once(dir.path().to_path_buf()).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ),
    )
    .expect("join");
    std::env::set_var("ORATOR_TEST_PATH", &path);
    let old = std::env::var_os("PATH");
    std::env::set_var("PATH", &path);
    let found = resolve_audio_server();
    if let Some(previous) = old {
        std::env::set_var("PATH", previous);
    } else {
        std::env::remove_var("PATH");
    }
    std::env::remove_var("ORATOR_TEST_PATH");
    assert_eq!(found, Some(candidate));
}

#[test]
fn readiness_against_a_closed_port_fails_fast() {
    let error = common_core::runtime::block_on_shared(wait_ready(
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
    common_core::runtime::block_on_shared(wait_ready(
        &format!("http://127.0.0.1:{port}"),
        Duration::from_secs(10),
    ))
    .expect("stub answers OPTIONS");
    server.join().expect("server done");
}

/// A stub audio server answering `OPTIONS`: the readiness probe sees 200
/// without loading weights, so `boot` succeeds hermetically.
#[cfg(unix)]
const FAKE_AUDIO_SERVER: &str = r#"#!/usr/bin/env python3
import http.server
import sys

port = 8080
args = sys.argv[1:]
for index, arg in enumerate(args):
    if arg == "--port" and index + 1 < len(args):
        port = int(args[index + 1])


class Handler(http.server.BaseHTTPRequestHandler):
    def do_OPTIONS(self):
        self.send_response(200)
        self.end_headers()

    def log_message(self, *args):
        pass


http.server.HTTPServer(("127.0.0.1", port), Handler).serve_forever()
"#;

#[test]
#[cfg(unix)]
fn stop_kills_and_reaps_the_supervised_child() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().expect("tempdir");
    for name in ["backbone.gguf", "mmproj.gguf", "vocoder.gguf", "tokenizer.gguf"] {
        std::fs::write(dir.path().join(name), b"stub").expect("stub weight");
    }
    let bundle = AudioBundle {
        backbone: dir.path().join("backbone.gguf"),
        mmproj: dir.path().join("mmproj.gguf"),
        vocoder: dir.path().join("vocoder.gguf"),
        tokenizer: dir.path().join("tokenizer.gguf"),
    };
    let script = dir.path().join("fake-audio-server");
    std::fs::write(&script, FAKE_AUDIO_SERVER).expect("stub server writes");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("stub server runs");
    let config = audio_supervisor_config(&bundle, 0);
    let server = common_core::runtime::block_on_shared(orator_llm::boot_with_binary(
        config, &script,
    ))
    .expect("stub server boots");
    let url = server.base_url().to_string();
    let port: u16 = url.rsplit(':').next().expect("port").parse().expect("port parses");
    assert!(
        std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            Duration::from_secs(5),
        )
        .is_ok(),
        "stub serves before stop"
    );
    server.stop();
    assert!(server.is_stopped(), "stop latches");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let dead = std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            Duration::from_millis(200),
        )
        .is_err();
        if dead {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "stop must kill the child; the port still answers"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
