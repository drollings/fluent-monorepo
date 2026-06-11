use std::sync::Arc;

use crossbeam_channel::{bounded, Sender, TrySendError};

use crate::config::QueueConfig;
use crate::error::QueueError;

pub struct EventQueue<T: Send + 'static> {
    sender: Sender<T>,
}

impl<T: Send + 'static> EventQueue<T> {
    pub fn new<F>(config: &QueueConfig, handler: F) -> Self
    where
        F: Fn(T) + Send + Sync + Clone + 'static,
    {
        let (sender, receiver) = bounded::<T>(config.channel_capacity);
        let receiver = Arc::new(receiver);

        for _ in 0..config.worker_count {
            let rx = Arc::clone(&receiver);
            let handler = handler.clone();
            std::thread::Builder::new()
                .name("queue-worker".into())
                .spawn(move || {
                    while let Ok(task) = rx.recv() {
                        handler(task);
                    }
                })
                .expect("failed to spawn queue worker");
        }

        Self { sender }
    }

    pub fn submit(&self, task: T) -> Result<(), QueueError> {
        match self.sender.try_send(task) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(QueueError::Full),
            Err(TrySendError::Disconnected(_)) => Err(QueueError::Disconnected),
        }
    }

    pub fn blocking_submit(&self, task: T) -> Result<(), QueueError> {
        self.sender.send(task).map_err(|_| QueueError::Disconnected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn test_event_queue_submit_and_process() {
        let results = Arc::new(Mutex::new(Vec::new()));
        let results_clone = Arc::clone(&results);
        let queue: EventQueue<i32> = EventQueue::new(
            &QueueConfig {
                worker_count: 1,
                timeout_ms: 1000,
                retry_policy: crate::RetryPolicy::None,
                channel_capacity: 10,
            },
            move |task| {
                let mut r = results_clone.lock().unwrap();
                r.push(task * 2);
            },
        );

        queue.submit(21).unwrap();
        queue.submit(22).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));

        let r = results.lock().unwrap();
        assert_eq!(*r, vec![42, 44]);
    }

    #[test]
    fn test_event_queue_multiple_workers() {
        let results = Arc::new(Mutex::new(Vec::new()));
        let results_clone = Arc::clone(&results);
        let queue: EventQueue<i32> = EventQueue::new(
            &QueueConfig {
                worker_count: 3,
                timeout_ms: 1000,
                retry_policy: crate::RetryPolicy::None,
                channel_capacity: 100,
            },
            move |task| {
                let mut r = results_clone.lock().unwrap();
                r.push(task);
            },
        );

        for i in 0..10 {
            queue.submit(i).unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(200));

        let mut r = results.lock().unwrap();
        assert_eq!(r.len(), 10);
        r.sort();
        let expected: Vec<i32> = (0..10).collect();
        assert_eq!(*r, expected);
    }

    #[test]
    fn test_event_queue_full() {
        let queue: EventQueue<i32> = EventQueue::new(
            &QueueConfig {
                worker_count: 1,
                timeout_ms: 1000,
                retry_policy: crate::RetryPolicy::None,
                channel_capacity: 1,
            },
            move |_task| {
                std::thread::sleep(std::time::Duration::from_millis(500));
            },
        );

        // Fill the channel
        queue.submit(1).unwrap();
        // Second submit should fail since capacity is 1 and worker is busy
        let result = queue.submit(2);
        assert!(result.is_err());
    }

    #[test]
    fn test_event_queue_blocking_submit_unbounded_effective() {
        let results = Arc::new(Mutex::new(Vec::new()));
        let results_clone = Arc::clone(&results);
        let queue: EventQueue<i32> = EventQueue::new(
            &QueueConfig {
                worker_count: 2,
                timeout_ms: 1000,
                retry_policy: crate::RetryPolicy::None,
                channel_capacity: 10,
            },
            move |task| {
                let mut r = results_clone.lock().unwrap();
                r.push(task);
            },
        );

        queue.blocking_submit(99).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));

        let r = results.lock().unwrap();
        assert!(r.contains(&99));
    }
}
