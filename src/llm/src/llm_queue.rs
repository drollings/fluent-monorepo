use std::sync::Arc;

use guidance_concurrency_queue::{EventQueue, QueueConfig, QueueError};
use tokio::sync::oneshot;

use crate::client::{ChatMessage, LlmConfig, LlmError};

pub struct LlmTask {
    pub messages: Vec<ChatMessage>,
    pub config: LlmConfig,
    pub response_tx: oneshot::Sender<Result<String, LlmError>>,
}

pub struct LlmRequestQueue {
    inner: Arc<EventQueue<LlmTask>>,
}

fn process_llm_task(task: LlmTask) {
    let result = make_llm_request(&task.messages, &task.config);
    let _ = task.response_tx.send(result);
}

impl LlmRequestQueue {
    pub fn new(config: &QueueConfig) -> Self {
        Self {
                inner: Arc::new(EventQueue::new(config, process_llm_task)),
        }
    }

    pub fn submit(
        &self,
        messages: Vec<ChatMessage>,
        config: LlmConfig,
    ) -> Result<String, LlmError> {
        let (tx, rx) = oneshot::channel();
        let task = LlmTask {
            messages,
            config,
            response_tx: tx,
        };
        self.inner.submit(task).map_err(|e| match e {
            QueueError::Full => LlmError::Http("queue full".into()),
            QueueError::Disconnected => LlmError::Http("queue disconnected".into()),
            QueueError::Timeout => LlmError::Http("queue timeout".into()),
            QueueError::Canceled => LlmError::Http("queue canceled".into()),
        })?;
        rx.blocking_recv()
            .map_err(|_| LlmError::Http("queue response canceled".into()))?
    }

    pub async fn submit_async(
        &self,
        messages: Vec<ChatMessage>,
        config: LlmConfig,
    ) -> Result<String, LlmError> {
        let (tx, rx) = oneshot::channel();
        let task = LlmTask {
            messages,
            config,
            response_tx: tx,
        };
        self.inner.submit(task).map_err(|e| match e {
            QueueError::Full => LlmError::Http("queue full".into()),
            QueueError::Disconnected => LlmError::Http("queue disconnected".into()),
            QueueError::Timeout => LlmError::Http("queue timeout".into()),
            QueueError::Canceled => LlmError::Http("queue canceled".into()),
        })?;
        rx.await
            .map_err(|_| LlmError::Http("queue response canceled".into()))?
    }
}

fn make_llm_request(messages: &[ChatMessage], config: &LlmConfig) -> Result<String, LlmError> {
    crate::client::chat_complete_http(&config.api_url, messages, &config.model, config.think)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::LlmConfig;

    #[test]
    fn test_llm_request_queue_creation() {
        let queue = LlmRequestQueue::new(&QueueConfig::default());
        let messages = vec![ChatMessage {
            role: "user".into(),
            content: "hello".into(),
        }];
        let config = LlmConfig::new()
            .api_url("http://localhost:11434/v1".into())
            .model("test".into())
            .build();

        // No server running, so submit should eventually fail
        let result = queue.submit(messages, config);
        assert!(result.is_err());
    }
}
