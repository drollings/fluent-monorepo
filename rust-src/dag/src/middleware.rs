use std::sync::Arc;

use guidance_common::traits::WorkUnit;
use guidance_common::wrapper::{Instrumented, WithRetry};

pub trait Middleware: Send + Sync {
    fn wrap(&self, inner: Arc<dyn WorkUnit>) -> Arc<dyn WorkUnit>;
}

pub struct TimingMiddleware;

impl Middleware for TimingMiddleware {
    fn wrap(&self, inner: Arc<dyn WorkUnit>) -> Arc<dyn WorkUnit> {
        let wrapped = Instrumented::new(inner, "middleware");
        Arc::new(wrapped)
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
        let wrapped = WithRetry::new(inner, self.max_attempts, self.backoff_ms);
        Arc::new(wrapped)
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

    pub fn add(mut self, m: Box<dyn Middleware>) -> Self {
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
    use guidance_common::traits::{WorkContext, WorkError, WorkOutput};
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
        let unit = Arc::new(PassthroughUnit {
            name: "test".into(),
        });
        let wrapped = mw.wrap(unit);
        let ctx = WorkContext::default();
        let result = wrapped.execute(&ctx).unwrap();
        assert!(result.success);
    }

    #[test]
    fn test_retry_middleware() {
        let mw = RetryMiddleware::new(3, 1);
        let unit = Arc::new(PassthroughUnit {
            name: "retry_test".into(),
        });
        let wrapped = mw.wrap(unit);
        let ctx = WorkContext::default();
        let result = wrapped.execute(&ctx).unwrap();
        assert!(result.success);
    }

    #[test]
    fn test_middleware_chain() {
        let chain = MiddlewareChain::new()
            .add(Box::new(TimingMiddleware))
            .add(Box::new(RetryMiddleware::new(2, 1)));
        let unit = Arc::new(PassthroughUnit {
            name: "chained".into(),
        });
        let wrapped = chain.apply(unit);
        let ctx = WorkContext::default();
        let result = wrapped.execute(&ctx).unwrap();
        assert!(result.success);
    }
}
