//! Session factory tests: lifecycle, Say/Ask over stubs, interrupt
//! abort, unknown ids fail closed.

use orator_core::LlmSession as _;

use crate::voice::VoiceOwner;

fn one_f32() -> String {
    use base64::prelude::*;
    BASE64_STANDARD.encode(2.5f32.to_le_bytes())
}

#[test]
fn start_list_stop_lifecycle() {
    let owner = VoiceOwner::disabled();
    assert!(owner.list_sessions().is_empty());
    let first = owner.start_session("assistant").expect("start");
    let second = owner.start_session("dictation").expect("start");
    assert_ne!(first, second);
    let mut listed = owner.list_sessions();
    listed.sort();
    assert_eq!(listed.len(), 2);
    assert_eq!(owner.session_count(), 2);
    owner.stop_session(&first).expect("stop");
    assert_eq!(owner.list_sessions(), vec![second.clone()]);
    owner.stop_session(&second).expect("stop");
    assert!(owner.list_sessions().is_empty());
    owner.shutdown();
}

#[test]
fn unknown_session_is_named() {
    let owner = VoiceOwner::disabled();
    let error = owner.stop_session("s999").expect_err("unknown must fail");
    assert!(error.to_string().contains("s999"), "names the id: {error}");
    let error = owner.interrupt_session("s999").expect_err("unknown must fail");
    assert!(error.to_string().contains("s999"), "names the id: {error}");
    let error = owner.say_blocking("", None).expect_err("empty text must fail");
    assert!(error.to_string().contains("nothing to speak"), "names the reason: {error}");
    let error = owner.ask_blocking("", None).expect_err("empty question must fail");
    assert!(error.to_string().contains("nothing to ask"), "names the reason: {error}");
    owner.shutdown();
}

#[test]
fn interrupt_rearms_for_the_next_turn() {
    let owner = VoiceOwner::disabled();
    let id = owner.start_session("assistant").expect("start");
    owner.interrupt_session(&id).expect("interrupt");
    // The session survives the interrupt and still lists.
    assert_eq!(owner.list_sessions(), vec![id.clone()]);
    owner.stop_session(&id).expect("stop");
    owner.shutdown();
}

#[test]
fn stop_without_id_needs_exactly_one_session() {
    let owner = VoiceOwner::disabled();
    owner.stop_session("").expect_err("empty with none live must fail");
    let first = owner.start_session("assistant").expect("start");
    owner.stop_session("").expect("single live session stops");
    assert!(owner.list_sessions().is_empty());
    let _ = owner.start_session("assistant").expect("start");
    let _ = owner.start_session("assistant").expect("start");
    owner.stop_session("").expect_err("ambiguous without id must fail");
    let _ = first;
    owner.shutdown();
}

#[test]
fn say_against_scripted_audio_server() {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("POST").path("/v1/chat/completions");
        then.status(200).body(format!(
            "data: {{\"choices\":[{{\"delta\":{{\"audio_chunk\":{{\"data\":\"{}\"}}}}}}]}}\n\
             data: [DONE]\n",
            one_f32()
        ));
    });
    let owner = VoiceOwner::disabled();
    let (samples, rate) = owner
        .say_via(&server.base_url(), "hello", None)
        .expect("scripted speak runs");
    assert_eq!(samples, 1);
    assert_eq!(rate, orator_s2s::AUDIO_OUT_RATE);
    owner.shutdown();
}

#[test]
fn say_reports_text_only_as_backend() {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("POST").path("/v1/chat/completions");
        then.status(200).body(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\
             data: [DONE]\n",
        );
    });
    let owner = VoiceOwner::disabled();
    let error = owner
        .say_via(&server.base_url(), "hello", None)
        .expect_err("text-only must not voice");
    assert!(error.to_string().contains("no audio"), "task-value drives fallback: {error}");
    owner.shutdown();
}

#[test]
fn ask_against_scripted_llm_queue_fixture() {
    use std::sync::Arc;
    use fluent_llm::protocol::{LlmError as FluentError, LlmQueueConfig, LlmRequestQueue, LlmTask};
    common_core::runtime::block_on_shared(async {
        let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let queue = Arc::new(LlmRequestQueue::new(
            fluent_concurrency::tokio_runtime(),
            &LlmQueueConfig {
                worker_count: 2,
                queue_capacity: 64,
            },
            {
                let recorded = Arc::clone(&recorded);
                move |task: LlmTask| {
                    let recorded = Arc::clone(&recorded);
                    async move {
                        let text = task
                            .messages
                            .iter()
                            .map(|message| message.content.as_str())
                            .collect::<Vec<_>>()
                            .join("\n");
                        common_core::sync::lock(&recorded).push(text);
                        Ok::<String, FluentError>("scripted reply".to_string())
                    }
                }
            },
        ));
        let mut session = orator_llm::HttpSession::with_queue(
            queue,
            orator_llm::SessionConfig {
                endpoint_base: "http://127.0.0.1:9".to_string(),
                model: "session-test".to_string(),
                ctx_tokens: 8192,
                n_batch: 512,
                system_prompt: "You are terse.".to_string(),
            },
        );
        session.push_text("what time is it");
        let out = session
            .step(&mut orator_core::Sampler::default())
            .expect("scripted ask runs");
        assert_eq!(out.delta, "scripted reply");
        let requests = common_core::sync::lock(&recorded);
        assert_eq!(requests.len(), 1);
        assert!(requests[0].contains("what time is it"));
    });
}

#[test]
fn meeting_start_builds_its_pipeline() {
    let owner = VoiceOwner::disabled();
    let id = owner.start_session("meeting").expect("meeting starts");
    assert!(owner.list_sessions().contains(&id));
    owner.stop_session(&id).expect("stop");
    owner.shutdown();
}

#[test]
fn toggle_dictation_stops_only_dictation_kinds() {
    let owner = VoiceOwner::disabled();
    let assistant = owner.start_session("assistant").expect("assistant");
    let dictation = owner.start_session("dictation").expect("dictation");
    let meeting = owner.start_session("meeting").expect("meeting");
    let (active, _) = owner.toggle_dictation(Some(false));
    assert!(!active);
    let remaining = owner.list_sessions();
    assert!(remaining.contains(&assistant), "assistant survives: {remaining:?}");
    assert!(remaining.contains(&meeting), "meeting survives: {remaining:?}");
    assert!(!remaining.contains(&dictation), "dictation stops: {remaining:?}");
    owner.shutdown();
}

#[test]
fn toggle_dictation_enable_starts_dictation() {
    let owner = VoiceOwner::disabled();
    let (active, session) = owner.toggle_dictation(Some(true));
    assert!(active);
    let id = session.expect("session id");
    assert!(owner.list_sessions().contains(&id));
    owner.shutdown();
}
