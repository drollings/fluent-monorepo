//! Needle — the cheapest structured rung in Coral Router.
//!
//! Needle is a 45M-parameter / ~28MB-RAM native-engine tool-calling model
//! shipped as a prebuilt `libneedle.so` and reused **by FFI only** (no
//! reimplementation, no tensor work). It is non-generative: it answers with a
//! grammar-constrained JSON tool-call envelope, never prose.
//!
//! This module owns everything Needle-specific inside `fluent-router`:
//!
//! - [`engine`] — the C-ABI FFI seam (`needle_init`/`needle_complete`/
//!   `needle_load`/`needle_reset`), library resolution, and the production
//!   [`engine::NativeNeedleEngine`].
//! - [`envelope`] — the typed [`envelope::NeedleEnvelope`] wire shape.
//! - [`backend`] — the injectable [`backend::NeedleBackend`] trait plus the
//!   hermetic [`backend::MockNeedleBackend`].
//! - [`schema`] — the single source of truth for the route → tool-schema
//!   derivation handed to the engine (description + examples + parameters +
//!   intents, overridden by `schema_overrides`).
//! - [`retriever`] — the candidate shortlister seam (`ToolRetriever`) used
//!   when a tool catalogue overflows `candidates_per_rung`.
//!
//! It never touches the llama-server supervisor / instances / VRAM machinery
//! (`ModelEntry::is_managed` is left untouched) and is wired as a dedicated
//! top-level `needle` config block, not a `models` entry.

pub mod backend;
pub mod engine;
pub mod envelope;
pub mod queue;
pub mod retriever;
pub mod schema;
pub mod template;

use thiserror::Error;

/// Errors from the Needle FFI seam and its typed envelope.
///
/// The stage maps every variant to a clean `Skipped` (fall through to the
/// classifier) — no Needle failure ever hard-errors a request.
#[derive(Debug, Error)]
pub enum NeedleError {
    /// The `libneedle` shared library could not be loaded or a symbol is
    /// missing.
    #[error("needle library error at {path}: {detail}")]
    Library { path: String, detail: String },

    /// Tuned weights could not be read or loaded by the engine.
    #[error("needle weights error at {path}: {detail}")]
    Weights { path: String, detail: String },

    /// `needle_init` returned an error code.
    #[error("needle_init failed: {detail}")]
    Init { detail: String },

    /// `needle_complete` returned an error code.
    #[error("needle_complete failed: {detail}")]
    Complete { detail: String },

    /// The engine's output was not a parseable envelope (an engine bug — its
    /// output is grammar-guaranteed).
    #[error("malformed needle envelope: {detail}")]
    MalformedEnvelope { detail: String },

    /// The engine is not available (library missing/unloadable).
    #[error("needle engine unavailable")]
    Unavailable,

    /// The engine input contained an embedded NUL (rejected, never silently
    /// truncated).
    #[error("needle engine input rejected: {detail}")]
    Input { detail: String },

    /// The mock backend's canned response queue is exhausted.
    #[error("mock needle backend has no more responses")]
    NoMoreResponses,
}