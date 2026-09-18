//! Per-request session-step lifecycle: begin a step, complete it from
//! whichever terminal branch the request takes, and record dispatch outcomes
//! into the session ledger + step.

use std::sync::{Arc, Mutex};

use crate::dag_session::{DependencySession, SessionRegistry, SessionStep, StepResult};
use crate::node_store::ContentNodeStore;

/// Best-effort ledger update, off the async handler (see
/// `record_ledger_request` for the swallow semantics).
pub(crate) async fn record_ledger_result(
    ledger: Option<&Arc<ContentNodeStore>>,
    node_id: Option<fluent_types::NodeId>,
    accepted: bool,
    score: Option<f64>,
    content: String,
) {
    let (Some(l), Some(node_id)) = (ledger, node_id) else {
        return;
    };
    let l = Arc::clone(l);
    tokio::task::spawn_blocking(move || {
        let _ = l.record_result(node_id, accepted, score, &content);
    })
    .await
    .ok();
}

/// Record a dispatch outcome into the session ledger + step.
///
/// Buffered dispatches carry the answer text synchronously and record it here.
/// Streaming dispatches assemble the answer as the client consumes the body,
/// so the record is deferred to a detached task that waits on the
/// [`StreamAnswer`](crate::streaming::StreamAnswer) finalizer (bounded by the
/// target's total timeout) and then records - never delaying the HTTP response.
/// `label` is the fallback content when no answer is available (escalation,
/// empty body).
pub(crate) async fn record_dispatch_outcome(
    answer_text: Option<String>,
    label: String,
    stream_answer: Option<crate::streaming::StreamAnswer>,
    ledger: Option<&Arc<ContentNodeStore>>,
    ledger_node_id: Option<fluent_types::NodeId>,
    session_step: Option<&SessionStepHandle>,
    wait_timeout_ms: u64,
    runtime: &Arc<dyn fluent_wvr::Runtime>,
) {
    let Some(finalizer) = stream_answer else {
        let answer = answer_text.unwrap_or_default();
        let content = if answer.is_empty() { label } else { answer };
        record_ledger_result(ledger, ledger_node_id, true, Some(1.0), content.clone()).await;
        if let Some(step) = session_step {
            step.complete(true, Some(1.0), content.clone(), None);
        }
        return;
    };

    let ledger = ledger.map(Arc::clone);
    let node_id = ledger_node_id;
    let step = session_step.cloned();
    runtime.spawn(Box::pin(async move {
        let content = finalizer
            .wait(std::time::Duration::from_millis(wait_timeout_ms))
            .await
            .unwrap_or_else(|| label.clone());
        record_ledger_result(ledger.as_ref(), node_id, true, Some(1.0), content.clone()).await;
        if let Some(step) = step {
            step.complete(true, Some(1.0), content.clone(), None);
        }
    }));
}

/// Per-request handle into a `DependencySession` step. Holds the session
/// `Arc` and the request's step id so the outcome can be recorded exactly once
/// from whichever terminal branch the request takes. Locking is scoped to the
/// `complete` call - never held across an await.
#[derive(Clone)]
pub(crate) struct SessionStepHandle {
    session: Arc<Mutex<DependencySession>>,
    step_id: String,
}

impl SessionStepHandle {
    pub(crate) fn complete(
        &self,
        accepted: bool,
        score: Option<f64>,
        content: String,
        error: Option<String>,
    ) {
        let mut session = match self.session.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        let _ = session.complete_step(
            &self.step_id,
            StepResult {
                content,
                accepted,
                score,
                latency_ms: 0,
                error,
            },
        );
    }

    /// The underlying session, for request-path reads that compose session
    /// state beyond step completion (frontier-ownership check, dispatch deps).
    pub(crate) fn session(&self) -> Arc<Mutex<DependencySession>> {
        Arc::clone(&self.session)
    }
}

/// Register the request as a step in the session keyed by `session_id` (if a
/// `SessionRegistry` is wired) and return a handle to complete it when the
/// outcome is known.
pub(crate) fn begin_session_step(
    sessions: Option<&Arc<SessionRegistry>>,
    session_id: &str,
    model_name: &str,
    adapter: Option<&str>,
    request_id: &str,
    request_text: &str,
) -> Option<SessionStepHandle> {
    let registry = sessions?;
    let session = registry.get_or_create(session_id);
    {
        let mut s = match session.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        s.set_model(model_name);
        if let Some(adapter) = adapter {
            s.adapter = Some(adapter.to_string());
        }
        let step_id = format!("req-{request_id}");
        if s.get_step(&step_id).is_none() {
            let _ = s.add_step(SessionStep::new(step_id.clone(), request_text));
        }
    }
    Some(SessionStepHandle {
        session,
        step_id: format!("req-{request_id}"),
    })
}
