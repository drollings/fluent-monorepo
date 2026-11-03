//! Endpoint session: one turn's confirmed prefix plus its request handle.
//!
//! The server owns its KV cache, so there is no client-side rewind. The
//! session keeps the DAC-confirmed prefix (tokens and their text) and, on
//! abort, re-requests exactly that prefix over a fresh HTTP call: the
//! server reuses its cached prefix and a wrong speculative guess costs at
//! most one re-request. History stores only confirmed words, so the
//! re-requested prefix is exactly what the user heard.

use std::sync::Arc;

use fluent_concurrency::stream::StreamAbort;
use fluent_llm::llm_queue::build_default_queue;
use fluent_llm::protocol::{
    ChatMessage, LlmConfig, LlmError as FluentError, LlmQueueConfig, LlmRequestQueue, LlmTask,
};
use fluent_llm::tokens::{estimate_tokens, TokenBudget};
use fluent_wvr::{
    Describable, FieldAccess, FieldError, WorkContext, WorkError, WorkOutput, WorkUnit,
    impl_component,
};
use internment::ArcIntern;
use orator_core::{LlmError, LlmSession, Sampler, StepOut, Token};

/// Queue-level confirmation that the worker slot is gone: the documented
/// `submit_with_abort` mapping. Matched, never reconstructed.
const QUEUE_CANCELED: &str = "queue response canceled";

/// Turn ask timeout: local inference answers in seconds, not milliseconds.
const ASK_TIMEOUT_MS: u64 = 120_000;

/// What to serve and how to address it.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Endpoint base URL (`http://127.0.0.1:<port>`, `/v1` optional).
    pub endpoint_base: String,
    /// Served model tag.
    pub model: String,
    /// Context window in tokens.
    pub ctx_tokens: usize,
    /// Prefill chunk size in tokens (cancel-latency bound).
    pub n_batch: usize,
    /// Stable turn prefix; the server reuses its cache across turns.
    pub system_prompt: String,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            endpoint_base: "http://127.0.0.1:8080".to_string(),
            model: "liquidai/lfm2.5-2.6b".to_string(),
            ctx_tokens: 8192,
            n_batch: 512,
            system_prompt: "You are a terse voice assistant. Reply in one or two short sentences, no markdown, no lists.".to_string(),
        }
    }
}

/// One turn over the supervised endpoint. Confirmed tokens grow only
/// through uncancelled prefill chunks; the request carries exactly the
/// confirmed text, packed under the context window.
pub struct HttpSession {
    queue: Arc<LlmRequestQueue>,
    config: SessionConfig,
    text_prefix: String,
    confirmed: Vec<Token>,
    answered: bool,
    abort: StreamAbort,
    last_request: Option<String>,
}

impl HttpSession {
    /// Session over the process-wide shared queue pointed at
    /// `endpoint_base`. One queue per process (two workers, bounded
    /// capacity); sessions stay per turn, so one-question ownership in
    /// the CLI keeps its cost model while concurrent turns share worker
    /// slots instead of oversubscribing a worker pair per question.
    #[must_use]
    pub fn open(endpoint_base: &str, model: &str, ctx_tokens: usize, n_batch: usize) -> Self {
        static SHARED_QUEUE: std::sync::LazyLock<Arc<LlmRequestQueue>> =
            std::sync::LazyLock::new(|| {
                build_default_queue(
                    fluent_concurrency::tokio_runtime(),
                    &LlmQueueConfig {
                        worker_count: 2,
                        queue_capacity: 64,
                    },
                )
            });
        Self::with_queue(
            Arc::clone(&SHARED_QUEUE),
            SessionConfig {
                endpoint_base: endpoint_base.to_string(),
                model: model.to_string(),
                ctx_tokens,
                n_batch,
                ..SessionConfig::default()
            },
        )
    }

    /// Session over a caller-built queue. The hermetic seam: tests serve
    /// scripted transports without a binary or a port.
    #[must_use]
    pub fn with_queue(queue: Arc<LlmRequestQueue>, config: SessionConfig) -> Self {
        Self {
            queue,
            config,
            text_prefix: String::new(),
            confirmed: Vec::new(),
            answered: false,
            abort: StreamAbort::new(),
            last_request: None,
        }
    }

    /// Install the reply epoch's abort. A fired abort short-circuits
    /// `step` before any submission and frees the worker slot mid-flight.
    pub fn set_abort(&mut self, abort: StreamAbort) {
        self.abort = abort;
    }

    /// Append turn text parallel to the next prefill. Pieces join with
    /// single spaces so the re-requested prefix reads as one turn.
    /// The stored prefix rolls under a hard char cap with a loud
    /// truncation marker (newest text protected, never silent head or
    /// tail loss); packing truncates the stored state, never just a
    /// copy, so long sessions hold a ceiling.
    pub fn push_text(&mut self, text: &str) {
        if !self.text_prefix.is_empty() {
            self.text_prefix.push(' ');
        }
        self.text_prefix.push_str(text);
        const MAX_PREFIX_CHARS: usize = 32_768;
        if self.text_prefix.chars().count() > MAX_PREFIX_CHARS {
            let total = self.text_prefix.chars().count();
            let marker = format!("\n[truncated {total} total chars; newest kept]\n");
            let keep = MAX_PREFIX_CHARS.saturating_sub(marker.chars().count()).max(512);
            let tail: String = self
                .text_prefix
                .chars()
                .rev()
                .take(keep)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            self.text_prefix = format!("{marker}{tail}");
        }
    }

    /// Tokens confirmed by uncancelled prefills. The re-request carries
    /// exactly these — an aborted chunk never lands here.
    #[must_use]
    pub fn confirmed_prefix(&self) -> &[Token] {
        &self.confirmed
    }

    /// The user text of the most recent request, after budget packing.
    #[must_use]
    pub fn last_request_text(&self) -> Option<&str> {
        self.last_request.as_deref()
    }

    /// One full turn: pack, submit abort-aware with transient retry, and
    /// return the reply as a single finished delta. A second call answers
    /// empty — the turn asked exactly once.
    pub async fn step_async(&mut self, sampler: &mut Sampler) -> Result<StepOut, LlmError> {
        if self.abort.is_cancelled() {
            return Err(LlmError::Aborted);
        }
        if self.answered {
            return Ok(StepOut {
                delta: String::new(),
                finished: true,
            });
        }
        let planned = self.plan_task(sampler);
        let abort = self.abort.clone();
        let queue = Arc::clone(&self.queue);
        let reply = common_core::retry::retry_async(
            3,
            200,
            25,
            FluentError::is_retryable,
            move || {
                let queue = Arc::clone(&queue);
                let task = LlmTask {
                    messages: planned.0.clone(),
                    config: planned.1.clone(),
                };
                let abort = abort.clone();
                async move { queue.submit_with_abort(task, abort).await }
            },
        )
        .await
        .map_err(map_fluent_error)?;
        self.answered = true;
        Ok(StepOut {
            delta: reply,
            finished: true,
        })
    }

    /// Plan the turn once: packed messages plus their transport config.
    /// Cloned per retry attempt (`LlmTask` is not `Clone`).
    fn plan_task(&mut self, sampler: &Sampler) -> (Vec<ChatMessage>, LlmConfig) {
        let user = pack_user_text(
            &self.config.system_prompt,
            &self.text_prefix,
            self.config.ctx_tokens,
        );
        self.last_request = Some(user.clone());
        let messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: self.config.system_prompt.clone(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: user,
            },
        ];
        let config = LlmConfig::new()
            .api_url(self.config.endpoint_base.clone())
            .model(self.config.model.clone())
            .timeout_ms(ASK_TIMEOUT_MS)
            .extra_body_params(serde_json::json!({
                "temperature": sampler.temperature,
                "top_p": sampler.top_p,
            }))
            .build();
        (messages, config)
    }
}

/// Fit the turn under `ctx_tokens`: the system prefix always survives;
/// the user text truncates to what remains. Newest-turn protection and
/// middle-truncation arrive with ledger-backed history.
fn pack_user_text(system: &str, user: &str, ctx_tokens: usize) -> String {
    let system_tokens = estimate_tokens(system) as usize;
    let user_tokens = estimate_tokens(user) as usize;
    if system_tokens + user_tokens <= ctx_tokens {
        return user.to_string();
    }
    let remaining = ctx_tokens.saturating_sub(system_tokens);
    TokenBudget(remaining).truncate_to_budget(user)
}

/// The queue's cancellation is the session's abort. Everything else names
/// the backend that failed.
fn map_fluent_error(error: FluentError) -> LlmError {
    match error {
        FluentError::Http(message) if message == QUEUE_CANCELED => LlmError::Aborted,
        FluentError::Http(message) | FluentError::Api(message) => LlmError::Backend(message),
        FluentError::RateLimited => LlmError::Backend("rate limited".to_string()),
        FluentError::NoResponse => LlmError::Backend("no response from model".to_string()),
    }
}

impl LlmSession for HttpSession {
    fn prefill(&mut self, toks: &[Token], abort: &StreamAbort) -> Result<(), LlmError> {
        // Chunked so cancel latency stays bounded by `n_batch`, not by the
        // turn length. A chunk confirms only when the abort is still quiet
        // after it — a fired abort keeps every token it touched out.
        let width = self.config.n_batch.max(1);
        for chunk in toks.chunks(width) {
            if abort.is_cancelled() {
                return Err(LlmError::Aborted);
            }
            self.confirmed.extend_from_slice(chunk);
        }
        Ok(())
    }

    fn step(&mut self, sampler: &mut Sampler) -> Result<StepOut, LlmError> {
        if self.abort.is_cancelled() {
            return Err(LlmError::Aborted);
        }
        // Re-entrant bridge (the `LlmClient::chat_complete` shape): inside
        // a runtime this parks via `block_in_place`; outside it the
        // process-wide fallback outlives the call, so pooled connections
        // are never stranded on a dropped per-call runtime.
        common_core::runtime::block_on(self.step_async(sampler))
    }
}

impl WorkUnit for HttpSession {
    fn name(&self) -> &str {
        "llm.session"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        if self.config.model.is_empty() {
            Ok(WorkOutput::fail("llm.session: no model"))
        } else {
            Ok(WorkOutput::ok(format!(
                "model={} confirmed={}",
                self.config.model,
                self.confirmed.len()
            )))
        }
    }
}

impl FieldAccess for HttpSession {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        match name {
            "model" => {
                self.config.model = value.to_string();
                Ok(())
            }
            "system_prompt" => {
                self.config.system_prompt = value.to_string();
                Ok(())
            }
            "ctx_tokens" => {
                self.config.ctx_tokens = value.parse().map_err(|_| {
                    FieldError::Parse("ctx_tokens must be a number".to_string())
                })?;
                Ok(())
            }
            "n_batch" => {
                self.config.n_batch = value.parse().map_err(|_| {
                    FieldError::Parse("n_batch must be a number".to_string())
                })?;
                Ok(())
            }
            _ => Err(FieldError::NotFound(name.into())),
        }
    }
    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        match name {
            "model" => Ok(self.config.model.clone()),
            "system_prompt" => Ok(self.config.system_prompt.clone()),
            "ctx_tokens" => Ok(self.config.ctx_tokens.to_string()),
            "n_batch" => Ok(self.config.n_batch.to_string()),
            _ => Err(FieldError::NotFound(name.into())),
        }
    }
    fn field_names(&self) -> &'static [&'static str] {
        &["model", "system_prompt", "ctx_tokens", "n_batch"]
    }
}

impl Describable for HttpSession {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "model": { "type": "string", "description": "Served model tag" },
                "system_prompt": { "type": "string", "description": "Stable turn prefix" },
                "ctx_tokens": { "type": "integer", "description": "Context window" },
                "n_batch": { "type": "integer", "description": "Prefill chunk size" }
            },
            "required": ["model", "system_prompt", "ctx_tokens", "n_batch"]
        })
    }
}

impl_component!(HttpSession);

#[cfg(test)]
#[path = "../tests/session.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/prefix_calibration.rs"]
mod calibration_tests;
