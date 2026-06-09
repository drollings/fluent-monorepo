#[derive(Debug, Clone)]
pub struct QueueConfig {
    pub worker_count: usize,
    pub timeout_ms: u64,
    pub retry_policy: RetryPolicy,
    pub channel_capacity: usize,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            worker_count: 1,
            timeout_ms: 60000,
            retry_policy: RetryPolicy::None,
            channel_capacity: 100,
        }
    }
}

#[derive(Debug, Clone)]
pub enum RetryPolicy {
    None,
    Fixed {
        max_attempts: u32,
        backoff_ms: u64,
    },
    Exponential {
        max_attempts: u32,
        base_ms: u64,
        max_ms: u64,
    },
}
