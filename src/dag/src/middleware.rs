use std::sync::Arc;

use fluent_wvr::wrapper::{Instrumented, WithRetry};
use fluent_wvr::WorkUnit;

pub trait Middleware: Send + Sync {
    fn wrap(&self, inner: Arc<dyn WorkUnit>) -> Arc<dyn WorkUnit>;
}

pub struct TimingMiddleware;
impl Middleware for TimingMiddleware {
    fn wrap(&self, inner: Arc<dyn WorkUnit>) -> Arc<dyn WorkUnit> {
        Arc::new(Instrumented::new(inner, "middleware"))
    }
}

pub struct RetryMiddleware {
    max_attempts: u32,
    backoff_ms: u64,
}
impl RetryMiddleware {
    pub fn new(max_attempts: u32, backoff_ms: u64) -> Self {
        Self {
            max_attempts,
            backoff_ms,
        }
    }
}
impl Middleware for RetryMiddleware {
    fn wrap(&self, inner: Arc<dyn WorkUnit>) -> Arc<dyn WorkUnit> {
        Arc::new(WithRetry::new(inner, self.max_attempts, self.backoff_ms))
    }
}

pub struct MiddlewareChain {
    middlewares: Vec<Box<dyn Middleware>>,
}
impl MiddlewareChain {
    pub fn new() -> Self {
        Self {
            middlewares: Vec::new(),
        }
    }
    #[must_use]
    pub fn push(mut self, m: Box<dyn Middleware>) -> Self {
        self.middlewares.push(m);
        self
    }
    pub fn apply(&self, unit: Arc<dyn WorkUnit>) -> Arc<dyn WorkUnit> {
        let mut result = unit;
        for mw in &self.middlewares {
            result = mw.wrap(result);
        }
        result
    }
}
impl Default for MiddlewareChain {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluent_wvr::{WorkContext, WorkError, WorkOutput, WorkUnit};
    use internment::ArcIntern;

    struct PassthroughUnit {
        name: String,
    }
    impl WorkUnit for PassthroughUnit {
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
            Ok(WorkOutput::ok("passthrough"))
        }
    }

    #[test]
    fn test_timing_middleware() {
        let mw = TimingMiddleware;
        let wrapped = mw.wrap(Arc::new(PassthroughUnit {
            name: "test".into(),
        }));
        assert!(wrapped.execute(&WorkContext::default()).unwrap().success);
    }
    #[test]
    fn test_retry_middleware() {
        let wrapped = RetryMiddleware::new(3, 1).wrap(Arc::new(PassthroughUnit {
            name: "retry_test".into(),
        }));
        assert!(wrapped.execute(&WorkContext::default()).unwrap().success);
    }
    #[test]
    fn test_middleware_chain() {
        let chain = MiddlewareChain::new()
            .push(Box::new(TimingMiddleware))
            .push(Box::new(RetryMiddleware::new(2, 1)));
        let wrapped = chain.apply(Arc::new(PassthroughUnit {
            name: "chained".into(),
        }));
        assert!(wrapped.execute(&WorkContext::default()).unwrap().success);
    }
}
