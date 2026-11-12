//! Backend substitution seam for bounded judgments.
//!
//! `DecideBackend` takes an owned `DecideRequest` (rendered prompt plus
//! structured state, labelled options, kind) and returns a `Decision`. The
//! boxed-future shape follows the `BackendLoader::load` precedent — no
//! `async-trait` crate. The logprobs-over-chat path stays the default
//! adapter inside `LlmClient`; a native multi-question endpoint overrides
//! `decide_batch`, which otherwise fans out per-question calls in order.
//! `ScriptedDecideBackend` is the hermetic double for consumer tests.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use common_core::sync::lock;

use crate::decision::{Decision, DecisionError, DecisionKind};

/// One labelled option with an optional description for native endpoints
/// that rank option text rather than single tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One labelled option with an optional description for native endpoints
/// that rank option text rather than single tokens.
pub struct DecisionOption {
    pub label: String,
    pub description: String,
}

/// Owned judgment request (`DecideRequest`): everything a backend needs,
/// no borrows, so it crosses the `dyn` boundary and the pool without
/// per-element dispatch. See `ScriptedDecideBackend` for construction.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Owned judgment request (`DecideRequest`): everything a backend needs,
/// no borrows, so it crosses the `dyn` boundary and the pool without
/// per-element dispatch. See `ScriptedDecideBackend` for construction.
pub struct DecideRequest {
    pub prompt: String,
    pub state: String,
    pub question: String,
    pub options: Vec<DecisionOption>,
    pub kind: DecisionKind,
    pub version: u64,
    pub timeout_ms: u64,
    pub model: String,
}

/// `BatchFuture`: boxed batch outcome future for [`DecideBackend::decide_batch`].
pub type BatchFuture<'a> =
    Pin<Box<dyn Future<Output = Vec<Result<Decision, DecisionError>>> + Send + 'a>>;

/// A decision backend: one judgment per request, batch override for native
/// multi-question endpoints. `dyn` lives on the client (one vtable per
/// call), never per bulk element.
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use fluent_llm::client::LlmClient;
/// use fluent_llm::decision::{Decision, ScriptedDecideBackend};
///
/// let backend = Arc::new(ScriptedDecideBackend::canned(
///     Decision::from_distribution(
///         "A",
///         vec![("A".to_string(), 1.0)],
///         "stub".to_string(),
///         1,
///         0,
///         false,
///     )
///     .expect("canned"),
/// ));
/// let mut client = LlmClient::new("http://127.0.0.1:9", "stub");
/// client.decide_backend = Some(backend);
/// ```
pub trait DecideBackend: Send + Sync {
    /// Answer one request.
    fn decide(
        &self,
        request: DecideRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Decision, DecisionError>> + Send + '_>>;

    /// Answer many requests, outputs aligned with inputs. The default fans
    /// out sequential per-question calls; a native endpoint overrides this
    /// with one multi-question round trip.
    fn decide_batch(&self, requests: Vec<DecideRequest>) -> BatchFuture<'_> {
        Box::pin(async move {
            let mut outcomes = Vec::with_capacity(requests.len());
            for request in requests {
                outcomes.push(self.decide(request).await);
            }
            outcomes
        })
    }
}

/// Scripted hermetic double: pops scripted outcomes in order (recording
/// every request), falls back to the canned decision, else errors loudly
/// on exhaustion instead of inventing an answer.
///
/// ```rust
/// use fluent_llm::client::block_on;
/// use fluent_llm::decision::{DecideRequest, Decision, DecisionKind, ScriptedDecideBackend, DecideBackend};
///
/// let backend = ScriptedDecideBackend::canned(
///     Decision::from_distribution(
///         "A",
///         vec![("A".to_string(), 1.0)],
///         "stub".to_string(),
///         1,
///         0,
///         false,
///     )
///     .expect("canned"),
/// );
/// let request = DecideRequest {
///     prompt: "hi".to_string(),
///     state: String::new(),
///     question: "hi".to_string(),
///     options: Vec::new(),
///     kind: DecisionKind::Choice,
///     version: 1,
///     timeout_ms: 1000,
///     model: "stub".to_string(),
/// };
/// let d = block_on(backend.decide(request)).expect("scripted");
/// assert_eq!(d.label, "A");
/// ```
pub struct ScriptedDecideBackend {
    script: Mutex<VecDeque<Result<Decision, DecisionError>>>,
    seen: Mutex<Vec<DecideRequest>>,
    canned: Option<Decision>,
}

impl ScriptedDecideBackend {
    /// Pop `script` in order; error loudly once exhausted.
    #[must_use]
    pub fn new(script: Vec<Result<Decision, DecisionError>>) -> Self {
        Self {
            script: Mutex::new(script.into()),
            seen: Mutex::new(Vec::new()),
            canned: None,
        }
    }

    /// Answer every request with `decision`, recording each one.
    #[must_use]
    pub fn canned(decision: Decision) -> Self {
        Self {
            script: Mutex::new(VecDeque::new()),
            seen: Mutex::new(Vec::new()),
            canned: Some(decision),
        }
    }

    /// Prompts received so far, in order.
    #[must_use]
    pub fn prompts(&self) -> Vec<String> {
        lock(&self.seen).iter().map(|r| r.prompt.clone()).collect()
    }

    /// Full requests received so far, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<DecideRequest> {
        lock(&self.seen).clone()
    }
}

impl DecideBackend for ScriptedDecideBackend {
    fn decide(
        &self,
        request: DecideRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Decision, DecisionError>> + Send + '_>> {
        lock(&self.seen).push(request);
        let outcome = lock(&self.script)
            .pop_front()
            .or_else(|| self.canned.clone().map(Ok))
            .unwrap_or_else(|| Err(DecisionError::Unavailable("script exhausted".to_string())));
        Box::pin(async move { outcome })
    }
}