//! Session contract: chunked prefill, abort mapping, slot release,
//! budget packing, transient retry — all against scripted transports.

use std::sync::Arc;

use fluent_concurrency::stream::StreamAbort;
use fluent_llm::protocol::{ChatMessage, LlmError as FluentError, LlmQueueConfig, LlmRequestQueue, LlmTask};
use fluent_wvr::{FieldAccess, WorkUnit};
use orator_core::{LlmError, LlmSession, Sampler, StepOut, Token};

use crate::session::{HttpSession, SessionConfig};

fn scripted_queue(
    recorded: Arc<std::sync::Mutex<Vec<String>>>,
    reply: &'static str,
) -> Arc<LlmRequestQueue> {
    Arc::new(LlmRequestQueue::new(
        fluent_concurrency::tokio_runtime(),
        &LlmQueueConfig {
            worker_count: 2,
            queue_capacity: 64,
        },
        move |task: LlmTask| {
            let recorded = Arc::clone(&recorded);
            async move {
                let text = task
                    .messages
                    .iter()
                    .map(|message: &ChatMessage| message.content.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                common_core::sync::lock(&recorded).push(text);
                Ok::<String, FluentError>(reply.to_string())
            }
        },
    ))
}

fn config() -> SessionConfig {
    SessionConfig {
        endpoint_base: "http://127.0.0.1:9".to_string(),
        model: "qwen3-test".to_string(),
        ctx_tokens: 8192,
        n_batch: 512,
        system_prompt: "You are a terse assistant.".to_string(),
    }
}

fn session_with(recorded: &Arc<std::sync::Mutex<Vec<String>>>) -> HttpSession {
    HttpSession::with_queue(scripted_queue(Arc::clone(recorded), "canned"), config())
}

fn toks(ids: &[u32]) -> Vec<Token> {
    ids.iter().map(|id| Token(*id)).collect()
}

#[test]
fn prefill_confirms_every_token() {
    common_core::runtime::block_on_shared(async {
        let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut session = session_with(&recorded);
        session
            .prefill(&toks(&[1, 2, 3, 4, 5]), &StreamAbort::new())
            .expect("prefill confirms");
        assert_eq!(session.confirmed_prefix(), &toks(&[1, 2, 3, 4, 5]));
    });
}

#[test]
fn pre_cancelled_prefill_confirms_nothing() {
    common_core::runtime::block_on_shared(async {
        let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut session = session_with(&recorded);
        let abort = StreamAbort::new();
        abort.cancel();
        let error = session
            .prefill(&toks(&[1, 2, 3]), &abort)
            .expect_err("cancelled prefill must fail");
        assert_eq!(error, LlmError::Aborted);
        assert!(session.confirmed_prefix().is_empty());
    });
}

#[test]
fn step_returns_the_turn_reply() {
    common_core::runtime::block_on_shared(async {
        let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut session = session_with(&recorded);
        session.push_text("what time is it");
        let out: StepOut = session
            .step(&mut Sampler::default())
            .expect("scripted step succeeds");
        assert_eq!(out.delta, "canned");
        assert!(out.finished);
        let requests = common_core::sync::lock(&recorded);
        assert_eq!(requests.len(), 1);
        assert!(requests[0].contains("You are a terse assistant."));
        assert!(requests[0].contains("what time is it"));
    });
}

#[test]
fn cancelled_queue_response_maps_to_aborted() {
    common_core::runtime::block_on_shared(async {
        let queue = Arc::new(LlmRequestQueue::new(
            fluent_concurrency::tokio_runtime(),
            &LlmQueueConfig {
                worker_count: 1,
                queue_capacity: 8,
            },
            |_task: LlmTask| async {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                Ok::<String, FluentError>("late".to_string())
            },
        ));
        let mut session = HttpSession::with_queue(queue, config());
        session.push_text("hello");
        let abort = StreamAbort::new();
        session.set_abort(abort.clone());
        let canceller = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            abort.cancel();
        });
        let error = session.step(&mut Sampler::default()).expect_err("cut turn must abort");
        assert_eq!(error, LlmError::Aborted);
        let _ = canceller.await;
    });
}

#[test]
fn abort_frees_the_worker_slot() {
    common_core::runtime::block_on_shared(async {
        let queue = Arc::new(LlmRequestQueue::new(
            fluent_concurrency::tokio_runtime(),
            &LlmQueueConfig {
                worker_count: 1,
                queue_capacity: 8,
            },
            |_task: LlmTask| async {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                Ok::<String, FluentError>("late".to_string())
            },
        ));
        let mut session = HttpSession::with_queue(queue, config());
        session.push_text("first");
        let abort = StreamAbort::new();
        session.set_abort(abort.clone());
        let canceller = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            abort.cancel();
        });
        session
            .step(&mut Sampler::default())
            .expect_err("first turn cut");
        let _ = canceller.await;
        // The slot is free: a fresh turn completes without waiting on the
        // cancelled request.
        session.set_abort(StreamAbort::new());
        session.push_text("second");
        let out = session
            .step(&mut Sampler::default())
            .expect("slot must be free");
        assert_eq!(out.delta, "late");
    });
}

#[test]
fn fired_session_abort_never_submits() {
    common_core::runtime::block_on_shared(async {
        let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut session = session_with(&recorded);
        session.push_text("hello");
        let abort = StreamAbort::new();
        abort.cancel();
        session.set_abort(abort);
        let error = session
            .step(&mut Sampler::default())
            .expect_err("fired abort must short-circuit");
        assert_eq!(error, LlmError::Aborted);
        assert!(common_core::sync::lock(&recorded).is_empty());
    });
}

#[test]
fn step_packs_the_turn_under_budget() {
    common_core::runtime::block_on_shared(async {
        let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut narrow = config();
        narrow.ctx_tokens = 32;
        let mut session = HttpSession::with_queue(scripted_queue(Arc::clone(&recorded), "ok"), narrow);
        session.push_text(&"word ".repeat(500));
        session.step(&mut Sampler::default()).expect("packed step");
        let requests = common_core::sync::lock(&recorded);
        assert_eq!(requests.len(), 1);
        assert!(requests[0].contains("You are a terse assistant."));
        assert!(
            requests[0].len() < 500 * 5,
            "packed request must fit the budget"
        );
        assert_eq!(
            session.last_request_text().expect("request recorded").len(),
            requests[0].len() - "You are a terse assistant.\n".len()
        );
    });
}

#[test]
fn stored_prefix_rolls_under_a_ceiling() {
    common_core::runtime::block_on_shared(async {
        let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut session = session_with(&recorded);
        session.push_text(&"w ".repeat(50_000));
        assert!(
            session.text_prefix.chars().count() <= 32_768,
            "stored prefix bounded"
        );
        assert!(
            session.text_prefix.contains("truncated"),
            "roll carries a loud marker, never silent loss"
        );
        session.push_text("newest");
        assert!(
            session.text_prefix.ends_with("newest"),
            "newest text protected"
        );
    });
}

#[test]
fn transient_failures_retry_then_succeed() {
    common_core::runtime::block_on_shared(async {
        let attempts = Arc::new(std::sync::Mutex::new(0usize));
        let observed = Arc::clone(&attempts);
        let queue = Arc::new(LlmRequestQueue::new(
            fluent_concurrency::tokio_runtime(),
            &LlmQueueConfig {
                worker_count: 1,
                queue_capacity: 8,
            },
            move |_task: LlmTask| {
                let attempts = Arc::clone(&observed);
                async move {
                    let mut guard = common_core::sync::lock(&attempts);
                    *guard += 1;
                    let attempt = *guard;
                    drop(guard);
                    if attempt == 1 {
                        Err::<String, FluentError>(FluentError::RateLimited)
                    } else {
                        Ok("recovered".to_string())
                    }
                }
            },
        ));
        let mut session = HttpSession::with_queue(queue, config());
        session.push_text("hello");
        let out = session
            .step(&mut Sampler::default())
            .expect("retry recovers");
        assert_eq!(out.delta, "recovered");
        assert_eq!(*common_core::sync::lock(&attempts), 2);
    });
}

#[test]
fn session_stays_configurable() {
    common_core::runtime::block_on_shared(async {
        let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut session = session_with(&recorded);
        session.set_field("n_batch", "128").expect("tunable by name");
        assert_eq!(session.get_field("n_batch").expect("read back"), "128");
        assert!(session.field_names().contains(&"model"));
        let output = session.execute(&fluent_wvr::WorkContext::default());
        assert!(output.expect("health executes").is_accept());
    });
}
