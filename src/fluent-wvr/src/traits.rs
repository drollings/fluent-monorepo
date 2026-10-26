use std::any::Any;
use std::sync::Arc;

use internment::ArcIntern;
use serde::{Deserialize, Serialize};

use crate::work::{WorkError, WorkOutput};

#[derive(Error, Debug, PartialEq, Eq)]
pub enum FieldError {
    #[error("field not found: {0}")]
    NotFound(String),
    #[error("field parse error: {0}")]
    Parse(String),
    #[error("constraint violation: {0}")]
    Constraint(String),
    #[error("field {0:?} is read-only on a shared Arc: {1}")]
    ReadOnly(String, String),
}
use thiserror::Error;

pub trait FieldAccess {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError>;
    fn get_field(&self, name: &str) -> Result<String, FieldError>;
    fn field_names(&self) -> &'static [&'static str];
}

pub trait Describable {
    fn describe(&self) -> serde_json::Value;
}

/// Canonical [`Describable::describe`] document from `name`/`depends`/`provides`.
/// Replaces hand-written literals; differs from pre-migration
/// `{type, description}` by design.
///
/// `purity` is [`None`] for audit-only stages (`{name, depends, provides}`).
#[must_use]
pub fn describe_work_unit(
    name: &str,
    depends: &[ArcIntern<str>],
    provides: &[ArcIntern<str>],
    purity: Option<&str>,
) -> serde_json::Value {
    let deps: Vec<&str> = depends.iter().map(|d| &**d).collect();
    let provs: Vec<&str> = provides.iter().map(|p| &**p).collect();
    let mut doc = serde_json::json!({
        "name": name,
        "depends": deps,
        "provides": provs,
    });
    if let Some(purity) = purity {
        doc["purity"] = serde_json::Value::String(purity.to_string());
    }
    doc
}

/// Compatibility surface (scaffold).
#[doc(hidden)]
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
/// Schema bounds travel as JSON numbers, never strings: a `minimum` that
/// serializes as `"1"` fails JSON Schema meta-validation. This example is
/// compiled and run by the regular gate, so a regression back to
/// string-encoded bounds fails the build.
///
/// ```rust
/// use fluent_wvr::traits::FieldSchema;
///
/// let field = FieldSchema {
///     name: "port".to_string(),
///     type_name: "u16".to_string(),
///     description: Some("TCP listen port".to_string()),
///     min: Some(1.0),
///     max: Some(65535.0),
///     min_raw: Some("1".to_string()),
///     max_raw: Some("65535".to_string()),
///     required: true,
///     format: None,
///     max_len: None,
///     sanitize: None,
///     pattern: None,
///     coerce: None,
///     parse: None,
///     axis: None,
///     visible_when: None,
///     items: None,
///     fixed: false,
/// };
/// let json = serde_json::to_value(&field).expect("FieldSchema serializes");
/// assert!(json["min"].is_number(), "min must be a JSON number, got {json}");
/// assert!(json["max"].is_number(), "max must be a JSON number, got {json}");
/// ```
pub struct FieldSchema {
    pub name: String,
    pub type_name: String,
    pub description: Option<String>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// Exact decimal spellings of the `min`/`max` attribute literals (e.g.
    /// `"18446744073709551615"`, `"-9223372036854775808"`). The `f64` bounds
    /// above cannot name integer edges exactly — they round to the nearest
    /// representable value (within one ulp, saturating to the edge on cast)
    /// — so the raws preserve the author's contract for schema consumers.
    /// `None` when no bound was given. Added additively: older schema JSON
    /// without these keys still deserializes.
    #[serde(default)]
    pub min_raw: Option<String>,
    /// Exact decimal spelling of the `max` attribute literal. See `min_raw`.
    #[serde(default)]
    pub max_raw: Option<String>,
    pub required: bool,
    /// Optional JSON Schema `format` hint (e.g. "url", "duration", "email").
    /// Informational only; not enforced by `set_field`.
    pub format: Option<String>,
    pub max_len: Option<usize>,
    pub sanitize: Option<String>,
    /// Substring pattern. The value must contain this string. Not a regex.
    pub pattern: Option<String>,
    /// Boundary-string coercion pipeline applied by `set_field` before parsing,
    /// spelled as a comma-separated `Coercion` mode list (e.g.
    /// `"trim,strip_quotes"`). `#[field(coerce = "...")]`.
    #[serde(default)]
    pub coerce: Option<String>,
    /// Optional tolerant parse mode for numeric members (`"number"`). `None`
    /// uses the strict `str::parse`. `#[field(parse = "...")]`.
    #[serde(default)]
    pub parse: Option<String>,
    /// Which axis this member measures, when the member is a gate, trigger,
    /// or threshold: `"correctness"`, `"task-value"`, or
    /// `"producer-confidence"`. `#[field(axis = "...")]`.
    ///
    /// The three axes answer different questions and must never stand in for
    /// one another: a correctness bound may not be widened by configuration, a
    /// task-value threshold judges the outcome, and a producer-confidence
    /// reading may only silence a producer — never deny a trade, size a
    /// position, or approve one on merit. `None` on presentation-only members.
    /// Added additively: older schema JSON without this key still deserializes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub axis: Option<String>,
    /// Visibility rule for a member that belongs to one arm of a tagged
    /// union: the member is editable only while the named selector has the
    /// named value (`"kind=breakout"`). `None` means always visible.
    ///
    /// The renderer hides an inactive member; the field layer refuses to write
    /// it, so visibility can never be the only guard. Added additively.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible_when: Option<String>,
    /// Maximum item count for a list member (`#[field(items = N)]`). Each item
    /// contributes its own dotted paths plus a synthetic `count`; `None` for a
    /// scalar or nested member. Added additively.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<usize>,
    /// Whether a list is fixed-arity (`[T; N]`, no `count` writer) rather than
    /// open (`Vec<T>`, a `count` grows/truncates). `false` for non-lists.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fixed: bool,
}

/// Compatibility surface (scaffold).
#[doc(hidden)]
#[allow(dead_code)]
pub trait SchemaProvider {
    fn schema(&self) -> Vec<FieldSchema>;
}

/// The core execution unit in the component system. Every `Component` implements
/// this trait. The `SupervisedBatch` supervisor and `MiddlewareChain` operate on `WorkUnit`s.
///
/// ## Purity contract
///
/// `execute` MUST be synchronous and non-blocking. Specifically:
///
/// - Do NOT call `tokio::spawn`, `tokio::time::sleep`, or
///   `Handle::block_on` inside `execute`.
/// - Do NOT perform I/O; instead emit a `WorkError` and let the
///   supervisor (`SupervisedBatch`) handle retry/backoff/timeout.
/// - Do NOT mutate any shared state external to `self` without
///   synchronization; `execute` MUST be a pure function of `(&self,
///   &WorkContext)`.
///
/// `SupervisedBatch::execute_with_timeout_and_retry` runs `execute` in an async
/// context but expects `execute` itself to return promptly. Violations
/// defeat the supervisor's timeout and retry invariants.
///
/// A blocking body that overruns its per-attempt budget keeps running
/// past it: the supervisor reports `completed`, late — never a prompt
/// `cancelled` — and a scheduled retry may overlap the still-running
/// zombie. See the timeout guarantee on the supervisor's per-attempt
/// runner for the full accounting.
///
/// See: `AGENTS.md` "Refinement contract" §3.
///
/// # Examples
///
/// ```
/// use fluent_wvr::{WorkUnit, WorkContext, WorkOutput, WorkError};
/// use internment::ArcIntern;
///
/// struct PingUnit;
///
/// impl WorkUnit for PingUnit {
///     fn name(&self) -> &str { "ping" }
///     fn depends(&self) -> &[ArcIntern<str>] { &[] }
///     fn provides(&self) -> &[ArcIntern<str>] { &[] }
///     fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
///         // Ok — fully synchronous, no I/O, no tokio calls.
///         Ok(WorkOutput::ok("pong"))
///     }
/// }
///
/// let unit = PingUnit;
/// assert_eq!(unit.name(), "ping");
/// ```
///
/// ```text
/// // BAD: this would block the executor.
/// // fn execute(&self, _: &WorkContext) -> Result<WorkOutput, WorkError> {
/// //     Handle::block_on(tokio::time::sleep(Duration::from_secs(1)));
/// //     Ok(WorkOutput::ok("done"))
/// // }
/// ```
pub trait WorkUnit: Send + Sync {
    fn name(&self) -> &str;
    fn depends(&self) -> &[ArcIntern<str>];
    fn provides(&self) -> &[ArcIntern<str>];
    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError>;

    /// Returns the default timeout in milliseconds for this unit.
    /// Override this to set a unit-specific timeout. Default: 30,000ms.
    fn default_timeout_ms(&self) -> u64 {
        30_000
    }

    /// Returns the Rust type name of this unit (e.g. "L3GraphUnit").
    /// Useful for logging, metrics aggregation, and debugging.
    fn type_name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}
use crate::work::WorkContext;

/// A fully-featured component: `FieldAccess` + `Describable` + `WorkUnit` + `Send + Sync`.
///
/// Any type that implements all four traits can implement `Component`. The derive
/// macros (`#[derive(FieldAccess, Describable)]`) plus a manual `WorkUnit` impl
/// is the 80% path. `Component` requires `as_any()`/`as_any_mut()` for runtime
/// type identification.
///
/// # Examples
///
/// ```no_run
/// use fluent_wvr::{Component, WorkUnit, WorkContext, WorkOutput, WorkError,
///     FieldAccess, Describable, FieldError, impl_component};
/// use internment::ArcIntern;
///
/// struct MyUnit { port: u16 }
///
/// impl WorkUnit for MyUnit {
///     fn name(&self) -> &str { "my_unit" }
///     fn depends(&self) -> &[ArcIntern<str>] { &[] }
///     fn provides(&self) -> &[ArcIntern<str>] { &[] }
///     fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
///         Ok(WorkOutput::ok("done"))
///     }
/// }
/// impl FieldAccess for MyUnit {
///     fn set_field(&mut self, _: &str, _: &str) -> Result<(), FieldError> { Ok(()) }
///     fn get_field(&self, _: &str) -> Result<String, FieldError> { Err(FieldError::NotFound("none".into())) }
///     fn field_names(&self) -> &'static [&'static str] { &[] }
/// }
/// impl Describable for MyUnit {
///     fn describe(&self) -> serde_json::Value { serde_json::json!({}) }
/// }
/// impl_component!(MyUnit);
///
/// // MyUnit is now a Component — can be wrapped in Arc<dyn Component>.
/// let _comp: std::sync::Arc<dyn Component> = std::sync::Arc::new(MyUnit { port: 8079 });
/// ```
pub trait Component: FieldAccess + Describable + WorkUnit + Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// Downcast a `dyn Component` to a concrete type. Returns `None` if the type doesn't match.
pub fn component_downcast_ref<T: 'static>(comp: &dyn Component) -> Option<&T> {
    comp.as_any().downcast_ref::<T>()
}

/// Mutable downcast a `dyn Component` to a concrete type. Returns `None` if the type doesn't match.
pub fn component_downcast_mut<T: 'static>(comp: &mut dyn Component) -> Option<&mut T> {
    comp.as_any_mut().downcast_mut::<T>()
}

/// Extension trait for safe mutable access through `Arc<dyn Component>`.
///
/// Use this when you can't guarantee exclusive ownership of the `Arc`.
/// If the `Arc` is shared, `try_as_any_mut` returns `None` and you can
/// decide whether to clone-and-mutate, defer, or error.
pub trait ComponentArcExt {
    fn try_as_any_mut(&mut self) -> Option<&mut dyn std::any::Any>;
}

impl ComponentArcExt for Arc<dyn Component> {
    fn try_as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Arc::get_mut(self).map(Component::as_any_mut)
    }
}
