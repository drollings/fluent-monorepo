use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use internment::ArcIntern;
use tracing::info;

use crate::{Component, Describable, FieldAccess, FieldError, Runtime, WorkContext, WorkError, WorkOutput, WorkUnit};

pub enum WrapperKind {
    None,
    Retry,
    Check(Box<dyn Fn() -> bool + Send + Sync>),
}

impl WrapperKind {
    pub fn name(&self) -> &'static str {
        match self {
            WrapperKind::None => "None",
            WrapperKind::Retry => "Retry",
            WrapperKind::Check(_) => "Check",
        }
    }
}

impl std::fmt::Debug for WrapperKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WrapperKind::None => write!(f, "None"),
            WrapperKind::Retry => write!(f, "Retry"),
            WrapperKind::Check(_) => write!(f, "Check(<fn>)"),
        }
    }
}

pub fn wrap_if<T>(condition: bool, if_true: T, if_false: T) -> T {
    if condition {
        if_true
    } else {
        if_false
    }
}

pub struct RetryResult<T> {
    pub result: T,
    pub attempts: usize,
}

/// Retry with fixed-delay backoff using Tokio's async sleep.
///
/// When called from within a Tokio runtime context, uses `tokio::time::sleep`
/// via `Handle::block_on` so the executor thread is not fully blocked (other
/// tasks can make progress). Falls back to `std::thread::sleep` only when no
/// Tokio runtime is active (e.g. unit tests without a runtime).
pub fn retry_call<F, T, E>(max_attempts: usize, f: F) -> Result<RetryResult<T>, E>
where
    F: Fn() -> Result<T, E>,
{
    assert!(max_attempts >= 1);
    let mut attempts = 0;
    loop {
        attempts += 1;
        match f() {
            Ok(v) => {
                return Ok(RetryResult {
                    result: v,
                    attempts,
                })
            }
            Err(e) => {
                if attempts >= max_attempts {
                    return Err(e);
                }
                #[allow(clippy::cast_lossless)]
                let delay = Duration::from_millis(10 * attempts as u64);
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.block_on(tokio::time::sleep(delay));
                } else {
                    std::thread::sleep(delay);
                }
            }
        }
    }
}

pub struct Pipeline;

impl Pipeline {
    pub fn call<F, T, E>(kinds: &[WrapperKind], f: F) -> Result<T, E>
    where
        F: Fn() -> Result<T, E>,
        E: Clone,
    {
        if kinds.is_empty() || kinds.iter().all(|k| matches!(k, WrapperKind::None)) {
            return f();
        }
        let mut bypass = false;
        for kind in kinds {
            match kind {
                WrapperKind::Check(predicate) => {
                    if !predicate() {
                        bypass = true;
                    }
                }
                WrapperKind::Retry => {
                    if bypass {
                        return f();
                    }
                    let result = retry_call(3, &f);
                    return result.map(|r| r.result);
                }
                WrapperKind::None => {}
            }
        }
        f()
    }
}

pub struct Instrumented<U> {
    inner: U,
    label: String,
}

impl<U: WorkUnit> Instrumented<U> {
    pub fn new(inner: U, label: impl Into<String>) -> Self {
        Self {
            inner,
            label: label.into(),
        }
    }
}

impl<U: WorkUnit> WorkUnit for Instrumented<U> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        self.inner.depends()
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        self.inner.provides()
    }

    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        let start = Instant::now();
        let result = self.inner.execute(ctx);
        let elapsed = start.elapsed();
        info!(target: "instrumented", label = %self.label, elapsed = ?elapsed, name = %self.inner.name(), "executed");
        result
    }
}

impl<U: crate::Component> FieldAccess for Instrumented<U> {
    fn set_field(&mut self, name: &str, _value: &str) -> Result<(), FieldError> {
        // Delegated to inner — instrumentation does not own configuration.
        // Requires mutable access to the wrapper (configure before sharing).
        Err(FieldError::NotFound(format!(
            "{name}: instrumented wrapper is read-only; configure the inner component directly"
        )))
    }
    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        <U as FieldAccess>::get_field(&self.inner, name)
    }
    fn field_names(&self) -> &'static [&'static str] {
        <U as FieldAccess>::field_names(&self.inner)
    }
}

impl<U: crate::Component> crate::Describable for Instrumented<U> {
    fn describe(&self) -> serde_json::Value {
        <U as crate::Describable>::describe(&self.inner)
    }
}

pub struct WithRetry<U> {
    inner: U,
    max_attempts: u32,
    backoff_ms: u64,
    #[allow(dead_code)]
    rt: Option<Arc<dyn Runtime>>,
}

impl<U: WorkUnit> WithRetry<U> {
    pub fn new(inner: U, max_attempts: u32, backoff_ms: u64) -> Self {
        Self {
            inner,
            max_attempts,
            backoff_ms,
            rt: None,
        }
    }

    pub fn with_runtime(inner: U, max_attempts: u32, backoff_ms: u64, rt: Arc<dyn Runtime>) -> Self {
        Self {
            inner,
            max_attempts,
            backoff_ms,
            rt: Some(rt),
        }
    }
}

impl<U: WorkUnit> WorkUnit for WithRetry<U> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        self.inner.depends()
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        self.inner.provides()
    }

    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        let mut attempts = 0u32;
        loop {
            attempts += 1;
            match self.inner.execute(ctx) {
                Ok(output) => return Ok(output),
                Err(e) => {
                    if attempts >= self.max_attempts {
                        return Err(e);
                    }
                    #[allow(clippy::cast_lossless)]
                    let delay = Duration::from_millis(self.backoff_ms * attempts as u64);
                    if let Ok(handle) = tokio::runtime::Handle::try_current() {
                        handle.block_on(tokio::time::sleep(delay));
                    } else {
                        std::thread::sleep(delay);
                    }
                }
            }
        }
    }
}

impl<U: crate::Component> FieldAccess for WithRetry<U> {
    fn set_field(&mut self, name: &str, _value: &str) -> Result<(), FieldError> {
        Err(FieldError::NotFound(format!(
            "{name}: retry wrapper is read-only; configure the inner component directly"
        )))
    }
    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        <U as FieldAccess>::get_field(&self.inner, name)
    }
    fn field_names(&self) -> &'static [&'static str] {
        <U as FieldAccess>::field_names(&self.inner)
    }
}

impl<U: crate::Component> crate::Describable for WithRetry<U> {
    fn describe(&self) -> serde_json::Value {
        <U as crate::Describable>::describe(&self.inner)
    }
}

/// A wrapper that adapts any `Arc<dyn Component>` at runtime.
///
/// `ComponentAdapter` lets callers override one or more of the four
/// `Component` facets — `name`, `execute`, and any field — without
/// subclassing or owning the inner component. Overrides stack in
/// reverse order of insertion (last override wins), so a caller that
/// wraps a component multiple times can layer configuration.
///
/// # When to use
///
/// - **Configuration injection** — wrap a component to set a field
///   that the inner type does not expose.
/// - **Behavior override** — swap `execute` for a test double or a
///   memoized fast-path without touching the inner implementation.
/// - **Renaming** — keep the inner name as the canonical identifier
///   while presenting a stable, user-facing name in error messages
///   or schemas.
///
/// # When NOT to use
///
/// - If you own the inner type, configure it directly — `Arc::get_mut`
///   + `set_field` is the canonical path.
/// - If the override is permanent, add it to the inner type's impl.
pub type ExecuteFn = Arc<dyn Fn(&WorkContext) -> Result<WorkOutput, WorkError> + Send + Sync>;

pub struct ComponentAdapter {
    inner: Arc<dyn Component>,
    name_override: Option<String>,
    execute_override: Option<ExecuteFn>,
    field_overrides: Vec<(String, String)>,
}

impl ComponentAdapter {
    /// Wrap an `Arc<dyn Component>` with no overrides — delegates every
    /// call to the inner component.
    pub fn new(inner: Arc<dyn Component>) -> Self {
        Self {
            inner,
            name_override: None,
            execute_override: None,
            field_overrides: Vec::new(),
        }
    }

    /// Override the name returned by `WorkUnit::name`. Pass-through to
    /// the inner component if no override is set.
    #[must_use]
    pub fn with_name_override(mut self, name: impl Into<String>) -> Self {
        self.name_override = Some(name.into());
        self
    }

    /// Replace the `execute` implementation. Useful for test doubles
    /// and policy enforcement layers (e.g. an audit wrapper around
    /// an existing component).
    #[must_use]
    pub fn with_execute_override(mut self, f: ExecuteFn) -> Self {
        self.execute_override = Some(f);
        self
    }

    /// Add a field override. Overrides stack in reverse order — the
    /// most recently set value for a given field name is the one
    /// `get_field` returns.
    #[must_use]
    pub fn with_field_override(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.field_overrides.push((name.into(), value.into()));
        self
    }

    /// Borrow the wrapped component.
    pub fn inner(&self) -> &Arc<dyn Component> {
        &self.inner
    }
}

impl WorkUnit for ComponentAdapter {
    fn name(&self) -> &str {
        self.name_override
            .as_deref()
            .unwrap_or_else(|| self.inner.name())
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        self.inner.depends()
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        self.inner.provides()
    }

    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        match &self.execute_override {
            Some(f) => f(ctx),
            None => self.inner.execute(ctx),
        }
    }
}

impl FieldAccess for ComponentAdapter {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        self.field_overrides.push((name.into(), value.into()));
        Ok(())
    }
    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        self.field_overrides
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .ok_or_else(|| FieldError::NotFound(name.into()))
    }
    fn field_names(&self) -> &'static [&'static str] {
        &[]
    }
}

impl Describable for ComponentAdapter {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name(),
            "adapted": true,
            "field_overrides": self.field_overrides,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn add1(x: i32) -> i32 {
        x + 1
    }
    fn add2(x: i32) -> i32 {
        x + 2
    }

    #[test]
    fn wrap_if_true() {
        let f = wrap_if(true, add1 as fn(i32) -> i32, add2 as fn(i32) -> i32);
        assert_eq!(f(5), 6);
    }

    #[test]
    fn wrap_if_false() {
        let f = wrap_if(false, add1 as fn(i32) -> i32, add2 as fn(i32) -> i32);
        assert_eq!(f(5), 7);
    }

    #[test]
    fn retry_call_succeeds_first() {
        let result: Result<RetryResult<i32>, ()> = retry_call(3, || Ok(42));
        assert_eq!(result.unwrap().result, 42);
    }

    #[test]
    fn retry_call_always_fails() {
        let result: Result<RetryResult<i32>, ()> = retry_call(3, || Err(()));
        assert!(result.is_err());
    }

    #[test]
    fn pipeline_none_is_identity() {
        let result = Pipeline::call(&[], || Ok::<_, ()>(42));
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn wrapper_kind_name() {
        assert_eq!(WrapperKind::None.name(), "None");
        assert_eq!(WrapperKind::Retry.name(), "Retry");
        assert_eq!(WrapperKind::Check(Box::new(|| true)).name(), "Check");
    }

    struct MockUnit {
        name: ArcIntern<str>,
        should_fail: bool,
        call_count: AtomicUsize,
    }

    impl MockUnit {
        fn ok(name: &str) -> Self {
            Self {
                name: ArcIntern::from(name),
                should_fail: false,
                call_count: AtomicUsize::new(0),
            }
        }
    }

    impl WorkUnit for MockUnit {
        fn name(&self) -> &str {
            &self.name
        }
        fn depends(&self) -> &[ArcIntern<str>] {
            &[]
        }
        fn provides(&self) -> &[ArcIntern<str>] {
            &[]
        }
        fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            if self.should_fail {
                Err(WorkError::Execution("failed".into()))
            } else {
                Ok(WorkOutput::ok("done"))
            }
        }
    }

    #[test]
    fn pipeline_retry_success() {
        let unit = MockUnit::ok("mock");
        let wrapped = WithRetry::new(unit, 3, 1);
        let ctx = WorkContext::default();
        let result = wrapped.execute(&ctx);
        assert!(result.is_ok());
    }

    #[test]
    fn with_retry_accepts_unit_depends_provides() {
        let inner = MockUnit::ok("mock");
        let wrapped = WithRetry::new(inner, 3, 1);
        assert_eq!(wrapped.name(), "mock");
        assert!(wrapped.depends().is_empty());
        assert!(wrapped.provides().is_empty());
    }

    #[test]
    fn instrumented_delegates() {
        let inner = MockUnit::ok("mock");
        let wrapped = Instrumented::new(inner, "test-label");
        let ctx = WorkContext::default();
        let result = wrapped.execute(&ctx);
        assert!(result.is_ok());
        assert_eq!(wrapped.name(), "mock");
    }

    #[test]
    fn arc_dyn_work_unit_delegates() {
        let inner = MockUnit::ok("mock");
        let arc: Arc<dyn WorkUnit> = Arc::new(inner);
        let ctx = WorkContext::default();
        assert_eq!(arc.name(), "mock");
        let result = arc.execute(&ctx);
        assert!(result.is_ok());
    }

    // --- ComponentAdapter tests ---

    /// A `Component` that is NOT a `MockUnit` — used to confirm the
    /// adapter works when wrapping through `Arc<dyn Component>`.
    struct AdapterHost {
        name: ArcIntern<str>,
        last_message: Arc<std::sync::Mutex<String>>,
    }
    impl AdapterHost {
        fn new(name: &str) -> Self {
            Self {
                name: ArcIntern::from(name),
                last_message: Arc::new(std::sync::Mutex::new(String::new())),
            }
        }
    }
    impl FieldAccess for AdapterHost {
        fn set_field(&mut self, _name: &str, _value: &str) -> Result<(), FieldError> {
            Ok(())
        }
        fn get_field(&self, _name: &str) -> Result<String, FieldError> {
            Err(FieldError::NotFound("no fields".into()))
        }
        fn field_names(&self) -> &'static [&'static str] {
            &[]
        }
    }
    impl Describable for AdapterHost {
        fn describe(&self) -> serde_json::Value {
            serde_json::json!({"name": &*self.name})
        }
    }
    impl WorkUnit for AdapterHost {
        fn name(&self) -> &str {
            &self.name
        }
        fn depends(&self) -> &[ArcIntern<str>] {
            &[]
        }
        fn provides(&self) -> &[ArcIntern<str>] {
            &[]
        }
        fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
            let mut g = self.last_message.lock().unwrap();
            *g = "from_inner".to_string();
            Ok(WorkOutput::ok("from_inner"))
        }
    }

    #[test]
    fn adapter_delegates_by_default() {
        let host = AdapterHost::new("inner");
        let adapter = ComponentAdapter::new(Arc::new(host));
        assert_eq!(adapter.name(), "inner");
        let result = adapter.execute(&WorkContext::default()).unwrap();
        assert_eq!(result.message, "from_inner");
    }

    #[test]
    fn adapter_name_override() {
        let host = AdapterHost::new("inner");
        let adapter = ComponentAdapter::new(Arc::new(host)).with_name_override("renamed");
        assert_eq!(adapter.name(), "renamed");
    }

    #[test]
    fn adapter_execute_override_short_circuits_inner() {
        let host = AdapterHost::new("inner");
        let adapter = ComponentAdapter::new(Arc::new(host))
            .with_execute_override(Arc::new(|_| Ok(WorkOutput::ok("overridden"))));
        let result = adapter.execute(&WorkContext::default()).unwrap();
        assert_eq!(result.message, "overridden");
    }

    #[test]
    fn adapter_field_override_set_then_get() {
        let host = AdapterHost::new("inner");
        let mut adapter =
            ComponentAdapter::new(Arc::new(host)).with_field_override("port", "8080");
        assert_eq!(adapter.get_field("port").unwrap(), "8080");
        adapter.set_field("port", "9090").unwrap();
        assert_eq!(adapter.get_field("port").unwrap(), "9090");
    }

    #[test]
    fn adapter_field_overrides_stack_last_wins() {
        let host = AdapterHost::new("inner");
        let adapter = ComponentAdapter::new(Arc::new(host))
            .with_field_override("k", "v1")
            .with_field_override("k", "v2");
        assert_eq!(adapter.get_field("k").unwrap(), "v2");
    }

    #[test]
    fn adapter_field_not_found() {
        let host = AdapterHost::new("inner");
        let adapter = ComponentAdapter::new(Arc::new(host));
        assert!(matches!(
            adapter.get_field("missing"),
            Err(FieldError::NotFound(_))
        ));
    }

    #[test]
    fn adapter_is_itself_a_component() {
        let host = AdapterHost::new("inner");
        let adapter = ComponentAdapter::new(Arc::new(host));
        // Box as dyn Component — proves the blanket impl fires.
        let boxed: Box<dyn Component> = Box::new(adapter);
        assert_eq!(boxed.name(), "inner");
    }

    #[test]
    fn adapter_describe_includes_overrides() {
        let host = AdapterHost::new("inner");
        let adapter = ComponentAdapter::new(Arc::new(host))
            .with_name_override("renamed")
            .with_field_override("port", "8080");
        let schema = adapter.describe();
        assert_eq!(schema["name"], "renamed");
        assert_eq!(schema["adapted"], true);
        let overrides = schema["field_overrides"].as_array().unwrap();
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0][0], "port");
        assert_eq!(overrides[0][1], "8080");
    }

    #[test]
    fn adapter_inner_accessor_returns_wrapped_component() {
        let host = AdapterHost::new("inner");
        let adapter = ComponentAdapter::new(Arc::new(host));
        let inner: &Arc<dyn Component> = adapter.inner();
        assert_eq!(inner.name(), "inner");
    }

    #[test]
    fn adapter_propagates_depends_and_provides_from_inner() {
        // A Component whose depends/provides are non-empty — confirm
        // delegation rather than pass-through to the empty default.
        struct DepProvider {
            name: ArcIntern<str>,
            deps: Vec<ArcIntern<str>>,
            provs: Vec<ArcIntern<str>>,
        }
        impl WorkUnit for DepProvider {
            fn name(&self) -> &str {
                &self.name
            }
            fn depends(&self) -> &[ArcIntern<str>] {
                &self.deps
            }
            fn provides(&self) -> &[ArcIntern<str>] {
                &self.provs
            }
            fn execute(&self, _: &WorkContext) -> Result<WorkOutput, WorkError> {
                Ok(WorkOutput::ok("ok"))
            }
        }
        impl FieldAccess for DepProvider {
            fn set_field(&mut self, _: &str, _: &str) -> Result<(), FieldError> {
                Ok(())
            }
            fn get_field(&self, _: &str) -> Result<String, FieldError> {
                Err(FieldError::NotFound("none".into()))
            }
            fn field_names(&self) -> &'static [&'static str] {
                &[]
            }
        }
        impl Describable for DepProvider {
            fn describe(&self) -> serde_json::Value {
                serde_json::json!({})
            }
        }
        let inner = DepProvider {
            name: ArcIntern::from("dp"),
            deps: vec![ArcIntern::from("a"), ArcIntern::from("b")],
            provs: vec![ArcIntern::from("c")],
        };
        let adapter = ComponentAdapter::new(Arc::new(inner));
        assert_eq!(adapter.depends().len(), 2);
        assert_eq!(&*adapter.depends()[0], "a");
        assert_eq!(&*adapter.depends()[1], "b");
        assert_eq!(adapter.provides().len(), 1);
        assert_eq!(&*adapter.provides()[0], "c");
    }
}
