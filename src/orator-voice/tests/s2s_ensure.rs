//! Assurance tests: shared when served, actionable otherwise —
//! all against stub servers, no binary, no model.

use crate::s2s::ensure::{ensure_audio_server, served_models_hint, serves_model};
use orator::AudioBundle;

fn bundle() -> AudioBundle {
    AudioBundle {
        backbone: std::path::PathBuf::from("/models/backbone.gguf"),
        mmproj: std::path::PathBuf::from("/models/mmproj.gguf"),
        vocoder: std::path::PathBuf::from("/models/vocoder.gguf"),
        tokenizer: std::path::PathBuf::from("/models/tokenizer.gguf"),
    }
}

#[test]
fn served_model_uses_the_endpoint_as_is() {
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        crate::s2s::ensure::invalidate_hint_cache(&server.base_url());
        server.mock(|when, then| {
            when.method("GET").path("/models");
            then.status(200).json_body(serde_json::json!({
                "data": [{"id": "liquidai/lfm2.5-audio-1.5b"}],
            }));
        });
        let endpoint = ensure_audio_server(
            &server.base_url(),
            "liquidai/lfm2.5-audio-1.5b",
            &bundle(),
            true,
            false,
        )
        .await
        .expect("served model needs no boot");
        assert_eq!(endpoint.base_url(), server.base_url());
        endpoint.release();
    });
}

#[test]
fn missing_model_without_bundle_fails_actionably() {
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        crate::s2s::ensure::invalidate_hint_cache(&server.base_url());
        server.mock(|when, then| {
            when.method("GET").path("/models");
            then.status(200).json_body(serde_json::json!({"data": []}));
        });
        server.mock(|when, then| {
            when.method("GET").path("/v1/models");
            then.status(200).json_body(serde_json::json!({"data": []}));
        });
        let Err(error) = ensure_audio_server(&server.base_url(), "nope", &bundle(), true, false).await
        else {
            panic!("nothing served, bundle incomplete on disk");
        };
        assert!(error.to_string().contains("incomplete"), "{error}");
    });
}

#[test]
fn shared_release_is_a_noop() {
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        crate::s2s::ensure::invalidate_hint_cache(&server.base_url());
        server.mock(|when, then| {
            when.method("GET").path("/models");
            then.status(200).json_body(serde_json::json!({
                "data": [{"id": "m"}],
            }));
        });
        let endpoint = ensure_audio_server(&server.base_url(), "m", &bundle(), true, false)
            .await
            .expect("served");
        endpoint.release();
        assert!(endpoint.base_url().starts_with("http"));
    });
}

#[test]
fn serves_model_reads_both_model_paths() {
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        crate::s2s::ensure::invalidate_hint_cache(&server.base_url());
        server.mock(|when, then| {
            when.method("GET").path("/models");
            then.status(200).json_body(serde_json::json!({
                "data": [{"id": "m"}],
            }));
        });
        assert!(serves_model(&server.base_url(), "m").await);
        assert!(!serves_model(&server.base_url(), "other").await);
    });
}

#[test]
fn serves_model_falls_back_to_v1_models() {
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        crate::s2s::ensure::invalidate_hint_cache(&server.base_url());
        server.mock(|when, then| {
            when.method("GET").path("/models");
            then.status(404).body("nope");
        });
        server.mock(|when, then| {
            when.method("GET").path("/v1/models");
            then.status(200).json_body(serde_json::json!({
                "data": [{"id": "m"}],
            }));
        });
        assert!(serves_model(&server.base_url(), "m").await);
        assert!(!serves_model(&server.base_url(), "other").await);
    });
}

#[test]
fn serves_model_false_when_nothing_lists() {
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        crate::s2s::ensure::invalidate_hint_cache(&server.base_url());
        server.mock(|when, then| {
            when.method("GET").path("/models");
            then.status(404).body("nope");
        });
        server.mock(|when, then| {
            when.method("GET").path("/v1/models");
            then.status(404).body("nope");
        });
        assert!(!serves_model(&server.base_url(), "m").await);
        assert_eq!(served_models_hint(&server.base_url()).await, String::new());
    });
}

#[test]
fn hint_names_the_served_models() {
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        crate::s2s::ensure::invalidate_hint_cache(&server.base_url());
        server.mock(|when, then| {
            when.method("GET").path("/models");
            then.status(200).json_body(serde_json::json!({
                "data": [{"id": "a"}, {"id": "b"}],
            }));
        });
        assert_eq!(
            served_models_hint(&server.base_url()).await,
            "; endpoint serves: [a, b]"
        );
    });
}

#[test]
fn cached_hint_avoids_a_second_models_hit() {
    use crate::s2s::ensure::invalidate_hint_cache;
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        crate::s2s::ensure::invalidate_hint_cache(&server.base_url());
        let models = server.mock(|when, then| {
            when.method("GET").path("/models");
            then.status(200).json_body(serde_json::json!({
                "data": [{"id": "cached-model"}],
            }));
        });
        assert!(serves_model(&server.base_url(), "cached-model").await);
        assert!(serves_model(&server.base_url(), "cached-model").await);
        models.assert_hits(1);
        invalidate_hint_cache(&server.base_url());
        assert!(serves_model(&server.base_url(), "cached-model").await);
        models.assert_hits(2);
    });
}

#[test]
fn hint_cache_expires_after_its_ttl() {
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        crate::s2s::ensure::invalidate_hint_cache(&server.base_url());
        let models = server.mock(|when, then| {
            when.method("GET").path("/models");
            then.status(200).json_body(serde_json::json!({
                "data": [{"id": "ttl-model"}],
            }));
        });
        assert!(serves_model(&server.base_url(), "ttl-model").await);
        models.assert_hits(1);
        std::thread::sleep(std::time::Duration::from_millis(650));
        assert!(serves_model(&server.base_url(), "ttl-model").await);
        models.assert_hits(2);
    });
}

/// A stub audio server answering `OPTIONS`: the readiness probe sees
/// 200 without loading weights, so `boot_audio_server` succeeds
/// hermetically — the same fake the voice tests boot directly.
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

/// No-leak golden through the public assurance API: boot an owned
/// server over a temp bundle, release it the way the daemon's reap
/// and the CLI's releaser do, and prove the port goes dark — no
/// child survives the release.
#[test]
#[cfg(unix)]
fn boot_release_reaps_the_owned_child() {
    use std::os::unix::fs::PermissionsExt as _;
    let _guard = common_core::sync::lock(&crate::s2s::ENV_LOCK);
    let prior = std::env::var_os(crate::s2s::voice::AUDIO_SERVER_ENV);
    let dir = tempfile::tempdir().expect("tempdir");
    for name in [
        "backbone.gguf",
        "mmproj.gguf",
        "vocoder.gguf",
        "tokenizer.gguf",
    ] {
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
    // Owner-only fixture: the ownership gate refuses /tmp, and the
    // tempdir lives there — the explicit override bypasses the gate
    // exactly as a user install would.
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("stub server runs");
    std::env::set_var(crate::s2s::voice::AUDIO_SERVER_ENV, &script);
    let endpoint = common_core::runtime::block_on_shared(
        crate::s2s::ensure::boot_audio_server(&bundle, "m", true, false),
    )
            .expect("stub server boots");
    let port: u16 = endpoint
        .base_url()
        .rsplit(':')
        .next()
        .expect("port")
        .parse()
        .expect("port parses");
    endpoint.release();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let dead = std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            std::time::Duration::from_millis(200),
        )
        .is_err();
        if dead {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "release must kill the child; the port still answers"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    match prior {
        Some(value) => std::env::set_var(crate::s2s::voice::AUDIO_SERVER_ENV, value),
        None => std::env::remove_var(crate::s2s::voice::AUDIO_SERVER_ENV),
    }
}

#[test]
fn listed_hint_unions_both_paths_in_first_seen_order_without_invalid_entries() {
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        crate::s2s::ensure::invalidate_hint_cache(&server.base_url());
        server.mock(|when, then| {
            when.method("GET").path("/models");
            then.status(200).json_body(serde_json::json!({
                "data": [
                    {"id": "a"},
                    {"object": "model"},
                    {"id": "b"},
                    {"id": 7},
                ],
            }));
        });
        server.mock(|when, then| {
            when.method("GET").path("/v1/models");
            then.status(200).json_body(serde_json::json!({
                "data": [
                    {"id": "b"},
                    {"id": "c"},
                    {"id": null},
                    {"id": "a"},
                ],
            }));
        });
        assert_eq!(
            served_models_hint(&server.base_url()).await,
            "; endpoint serves: [a, b, c]"
        );
    });
}

#[test]
fn audio_build_without_models_list_serves_on_options_readiness() {
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        crate::s2s::ensure::invalidate_hint_cache(&server.base_url());
        // The appliance build serves no `/models` (unmocked paths
        // answer 404); its readiness is the completions `OPTIONS`.
        server.mock(|when, then| {
            when.method("OPTIONS").path("/v1/chat/completions");
            then.status(200);
        });
        assert!(
            serves_model(&server.base_url(), "any-audio-model").await,
            "OPTIONS-ready appliance serves without a listing"
        );
        let endpoint = ensure_audio_server(&server.base_url(), "any-audio-model", &bundle(), true, false)
            .await
            .expect("OPTIONS-ready audio build is shared, never booted");
        assert_eq!(endpoint.base_url(), server.base_url());
        endpoint.release();
    });
}

#[test]
fn auto_boot_off_refuses_before_spawning() {
    common_core::runtime::block_on_shared(async {
        // Nothing serves here and nothing may boot: the refusal names
        // the remedy instead of burning a health timeout on a child.
        match ensure_audio_server("http://127.0.0.1:1", "m", &bundle(), false, false).await {
            Err(error) => assert!(error.to_string().contains("s2s.auto_boot"), "{error}"),
            Ok(_) => panic!("auto-boot off must refuse"),
        }
        match crate::s2s::ensure::boot_audio_server(&bundle(), "m", false, false).await {
            Err(error) => assert!(error.to_string().contains("s2s.auto_boot"), "{error}"),
            Ok(_) => panic!("direct boot refusal"),
        }
    });
}

/// Writable fixture base outside `/tmp`: the ownership gate refuses
/// anything under `/tmp`, so candidate binaries live under the test
/// binary's own directory instead of `tempfile::tempdir()`.
#[cfg(unix)]
fn fixture_base() -> tempfile::TempDir {
    let base = std::env::current_exe()
        .expect("exe path")
        .parent()
        .expect("exe parent")
        .to_path_buf();
    tempfile::tempdir_in(base).expect("fixture base")
}

/// Healthy audio stub with an absolute interpreter path: answers
/// `OPTIONS` 200 on the `--port` argv token, so an isolated `PATH`
/// (no `/usr/bin` for `env`) still boots. Same contract as
/// `FAKE_AUDIO_SERVER`, without the `env` lookup.
#[cfg(unix)]
const FAKE_AUDIO_SERVER_ABS: &str = r#"#!/usr/bin/python3
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

/// M12-S1: ordered audio candidates — first fails, second serves,
/// third never runs. Markers prove attempt order (first ran, third
/// absent); success proves the second served. The per-candidate
/// `eprintln!` log fires once per failure in the same order as the
/// attempts (one log for the first failure; none for the winner or
/// the unattempted third).
#[test]
#[cfg(unix)]
fn boot_audio_server_tries_candidates_in_order_and_stops_after_success() {
    use std::os::unix::fs::PermissionsExt as _;
    let _guard = common_core::sync::lock(&crate::s2s::ENV_LOCK);
    let prior_audio = std::env::var_os(crate::s2s::voice::AUDIO_SERVER_ENV);
    let prior_llama = std::env::var_os(orator_supervise::LLAMA_SERVER_ENV);
    let prior_path = std::env::var_os("PATH");
    let prior_home = std::env::var_os("HOME");
    let prior_xdg = std::env::var_os("XDG_DATA_HOME");
    std::env::remove_var(crate::s2s::voice::AUDIO_SERVER_ENV);
    std::env::remove_var(orator_supervise::LLAMA_SERVER_ENV);

    let base = fixture_base();
    std::env::set_var("HOME", base.path());
    std::env::remove_var("XDG_DATA_HOME");
    for name in ["backbone.gguf", "mmproj.gguf"] {
        std::fs::write(base.path().join(name), b"stub").expect("stub weight");
    }
    let bundle = AudioBundle {
        backbone: base.path().join("backbone.gguf"),
        mmproj: base.path().join("mmproj.gguf"),
        vocoder: std::path::PathBuf::new(),
        tokenizer: std::path::PathBuf::new(),
    };
    let dir1 = base.path().join("a1");
    let dir2 = base.path().join("a2");
    let dir3 = base.path().join("a3");
    for dir in [&dir1, &dir2, &dir3] {
        std::fs::create_dir(dir).expect("candidate dir");
    }
    let marker1 = base.path().join("ran-first");
    let marker3 = base.path().join("ran-third");
    std::fs::write(
        dir1.join(crate::s2s::voice::AUDIO_SERVER_BIN),
        format!("#!/bin/sh\ntouch \"{}\"\nexit 1\n", marker1.display()),
    )
    .expect("failing stub writes");
    std::fs::write(
        dir2.join(crate::s2s::voice::AUDIO_SERVER_BIN),
        FAKE_AUDIO_SERVER_ABS,
    )
    .expect("healthy stub writes");
    std::fs::write(
        dir3.join(crate::s2s::voice::AUDIO_SERVER_BIN),
        format!("#!/bin/sh\ntouch \"{}\"\nsleep 30\n", marker3.display()),
    )
    .expect("unused stub writes");
    for dir in [&dir1, &dir2, &dir3] {
        std::fs::set_permissions(
            dir.join(crate::s2s::voice::AUDIO_SERVER_BIN),
            std::fs::Permissions::from_mode(0o755),
        )
        .expect("stub runs");
    }
    // Isolated PATH: only our three legacy candidates exist, in order.
    // Absolute shebangs keep both shell and python stubs working.
    std::env::set_var(
        "PATH",
        std::env::join_paths([dir1.clone(), dir2.clone(), dir3.clone()]).expect("join"),
    );

    let endpoint = common_core::runtime::block_on_shared(
        crate::s2s::ensure::boot_audio_server(&bundle, "m", true, false),
    );
    let Ok(endpoint) = endpoint else {
        panic!("second candidate serves");
    };
    assert!(
        endpoint.base_url().starts_with("http://127.0.0.1:"),
        "owned loopback endpoint: {}",
        endpoint.base_url()
    );
    assert!(marker1.exists(), "first candidate ran");
    assert!(!marker3.exists(), "third candidate never runs after success");
    endpoint.release();

    match prior_audio {
        Some(value) => std::env::set_var(crate::s2s::voice::AUDIO_SERVER_ENV, value),
        None => std::env::remove_var(crate::s2s::voice::AUDIO_SERVER_ENV),
    }
    match prior_llama {
        Some(value) => std::env::set_var(orator_supervise::LLAMA_SERVER_ENV, value),
        None => std::env::remove_var(orator_supervise::LLAMA_SERVER_ENV),
    }
    match prior_path {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }
    match prior_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    match prior_xdg {
        Some(value) => std::env::set_var("XDG_DATA_HOME", value),
        None => std::env::remove_var("XDG_DATA_HOME"),
    }
}

/// M12-S1: all audio candidates fail — the exact final text carries
/// the last error. Exec-format (spawn) vs immediate exit (exited
/// before healthy) keeps last-error identity observable.
#[test]
#[cfg(unix)]
fn boot_audio_server_returns_last_error_when_all_candidates_fail() {
    use std::os::unix::fs::PermissionsExt as _;
    let _guard = common_core::sync::lock(&crate::s2s::ENV_LOCK);
    let prior_audio = std::env::var_os(crate::s2s::voice::AUDIO_SERVER_ENV);
    let prior_llama = std::env::var_os(orator_supervise::LLAMA_SERVER_ENV);
    let prior_path = std::env::var_os("PATH");
    let prior_home = std::env::var_os("HOME");
    let prior_xdg = std::env::var_os("XDG_DATA_HOME");
    std::env::remove_var(crate::s2s::voice::AUDIO_SERVER_ENV);
    std::env::remove_var(orator_supervise::LLAMA_SERVER_ENV);

    let base = fixture_base();
    std::env::set_var("HOME", base.path());
    std::env::remove_var("XDG_DATA_HOME");
    for name in ["backbone.gguf", "mmproj.gguf"] {
        std::fs::write(base.path().join(name), b"stub").expect("stub weight");
    }
    let bundle = AudioBundle {
        backbone: base.path().join("backbone.gguf"),
        mmproj: base.path().join("mmproj.gguf"),
        vocoder: std::path::PathBuf::new(),
        tokenizer: std::path::PathBuf::new(),
    };
    let dir1 = base.path().join("b1");
    let dir2 = base.path().join("b2");
    for dir in [&dir1, &dir2] {
        std::fs::create_dir(dir).expect("candidate dir");
    }
    std::fs::write(dir1.join(crate::s2s::voice::AUDIO_SERVER_BIN), "not an executable\n")
        .expect("exec-format stub writes");
    std::fs::write(dir2.join(crate::s2s::voice::AUDIO_SERVER_BIN), "#!/bin/sh\nexit 1\n")
        .expect("immediate-exit stub writes");
    for dir in [&dir1, &dir2] {
        std::fs::set_permissions(
            dir.join(crate::s2s::voice::AUDIO_SERVER_BIN),
            std::fs::Permissions::from_mode(0o755),
        )
        .expect("stub mode");
    }
    std::env::set_var(
        "PATH",
        std::env::join_paths([dir1.clone(), dir2.clone()]).expect("join"),
    );

    let error = common_core::runtime::block_on_shared(crate::s2s::ensure::boot_audio_server(
        &bundle, "m", true, false,
    ));
    let Err(error) = error else {
        panic!("all candidates fail");
    };
    let text = error.to_string();
    assert!(
        text.contains("every voice server candidate failed (last: "),
        "exact final text: {text}"
    );
    assert!(
        text.contains("exited before healthy"),
        "last error is the immediate-exit shape: {text}"
    );
    assert!(
        !text.contains("Exec format error"),
        "first error is not returned: {text}"
    );

    match prior_audio {
        Some(value) => std::env::set_var(crate::s2s::voice::AUDIO_SERVER_ENV, value),
        None => std::env::remove_var(crate::s2s::voice::AUDIO_SERVER_ENV),
    }
    match prior_llama {
        Some(value) => std::env::set_var(orator_supervise::LLAMA_SERVER_ENV, value),
        None => std::env::remove_var(orator_supervise::LLAMA_SERVER_ENV),
    }
    match prior_path {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }
    match prior_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    match prior_xdg {
        Some(value) => std::env::set_var("XDG_DATA_HOME", value),
        None => std::env::remove_var("XDG_DATA_HOME"),
    }
}

/// GPU opt-in reaches the supervised spawn argv: a stub server records
/// its argv, and the boot with `gpu = true` must carry the full
/// offload pair. Skips where no GPU device is visible (the opt-in
/// fails actionably there before any spawn).
#[test]
#[cfg(unix)]
fn boot_audio_server_passes_gpu_layers_when_opted_in() {
    use std::os::unix::fs::PermissionsExt as _;
    if !orator::gpu::device_present() {
        eprintln!("SKIP gpu-argv: no GPU device on this machine");
        return;
    }
    let _guard = common_core::sync::lock(&crate::s2s::ENV_LOCK);
    let prior_audio = std::env::var_os(crate::s2s::voice::AUDIO_SERVER_ENV);
    let prior_llama = std::env::var_os(orator_supervise::LLAMA_SERVER_ENV);
    let prior_path = std::env::var_os("PATH");
    let prior_home = std::env::var_os("HOME");
    let prior_xdg = std::env::var_os("XDG_DATA_HOME");
    std::env::remove_var(crate::s2s::voice::AUDIO_SERVER_ENV);
    std::env::remove_var(orator_supervise::LLAMA_SERVER_ENV);

    let base = fixture_base();
    std::env::set_var("HOME", base.path());
    std::env::remove_var("XDG_DATA_HOME");
    for name in ["backbone.gguf", "mmproj.gguf"] {
        std::fs::write(base.path().join(name), b"stub").expect("stub weight");
    }
    let bundle = AudioBundle {
        backbone: base.path().join("backbone.gguf"),
        mmproj: base.path().join("mmproj.gguf"),
        vocoder: std::path::PathBuf::new(),
        tokenizer: std::path::PathBuf::new(),
    };
    let dir = base.path().join("g1");
    std::fs::create_dir(&dir).expect("candidate dir");
    let argv_log = base.path().join("argv.txt");
    std::fs::write(
        dir.join(crate::s2s::voice::AUDIO_SERVER_BIN),
        format!(
            "#!/usr/bin/python3\nimport http.server, sys\nopen({:?}, \"w\").write(\" \".join(sys.argv[1:]))\nport = 8080\nargs = sys.argv[1:]\nfor index, arg in enumerate(args):\n    if arg == \"--port\" and index + 1 < len(args):\n        port = int(args[index + 1])\nclass Handler(http.server.BaseHTTPRequestHandler):\n    def do_OPTIONS(self):\n        self.send_response(200)\n        self.end_headers()\n    def log_message(self, *args):\n        pass\nhttp.server.HTTPServer((\"127.0.0.1\", port), Handler).serve_forever()\n",
            argv_log.display().to_string()
        ),
    )
    .expect("argv-recording stub writes");
    std::fs::set_permissions(
        dir.join(crate::s2s::voice::AUDIO_SERVER_BIN),
        std::fs::Permissions::from_mode(0o755),
    )
    .expect("stub runs");
    std::env::set_var("PATH", std::env::join_paths([dir.clone()]).expect("join"));

    let endpoint = common_core::runtime::block_on_shared(crate::s2s::ensure::boot_audio_server(
        &bundle, "m", true, true,
    ));
    let endpoint = match endpoint {
        Ok(endpoint) => endpoint,
        Err(error) => panic!("gpu boot serves: {error}"),
    };
    endpoint.release();
    let argv = std::fs::read_to_string(&argv_log).expect("stub recorded argv");
    assert!(
        argv.contains("--n-gpu-layers 999"),
        "full offload pair reaches the spawn argv: {argv}"
    );

    match prior_audio {
        Some(value) => std::env::set_var(crate::s2s::voice::AUDIO_SERVER_ENV, value),
        None => std::env::remove_var(crate::s2s::voice::AUDIO_SERVER_ENV),
    }
    match prior_llama {
        Some(value) => std::env::set_var(orator_supervise::LLAMA_SERVER_ENV, value),
        None => std::env::remove_var(orator_supervise::LLAMA_SERVER_ENV),
    }
    match prior_path {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }
    match prior_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    match prior_xdg {
        Some(value) => std::env::set_var("XDG_DATA_HOME", value),
        None => std::env::remove_var("XDG_DATA_HOME"),
    }
}

/// Loaded-status routing: role heads serve through the shared
/// endpoint only from loaded instances. Listing alone never counts
/// (the preset lists unloadable architectures), and unknown shapes,
/// missing ids, and dead endpoints all answer false.
#[test]
fn model_loaded_needs_a_loaded_status() {
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        server.mock(|when, then| {
            when.method("GET").path("/v1/models");
            then.status(200).json_body(serde_json::json!({
                "data": [
                    {"id": "voice-head", "status": {"value": "loaded"}},
                    {"id": "legacy", "status": "loaded"},
                    {"id": "sleeping", "status": {"value": "unloaded"}},
                    {"id": "warming", "status": {"value": "loading"}},
                    {"id": "no-status"},
                ],
            }));
        });
        assert!(crate::s2s::ensure::model_loaded(&server.base_url(), "voice-head").await);
        assert!(crate::s2s::ensure::model_loaded(&server.base_url(), "legacy").await);
        assert!(!crate::s2s::ensure::model_loaded(&server.base_url(), "sleeping").await);
        assert!(!crate::s2s::ensure::model_loaded(&server.base_url(), "warming").await);
        assert!(!crate::s2s::ensure::model_loaded(&server.base_url(), "no-status").await);
        assert!(!crate::s2s::ensure::model_loaded(&server.base_url(), "absent").await);
        assert!(!crate::s2s::ensure::model_loaded("http://127.0.0.1:1", "voice-head").await);
    });
}
