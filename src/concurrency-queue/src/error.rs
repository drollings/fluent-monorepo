use thiserror::Error;

#[derive(Error, Debug)]
pub enum QueueError {
    #[error("queue is full")]
    Full,
    #[error("queue sender disconnected")]
    Disconnected,
    #[error("task timed out")]
    Timeout,
    #[error("task canceled")]
    Canceled,
}
