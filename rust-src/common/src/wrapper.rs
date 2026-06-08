use std::time::Duration;

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
    if condition { if_true } else { if_false }
}

pub struct RetryResult<T> {
    pub result: T,
    pub attempts: usize,
}

pub fn retry_call<F, T, E>(
    max_attempts: usize,
    f: F,
) -> Result<RetryResult<T>, E>
where
    F: Fn() -> Result<T, E>,
{
    assert!(max_attempts >= 1);
    let mut attempts = 0;
    loop {
        attempts += 1;
        match f() {
            Ok(v) => return Ok(RetryResult { result: v, attempts }),
            Err(e) => {
                if attempts >= max_attempts {
                    return Err(e);
                }
                #[allow(clippy::cast_lossless)]
                std::thread::sleep(Duration::from_millis(10 * attempts as u64));
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
                WrapperKind::None => {},
            }
        }
        f()
    }
}

use std::sync::Arc;
use std::time::Instant;

use internment::ArcIntern;
use tracing::info;

use crate::traits::{WorkContext, WorkError, WorkOutput, WorkUnit};

impl WorkUnit for Arc<dyn WorkUnit> {
    fn name(&self) -> &str {
        (**self).name()
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        (**self).depends()
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        (**self).provides()
    }

    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        (**self).execute(ctx)
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

pub struct WithRetry<U> {
    inner: U,
    max_attempts: u32,
    backoff_ms: u64,
}

impl<U: WorkUnit> WithRetry<U> {
    pub fn new(inner: U, max_attempts: u32, backoff_ms: u64) -> Self {
        Self {
            inner,
            max_attempts,
            backoff_ms,
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
                    std::thread::sleep(std::time::Duration::from_millis(self.backoff_ms * attempts as u64));
                }
            }
        }
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
    fn retry_call_succeeds_after_failure() {
        let counter = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&counter);
        let result: Result<RetryResult<i32>, ()> = retry_call(3, move || {
            if c.fetch_add(1, Ordering::SeqCst) < 2 {
                Err(())
            } else {
                Ok(42)
            }
        });
        assert_eq!(result.unwrap().attempts, 3);
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
    fn check_passes_executes() {
        let result = Pipeline::call(
            &[WrapperKind::Check(Box::new(|| true))],
            || Ok::<_, ()>(42),
        );
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn check_fails_no_retry() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let cc = Arc::clone(&call_count);
        let result = Pipeline::call(
            &[WrapperKind::Check(Box::new(|| false)), WrapperKind::Retry],
            move || {
                cc.fetch_add(1, Ordering::SeqCst);
                Err::<i32, ()>(())
            },
        );
        assert!(result.is_err());
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn wrapper_kind_name() {
        assert_eq!(WrapperKind::None.name(), "None");
        assert_eq!(WrapperKind::Retry.name(), "Retry");
        assert_eq!(WrapperKind::Check(Box::new(|| true)).name(), "Check");
    }

    #[test]
    fn check_passes_executes_no_retry() {
        let called = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&called);
        let result = Pipeline::call(
            &[WrapperKind::Check(Box::new(|| true))],
            move || {
                c.fetch_add(1, Ordering::SeqCst);
                Ok::<i32, ()>(42)
            },
        );
        assert_eq!(result.unwrap(), 42);
        assert_eq!(called.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn wrapper_kind_debug() {
        let d = format!("{:?}", WrapperKind::None);
        assert_eq!(d, "None");
        let d = format!("{:?}", WrapperKind::Retry);
        assert_eq!(d, "Retry");
        let d = format!("{:?}", WrapperKind::Check(Box::new(|| true)));
        assert_eq!(d, "Check(<fn>)");
    }
}
