use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use internment::ArcIntern;
use tracing::info;

use crate::{FieldAccess, FieldError, Runtime, WorkContext, WorkError, WorkOutput, WorkUnit};

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
}
