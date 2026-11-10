//! Adversarial prefix check: abort at every token position, then prove
//! the next request carries exactly the confirmed prefix — nothing more.
//!
//! Only prefix equality is asserted (the task-value outcome). Server
//! cache behavior is never measured here.

use std::sync::Arc;

use fluent_concurrency::stream::StreamAbort;
use fluent_llm::protocol::{
    ChatMessage, LlmError as FluentError, LlmQueueConfig, LlmRequestQueue, LlmTask,
};
use orator::{LlmSession, Sampler, Token};

use crate::llm::session::{HttpSession, SessionConfig};

fn word(index: usize) -> String {
    format!("w{index}")
}

#[test]
fn abort_at_every_position_leaks_nothing() {
    common_core::runtime::block_on_shared(async {
        let count = 8usize;
        let tokens: Vec<Token> = (0..count as u32).map(Token).collect();
        for cut in 0..=count {
            let recorded = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
            let tap = Arc::clone(&recorded);
            let queue = Arc::new(LlmRequestQueue::new(
                fluent_concurrency::tokio_runtime(),
                &LlmQueueConfig {
                    worker_count: 1,
                    queue_capacity: 8,
                },
                move |task: LlmTask| {
                    let recorded = Arc::clone(&tap);
                    async move {
                        let user = task
                            .messages
                            .iter()
                            .find(|message: &&ChatMessage| message.role == "user")
                            .map(|message| message.content.clone())
                            .unwrap_or_default();
                        common_core::sync::lock(&recorded).push(user);
                        Ok::<String, FluentError>("done".to_string())
                    }
                },
            ));
            let mut session = HttpSession::with_queue(
                queue,
                SessionConfig {
                    endpoint_base: "http://127.0.0.1:9".to_string(),
                    model: "qwen3-test".to_string(),
                    ctx_tokens: 8192,
                    n_batch: 1,
                    system_prompt: "sys".to_string(),
                },
            );
            // Confirm the first `cut` tokens one at a time, with text.
            let live = StreamAbort::new();
            for (index, token) in tokens[..cut].iter().enumerate() {
                session.push_text(&word(index));
                session
                    .prefill(std::slice::from_ref(token), &live)
                    .expect("confirmed prefix must accept");
            }
            // Cut here: the remainder must abort and confirm nothing.
            let abort = StreamAbort::new();
            abort.cancel();
            if cut < count {
                let error = session
                    .prefill(&tokens[cut..], &abort)
                    .expect_err("post-cut prefill must abort");
                assert_eq!(error, orator::LlmError::Aborted);
            }
            assert_eq!(
                session.confirmed_prefix(),
                &tokens[..cut],
                "cut at {cut}: confirmed prefix must equal exactly the pre-cut tokens"
            );
            // The re-requested turn carries exactly the confirmed prefix.
            session.set_abort(StreamAbort::new());
            session
                .step(&mut Sampler::default())
                .expect("re-request must succeed");
            let requests = common_core::sync::lock(&recorded);
            assert_eq!(requests.len(), 1, "cut at {cut}");
            let expected = (0..cut).map(word).collect::<Vec<_>>().join(" ");
            assert_eq!(requests[0], expected, "cut at {cut}: prefix equality");
        }
    });
}
