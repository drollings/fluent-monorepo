//! Readiness/fallback calibration: dead, text-only, and audio-ready
//! control groups measured through the shared probe and the
//! task-value turn outcome.
//!
//! The probe is producer confidence (cheap, can be wrong); audio
//! actually flowing is task-value (expensive, gates fallback and
//! reaping). The aggregate test scores the probe as a predictor of
//! audio through the shared calibration harness — the numbers feed
//! the calibration register, and the production 405 policy stays
//! strict on their strength.

use common_core::calibration::calibrate_threshold;

use orator_voice::s2s::config::S2sConfig;
use orator_voice::s2s::session::{AudioSession, S2sError};

/// One control case: did the readiness probe fire, and did a real
/// turn produce audio?
struct Case {
    probe_ready: bool,
    audio_flowed: bool,
}

fn session_at(base: &str) -> AudioSession {
    AudioSession::new(S2sConfig {
        endpoint: base.to_string(),
        ..S2sConfig::default()
    })
}

fn probe(base: &str) -> bool {
    let supervisor = orator_agent::llm::Supervisor::attach_with_probe(
        base.to_string(),
        orator_agent::llm::HealthProbe::OptionsCompletions,
    );
    common_core::runtime::block_on_shared(
        supervisor.wait_healthy_timeout(std::time::Duration::from_secs(2)),
    )
    .is_ok()
}

fn audio_flows(session: &mut AudioSession) -> bool {
    session.speak_blocking("hi").is_ok()
}

fn refused_case() -> Case {
    let mut session = session_at("http://127.0.0.1:1");
    Case {
        probe_ready: probe("http://127.0.0.1:1"),
        audio_flowed: audio_flows(&mut session),
    }
}

fn failing_case() -> Case {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("OPTIONS").path("/v1/chat/completions");
        then.status(500).body("sick");
    });
    server.mock(|when, then| {
        when.method("POST").path("/v1/chat/completions");
        then.status(500).body("sick");
    });
    let mut session = session_at(&server.base_url());
    Case {
        probe_ready: probe(&server.base_url()),
        audio_flowed: audio_flows(&mut session),
    }
}

fn text_only_case() -> (Case, S2sError) {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("OPTIONS").path("/v1/chat/completions");
        then.status(405).body("method not allowed");
    });
    server.mock(|when, then| {
        when.method("GET").path("/models");
        then.status(200).json_body(serde_json::json!({
            "data": [{"id": "m"}],
        }));
    });
    server.mock(|when, then| {
        when.method("POST").path("/v1/chat/completions");
        then.status(200).body(
            "data: {\"choices\":[{\"delta\":{\"content\":\"text only\"}}]}\n\
             data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n",
        );
    });
    let mut session = session_at(&server.base_url());
    let outcome = session.speak_blocking("hi").expect_err("no audio served");
    (
        Case {
            probe_ready: probe(&server.base_url()),
            audio_flowed: false,
        },
        outcome,
    )
}

fn audio_ready_case() -> (Case, Vec<f32>) {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("OPTIONS").path("/v1/chat/completions");
        then.status(200);
    });
    server.mock(|when, then| {
        when.method("POST").path("/v1/chat/completions");
        then.status(200).body(format!(
            "data: {{\"choices\":[{{\"delta\":{{\"audio_chunk\":{{\"data\":\"{}\"}}}}}}]}}\n\
             data: [DONE]\n",
            {
                use base64::prelude::*;
                BASE64_STANDARD.encode(2.5f32.to_le_bytes())
            }
        ));
    });
    let mut session = session_at(&server.base_url());
    let samples = session.speak_blocking("hi").expect("audio served");
    (
        Case {
            probe_ready: probe(&server.base_url()),
            audio_flowed: !samples.is_empty(),
        },
        samples,
    )
}

#[ignore = "heavy (~1.5s): readiness/fallback threshold calibration with real-timer probes; run via `make test-heavy`"]
#[test]
fn dead_servers_are_never_ready() {
    for case in [refused_case(), failing_case()] {
        assert!(!case.probe_ready, "a dead server must not read ready");
        assert!(
            !case.audio_flowed,
            "no turn can voice through a dead server"
        );
    }
}

#[ignore = "heavy (~1.5s): readiness/fallback threshold calibration with real-timer probes; run via `make test-heavy`"]
#[test]
fn text_only_server_reports_no_audio_not_readiness() {
    let (case, outcome) = text_only_case();
    assert!(!case.probe_ready, "405 is not success: probe stays silent");
    assert_eq!(
        outcome,
        S2sError::NoAudio,
        "the task-value signal is NoAudio, never the probe"
    );
    assert!(!case.audio_flowed);
}

#[ignore = "heavy (~1.5s): readiness/fallback threshold calibration with real-timer probes; run via `make test-heavy`"]
#[test]
fn audio_ready_server_flows() {
    let (case, samples) = audio_ready_case();
    assert!(case.probe_ready, "the reference shape reads ready");
    assert!(case.audio_flowed);
    assert_eq!(samples, vec![2.5]);
}

#[ignore = "heavy (~1.5s): readiness/fallback threshold calibration with real-timer probes; run via `make test-heavy`"]
#[test]
fn probe_predicts_audio_on_the_fixture_set() {
    let refused = refused_case();
    let failing = failing_case();
    let (text_only, _) = text_only_case();
    let (audio_ready, _) = audio_ready_case();
    let cases = [refused, failing, text_only, audio_ready];
    let report = calibrate_threshold(
        &cases,
        |case: &Case| {
            if case.probe_ready {
                1.0
            } else {
                0.0
            }
        },
        |case: &Case| case.audio_flowed,
        0.5,
    );
    assert_eq!((report.tp, report.fp, report.tn, report.r#fn), (1, 0, 3, 0));
    assert!(
        (report.precision - 1.0).abs() < f64::EPSILON,
        "zero false-ready on the fixtures"
    );
    assert!(
        (report.recall - 1.0).abs() < f64::EPSILON,
        "zero false-not-ready on the fixtures"
    );
    assert!(report.fpr.abs() < f64::EPSILON);
    assert!(report.passes_gate(), "the strict policy holds the gate");
}
