use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::capability::CapabilitySet;
use crate::metadata::MetadataValue;
use crate::runtime::{NoopRuntime, Runtime};
use crate::store::OutputStore;
use crate::traits::WorkUnit;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum WorkError {
    #[error("execution failed: {0}")]
    Execution(String),
    #[error("dependency not satisfied: {0}")]
    Dependency(String),
    #[error("timeout after {duration_ms}ms ({unit})")]
    Timeout { duration_ms: u64, unit: String },
}

impl WorkError {
    /// Bridge any displayable error into [`WorkError::Execution`] verbatim.
    pub fn execution_display(e: impl core::fmt::Display) -> Self {
        Self::Execution(e.to_string())
    }

    /// Context-prefixed [`WorkError::Execution`]: `"{context}: {e}"`.
    pub fn execution_context(context: &str, e: impl core::fmt::Display) -> Self {
        Self::Execution(format!("{context}: {e}"))
    }

    /// Whether the error represents a transient condition worth retrying.
    ///
    /// `Execution` is a *permanent* failure — the unit ran and failed, so a
    /// retry would burn backoff budget without a recovery signal. `Dependency`
    /// (a prerequisite not yet satisfied) and `Timeout` (attempt budget
    /// exhausted, e.g. by transient overload) are retryable.
    ///
    /// This is the canonical predicate for retry loops that drive
    /// `WorkUnit::execute` (the `SupervisedBatch` supervisor and any future caller);
    /// `common_core::retry::retry_async` short-circuits on `false`.
    pub fn is_retryable(&self) -> bool {
        matches!(self, WorkError::Dependency(_) | WorkError::Timeout { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkOutput {
    pub success: bool,
    pub message: String,
    pub data: serde_json::Value,
}

impl WorkOutput {
    /// Task-value acceptance predicate: the unit ran cleanly **and** its
    /// verdict is positive.
    ///
    /// This is orthogonal to `Result::is_ok` (producer-confidence — "did it
    /// run"). A confident `Ok(WorkOutput::fail(..))` is maximally confident
    /// and task-value-negative: `is_ok()` is true, `is_accept()` is false.
    /// Callers that need "was it accepted" must use this predicate; callers
    /// that need "did it run" keep using `Result::is_ok`. Never derive one
    /// from the other implicitly.
    pub fn is_accept(&self) -> bool {
        self.success
    }

    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: message.into(),
            data: serde_json::Value::Null,
        }
    }
    pub fn ok_with_data(message: impl Into<String>, data: serde_json::Value) -> Self {
        Self {
            success: true,
            message: message.into(),
            data,
        }
    }
    pub fn fail(message: impl Into<String>) -> Self {
        Self {
            success: false,
            message: message.into(),
            data: serde_json::Value::Null,
        }
    }

    /// Create a successful output with typed data serialized to JSON.
    /// Returns `Err(WorkError::Execution(...))` on serialization failure.
    /// This is the canonical way to pass structured data between components.
    pub fn typed<T: serde::Serialize>(
        message: impl Into<String>,
        data: &T,
    ) -> Result<Self, WorkError> {
        let data = serde_json::to_value(data)
            .map_err(|e| WorkError::Execution(format!("serialize typed data: {e}")))?;
        Ok(Self {
            success: true,
            message: message.into(),
            data,
        })
    }

    /// Same as `typed` but never panics: on serialization failure the error
    /// is stored as a string in `data` instead. Prefer `typed` (which
    /// reports the failure as `Err`) when the caller can handle it.
    pub fn typed_lossy<T: serde::Serialize>(message: impl Into<String>, data: &T) -> Self {
        let data =
            serde_json::to_value(data).unwrap_or_else(|e| serde_json::Value::String(e.to_string()));
        Self {
            success: true,
            message: message.into(),
            data,
        }
    }

    /// Deserialize the `data` field back to a typed value, borrowing it.
    ///
    /// Identical errors to [`WorkOutput::data_as`], without the intermediate
    /// `Value` clone: deserialization reads directly from the stored value.
    pub fn data_ref<'a, T: serde::Deserialize<'a>>(&'a self) -> Result<T, WorkError> {
        T::deserialize(&self.data)
            .map_err(|e| WorkError::Execution(format!("deserialize data: {e}")))
    }

    /// Deserialize the `data` field back to a typed value (borrowed).
    pub fn data_as<T: for<'de> serde::Deserialize<'de>>(&self) -> Result<T, WorkError> {
        self.data_ref()
    }

    /// Deserialize the `data` field back to a typed value, consuming `self`.
    /// Avoids the clone that `data_as` requires.
    pub fn data_take<T: for<'de> Deserialize<'de>>(self) -> Result<T, WorkError> {
        serde_json::from_value(self.data)
            .map_err(|e| WorkError::Execution(format!("deserialize data: {e}")))
    }
}

impl std::fmt::Display for WorkOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.success {
            write!(f, "OK: {}", self.message)
        } else {
            write!(f, "FAIL: {}", self.message)
        }
    }
}

/// Execution context passed to `WorkUnit::execute`. Carries configuration
/// (dry-run, retries, timeout), typed metadata, a runtime, and a capability set.
///
/// # Data channels — the decision rule
///
/// A `WorkContext` exposes four data channels with different typing fidelity.
/// The rule for choosing between them is:
///
/// 1. **`outputs` (the typed store, [`OutputStore`])** — the *primary* channel
///    for in-process handoff between stages / work units. Values are written
///    with [`WorkContext::set::<T>`] and read with [`WorkContext::get::<T>`],
///    so the type is pinned at the call site and no serialize/deserialize
///    round-trip happens between producers and consumers in the same process.
/// 2. **`WorkOutput.data` (`serde_json::Value`)** — reserved exclusively for
///    payloads that genuinely cross a serialization boundary: WASM units,
///    network dispatch, and a `WorkUnit`'s output consumed by a generic
///    orchestrator. Do not use it for same-process handoffs.
/// 3. **`metadata` (`HashMap<String, MetadataValue>`)** — only for genuinely
///    dynamic / stringly-typed fields such as debug annotations, where the
///    value's type is not known at the call site.
/// 4. **`structured` (`HashMap<String, serde_json::Value>`)** — the JSON
///    handoff channel for payloads that are natively `serde_json::Value`-shaped
///    (e.g. a serialized `RouterRequest`). Prefer `outputs` whenever the type
///    is known at compile time.
///
/// # Examples
///
/// ```no_run
/// use fluent_wvr::{WorkContext, CapabilitySet, NoopRuntime};
/// use std::sync::Arc;
///
/// let mut ctx = WorkContext {
///     dry_run: true,
///     max_retries: 3,
///     timeout_ms: 10_000,
///     metadata: Default::default(),
///     structured: Default::default(),
///     outputs: Default::default(),
///     rt: Arc::new(NoopRuntime),
///     caps: CapabilitySet::new(),
/// };
/// ctx.set("attempt", 2_i32);
/// assert_eq!(ctx.get::<i32>("attempt"), Some(&2));
/// ```
#[derive(Clone)]
/// Execution context passed to `WorkUnit::execute`. Carries configuration
/// (dry-run, retries, timeout), typed metadata, a runtime, and a capability set.
/// 
/// # Data channels — the decision rule
/// 
/// A `WorkContext` exposes four data channels with different typing fidelity.
/// The rule for choosing between them is:
/// 
/// 1. **`outputs` (the typed store, [`OutputStore`])** — the *primary* channel
/// for in-process handoff between stages / work units. Values are written
/// with [`WorkContext::set::<T>`] and read with [`WorkContext::get::<T>`],
/// so the type is pinned at the call site and no serialize/deserialize
/// round-trip happens between producers and consumers in the same process.
/// 2. **`WorkOutput.data` (`serde_json::Value`)** — reserved exclusively for
/// payloads that genuinely cross a serialization boundary: WASM units,
/// network dispatch, and a `WorkUnit`'s output consumed by a generic
/// orchestrator. Do not use it for same-process handoffs.
/// 3. **`metadata` (`HashMap<String, MetadataValue>`)** — only for genuinely
/// dynamic / stringly-typed fields such as debug annotations, where the
/// value's type is not known at the call site.
/// 4. **`structured` (`HashMap<String, serde_json::Value>`)** — the JSON
/// handoff channel for payloads that are natively `serde_json::Value`-shaped
/// (e.g. a serialized `RouterRequest`). Prefer `outputs` whenever the type
/// is known at compile time.
/// 
/// # Examples
/// 
/// ```no_run
/// use fluent_wvr::{WorkContext, CapabilitySet, NoopRuntime};
/// use std::sync::Arc;
/// 
/// let mut ctx = WorkContext {
/// dry_run: true,
/// max_retries: 3,
/// timeout_ms: 10_000,
/// metadata: Default::default(),
/// structured: Default::default(),
/// outputs: Default::default(),
/// rt: Arc::new(NoopRuntime),
/// caps: CapabilitySet::new(),
/// };
/// ctx.set("attempt", 2_i32);
/// assert_eq!(ctx.get::<i32>("attempt"), Some(&2));
/// ```
pub struct WorkContext {
    pub dry_run: bool,
    pub max_retries: u32,
    pub timeout_ms: u64,
    pub metadata: HashMap<String, MetadataValue>,
    /// Typed/structured metadata channel. Unlike `metadata` (scalar
    /// `MetadataValue`), this holds arbitrary `serde_json::Value` payloads —
    /// the canonical home for structured handoffs such as the serialized
    /// `RouterRequest` (`"request"`), bound chart entities (`"entities"`),
    /// and per-stage outputs (`"stage.{id}"`). Crosses crate boundaries as
    /// `serde_json::Value` only (never a domain type).
    ///
    /// Prefer the typed store (`outputs`) whenever the payload type is known
    /// at compile time — see the decision rule above.
    pub structured: HashMap<String, serde_json::Value>,
    /// Typed in-process handoff accumulator (the primary inter-unit channel).
    ///
    /// Write with [`WorkContext::set::<T>`] / [`OutputStore::set`], read with
    /// [`WorkContext::get::<T>`] / [`OutputStore::get`]. See the struct-level
    /// decision rule for when this is preferred over `data`/`metadata`.
    pub outputs: OutputStore,
    pub rt: Arc<dyn Runtime>,
    pub caps: CapabilitySet,
}

impl std::fmt::Debug for WorkContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkContext")
            .field("dry_run", &self.dry_run)
            .field("max_retries", &self.max_retries)
            .field("timeout_ms", &self.timeout_ms)
            .field("metadata", &self.metadata)
            .field("structured", &self.structured)
            .field("outputs", &self.outputs)
            .field("rt", &"<dyn Runtime>")
            .field("caps", &self.caps)
            .finish()
    }
}

impl Default for WorkContext {
    fn default() -> Self {
        Self {
            dry_run: false,
            max_retries: 0,
            timeout_ms: 30_000,
            metadata: HashMap::new(),
            structured: HashMap::new(),
            outputs: OutputStore::default(),
            rt: Arc::new(NoopRuntime),
            caps: CapabilitySet::new(),
        }
    }
}

impl WorkContext {
    /// Construct a `WorkContext` for a specific unit with a given capability set.
    /// Uses the unit's `default_timeout_ms()` and a default runtime.
    pub fn for_unit(unit: &dyn WorkUnit, caps: CapabilitySet) -> Self {
        Self {
            dry_run: false,
            max_retries: 0,
            timeout_ms: unit.default_timeout_ms(),
            metadata: HashMap::new(),
            structured: HashMap::new(),
            outputs: OutputStore::default(),
            rt: Arc::new(NoopRuntime),
            caps,
        }
    }

    /// Store a typed value for in-process handoff under `key`.
    ///
    /// The canonical writer for the typed store — equivalent to
    /// `self.outputs.set::<T>(key, value)`. No serialization is performed.
    pub fn set<T: Send + Sync + 'static>(&mut self, key: impl Into<String>, value: T) {
        self.outputs.set(key, value);
    }

    /// Read a typed value previously written with [`WorkContext::set`].
    ///
    /// Returns `None` if `key` is absent or holds a different type. This is
    /// the canonical reader for the typed store; no deserialization is
    /// performed.
    pub fn get<T: 'static>(&self, key: &str) -> Option<&T> {
        self.outputs.get::<T>(key)
    }

    /// Borrow a structured value by key, without cloning or deserializing.
    ///
    /// Returns `None` when the key is absent. This is the zero-copy reader
    /// for the channel; [`WorkContext::structured`] (owned, deserializing)
    /// delegates to it.
    pub fn structured_ref(&self, key: &str) -> Option<&serde_json::Value> {
        self.structured.get(key)
    }

    /// Read a structured value by key, deserializing it to a typed value.
    /// The single place that does `serde_json::from_value` for the channel.
    pub fn structured<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<T, WorkError> {
        let value = self.structured_ref(key).ok_or_else(|| {
            WorkError::Execution(format!("structured metadata key not found: {key}"))
        })?;
        T::deserialize(value)
            .map_err(|e| WorkError::Execution(format!("structured metadata key {key:?}: {e}")))
    }

    /// Write a structured value by key, serializing a typed value.
    /// The single place that does `serde_json::to_value` for the channel.
    /// Returns `false` (leaving any previous value intact) when the value
    /// has no JSON representation instead of silently skipping the write.
    pub fn try_set_structured<T: serde::Serialize>(&mut self, key: &str, value: &T) -> bool {
        match serde_json::to_value(value) {
            Ok(v) => {
                self.structured.insert(key.to_string(), v);
                true
            }
            Err(_) => false,
        }
    }

    /// Write a structured value by key, serializing a typed value.
    /// The single place that does `serde_json::to_value` for the channel.
    /// A serialization failure is silently skipped (matches the typed-setter
    /// convention of `StageMetadata`).
    pub fn set_structured<T: serde::Serialize>(&mut self, key: &str, value: &T) {
        let _ = self.try_set_structured(key, value);
    }

    /// Returns a new `WorkContext` with `rt` and `caps` cloned from the
    /// SupervisedBatch's defaults, with `max_retries`/`timeout_ms`/etc
    /// overridden by the supplied closure.
    ///
    /// For batched registration of identical contexts, construct one `WorkContext`
    /// and reuse it with `register_with_context` directly.
    pub fn for_unit_in_batch(
        batch_rt: &Arc<dyn Runtime>,
        batch_caps: &CapabilitySet,
        mutate: impl FnOnce(&mut WorkContext),
    ) -> Self {
        let mut ctx = WorkContext {
            rt: Arc::clone(batch_rt),
            caps: batch_caps.clone(),
            ..WorkContext::default()
        };
        mutate(&mut ctx);
        ctx
    }
}