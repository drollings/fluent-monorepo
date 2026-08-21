//! `NeedleBackend` — the injectable trait for Needle completions.
//!
//! Mirrors the `ChatBackend` trait + mock-injection pattern from
//! `src/llm/src/client.rs` / `src/router/src/stages/classifier.rs`: production
//! uses [`crate::needle::engine::NativeNeedleEngine`] over the FFI; tests and
//! `--mock` mode use [`MockNeedleBackend`]. The `NeedlePreFilter` stage
//! receives `Arc<dyn NeedleBackend>` and never cares which impl it holds.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use common_core::sync::lock;

use crate::needle::envelope::NeedleEnvelope;
use crate::needle::NeedleError;

/// The injectable Needle completion backend.
///
/// `complete` returns a typed envelope or an error; `is_available` lets the
/// stage skip cleanly when the engine is unavailable (never error the
/// request). `reset` clears the engine's session state.
pub trait NeedleBackend: Send + Sync {
    /// Run one grammar-constrained completion. `tools_json` is the tool-schema
    /// array handed to the engine; `max_new_tokens` bounds generation.
    fn complete(
        &self,
        text: &str,
        tools_json: &str,
        max_new_tokens: i32,
    ) -> Result<NeedleEnvelope, NeedleError>;

    /// Whether this backend is usable right now. A `false` backend makes the
    /// stage emit `Skipped` (fall through to the classifier).
    fn is_available(&self) -> bool;

    /// Reset the engine's session state.
    fn reset(&self);
}

/// A `NeedleBackend` driven by a canned response queue — the hermetic test
/// double (mirrors `StubChatBackend` / `CountingBackend`).
pub struct MockNeedleBackend {
    responses: Mutex<VecDeque<Result<NeedleEnvelope, NeedleError>>>,
    available: AtomicBool,
    calls: AtomicUsize,
}

impl MockNeedleBackend {
    /// A backend that pops canned responses in order; an exhausted queue is an
    /// error (`NoMoreResponses`).
    pub fn new(responses: Vec<Result<NeedleEnvelope, NeedleError>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            available: AtomicBool::new(true),
            calls: AtomicUsize::new(0),
        }
    }

    /// A backend that always returns `envelope`.
    pub fn always(envelope: NeedleEnvelope) -> Self {
        Self::new(vec![Ok(envelope)])
    }

    /// A backend that fails every completion (e.g. engine unavailable).
    pub fn failing() -> Self {
        Self::new(vec![Err(NeedleError::Unavailable)])
    }

    /// Mark the backend available/unavailable. Unavailable backends report
    /// `false` from `is_available` (the stage then skips without calling
    /// `complete`).
    pub fn set_available(&self, available: bool) {
        self.available.store(available, Ordering::SeqCst);
    }

    /// Number of `complete` calls made.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Default for MockNeedleBackend {
    fn default() -> Self {
        Self::new(vec![])
    }
}

impl NeedleBackend for MockNeedleBackend {
    fn complete(
        &self,
        _text: &str,
        _tools_json: &str,
        _max_new_tokens: i32,
    ) -> Result<NeedleEnvelope, NeedleError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut queue = lock(&self.responses);
        queue
            .pop_front()
            .ok_or(NeedleError::NoMoreResponses)
            .and_then(|r| r)
    }

    fn is_available(&self) -> bool {
        self.available.load(Ordering::SeqCst)
    }

    fn reset(&self) {
        // Mock has no engine state to reset.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::needle::envelope::{NeedleEnvelopeType, NeedleFunctionCall};
    use serde_json::json;

    fn call_envelope(tool: &str) -> NeedleEnvelope {
        NeedleEnvelope {
            r#type: NeedleEnvelopeType::Call,
            success: None,
            error: None,
            error_code: None,
            function_calls: vec![NeedleFunctionCall {
                name: tool.to_string(),
                arguments: json!({}),
            }],
            reasoning: None,
            confidence: Some(0.95),
            results: None,
        }
    }

    #[test]
    fn mock_pops_canned_responses_in_order() {
        let backend = MockNeedleBackend::new(vec![
            Ok(call_envelope("route_a")),
            Ok(call_envelope("route_b")),
        ]);
        assert!(backend.is_available());
        let a = backend.complete("q", "[]", 32).expect("first");
        let b = backend.complete("q", "[]", 32).expect("second");
        assert_eq!(a.single_tool(), Some("route_a"));
        assert_eq!(b.single_tool(), Some("route_b"));
        assert_eq!(backend.calls(), 2);
    }

    #[test]
    fn mock_exhausted_queue_is_error() {
        let backend = MockNeedleBackend::new(vec![]);
        let err = backend.complete("q", "[]", 32).expect_err("empty");
        assert!(matches!(err, NeedleError::NoMoreResponses));
    }

    #[test]
    fn mock_failing_backend_errors() {
        let backend = MockNeedleBackend::failing();
        let err = backend.complete("q", "[]", 32).expect_err("failing");
        assert!(matches!(err, NeedleError::Unavailable));
    }

    #[test]
    fn mock_availability_is_configurable() {
        let backend = MockNeedleBackend::always(call_envelope("r"));
        assert!(backend.is_available());
        backend.set_available(false);
        assert!(!backend.is_available());
    }

    #[test]
    fn mock_available_flag_does_not_gate_complete() {
        // The trait reports availability; the stage decides whether to call
        // `complete`. A mock may still serve completions when marked
        // unavailable (mirrors the real engine's `is_available` semantics).
        let backend = MockNeedleBackend::always(call_envelope("r"));
        backend.set_available(false);
        assert!(backend.complete("q", "[]", 32).is_ok());
    }
}