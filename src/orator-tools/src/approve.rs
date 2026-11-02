//! Approvals: a rendezvous with a deadline. Answered by a notification
//! action, an `approve` command, or verified non-ambient voice — never by
//! ambient or unverified speech, and never by an unknown caller id.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// Approval identifiers.
pub type ApprovalId = String;

/// One pending approval: what the human is asked, and the channel their
/// answer arrives on.
pub struct ApprovalHub {
    pending: Mutex<HashMap<ApprovalId, (tokio::sync::oneshot::Sender<bool>, String)>>,
    seq: AtomicU64,
}

impl Default for ApprovalHub {
    fn default() -> Self {
        Self::new()
    }
}

impl ApprovalHub {
    /// Empty hub.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(0),
        }
    }

    /// Open an approval round for `summary`. Returns the id plus the
    /// receiver the waiter holds; the answer arrives via [`answer`](Self::answer).
    pub fn request(&self, summary: String) -> (ApprovalId, tokio::sync::oneshot::Receiver<bool>) {
        let id = format!("appr-{}", self.seq.fetch_add(1, Ordering::Relaxed));
        let (send, recv) = tokio::sync::oneshot::channel();
        common_core::sync::lock(&self.pending).insert(id.clone(), (send, summary));
        (id, recv)
    }

    /// The human-readable summary for `id`, if the round is still open.
    #[must_use]
    pub fn summary(&self, id: &str) -> Option<String> {
        common_core::sync::lock(&self.pending)
            .get(id)
            .map(|(_, summary)| summary.clone())
    }

    /// Answer `id` with `yes`. Unknown ids refuse (spoof-safe): the answer
    /// is dropped and `false` returns.
    pub fn answer(&self, id: &str, yes: bool) -> bool {
        match common_core::sync::lock(&self.pending).remove(id) {
            Some((send, _)) => send.send(yes).is_ok(),
            None => false,
        }
    }

    /// Answer from voice: accepted only when the speaker verified AND the
    /// audio is not ambient. Anything else denies and reports refusal.
    pub fn answer_voice(&self, id: &str, verified: bool, ambient: bool) -> bool {
        if verified && !ambient {
            self.answer(id, true)
        } else {
            self.answer(id, false);
            false
        }
    }

    /// Drop the round without answering. Late answers then refuse.
    pub fn cancel(&self, id: &str) -> bool {
        common_core::sync::lock(&self.pending).remove(id).is_some()
    }

    /// Wait for the answer to `id` up to `deadline_ms`. Timeout, cancel,
    /// or a dropped sender all resolve to `None` — the caller denies —
    /// and the round drains so `pending_count` cannot leak and late
    /// answers refuse.
    pub async fn wait(
        &self,
        id: &str,
        recv: tokio::sync::oneshot::Receiver<bool>,
        deadline_ms: u64,
    ) -> Option<bool> {
        struct Drain<'a> {
            hub: &'a ApprovalHub,
            id: String,
            defused: bool,
        }
        impl Drop for Drain<'_> {
            fn drop(&mut self) {
                if !self.defused {
                    let _ = self.hub.cancel(&self.id);
                }
            }
        }
        let mut drain = Drain {
            hub: self,
            id: id.to_string(),
            defused: false,
        };
        let answered = tokio::time::timeout(Duration::from_millis(deadline_ms.max(1)), recv)
            .await
            .ok()
            .and_then(std::result::Result::ok);
        if answered.is_some() {
            // `answer` already removed the entry; defuse the drop guard.
            drain.defused = true;
            let _ = self.cancel(id);
            answered
        } else {
            let _ = self.cancel(id);
            drain.defused = true;
            None
        }
    }

    /// Pending round count. Drains as rounds answer or time out.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        common_core::sync::lock(&self.pending).len()
    }
}

#[cfg(test)]
#[path = "../tests/approve.rs"]
mod tests;
