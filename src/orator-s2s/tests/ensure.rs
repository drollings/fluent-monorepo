//! Assurance tests: shared when served, actionable otherwise —
//! all against stub servers, no binary, no model.

use orator_core::AudioBundle;
use crate::ensure::{ensure_audio_server, served_models_hint, serves_model};

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
        crate::ensure::invalidate_hint_cache(&server.base_url());
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
        crate::ensure::invalidate_hint_cache(&server.base_url());
        server.mock(|when, then| {
            when.method("GET").path("/models");
            then.status(200).json_body(serde_json::json!({"data": []}));
        });
        server.mock(|when, then| {
            when.method("GET").path("/v1/models");
            then.status(200).json_body(serde_json::json!({"data": []}));
        });
        let Err(error) = ensure_audio_server(&server.base_url(), "nope", &bundle()).await
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
        crate::ensure::invalidate_hint_cache(&server.base_url());
        server.mock(|when, then| {
            when.method("GET").path("/models");
            then.status(200).json_body(serde_json::json!({
                "data": [{"id": "m"}],
            }));
        });
        let endpoint = ensure_audio_server(&server.base_url(), "m", &bundle())
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
        crate::ensure::invalidate_hint_cache(&server.base_url());
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
        crate::ensure::invalidate_hint_cache(&server.base_url());
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
        crate::ensure::invalidate_hint_cache(&server.base_url());
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
        crate::ensure::invalidate_hint_cache(&server.base_url());
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
    use crate::ensure::invalidate_hint_cache;
    common_core::runtime::block_on_shared(async {
        let server = httpmock::MockServer::start();
        crate::ensure::invalidate_hint_cache(&server.base_url());
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
        crate::ensure::invalidate_hint_cache(&server.base_url());
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
    let _guard = common_core::sync::lock(&crate::ENV_LOCK);
    let prior = std::env::var_os(crate::voice::AUDIO_SERVER_ENV);
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
    // Owner-only fixture: the ownership gate refuses /tmp, and the
    // tempdir lives there — the explicit override bypasses the gate
    // exactly as a user install would.
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("stub server runs");
    std::env::set_var(crate::voice::AUDIO_SERVER_ENV, &script);
    let endpoint = common_core::runtime::block_on_shared(crate::ensure::boot_audio_server(
        &bundle, "m",
    ))
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
        Some(value) => std::env::set_var(crate::voice::AUDIO_SERVER_ENV, value),
        None => std::env::remove_var(crate::voice::AUDIO_SERVER_ENV),
    }
}
