//! Session registry: one entry per live conversation.
//!
//! The registry is the single source `ListSessions` and `GetState`
//! read. Entries own their turn machine, barge-in signal, and last
//! activity; the lock never crosses an await (the owner discipline).

use std::collections::HashMap;
use std::time::Instant;

use fluent_concurrency::stream::StreamAbort;

/// One live session.
pub struct Session {
    /// Stable id (`s1`, `s2`, …).
    pub id: String,
    /// Kind label (`assistant`, `dictation`, `meeting`, …).
    pub kind: String,
    /// Turn-taking machine for this session.
    pub machine: orator_turn::TurnMachine,
    /// Barge-in signal for the in-flight turn.
    pub abort: StreamAbort,
    /// Last activity for idle accounting.
    pub last_active: Instant,
}

/// The live set plus its id counter.
pub struct SessionRegistry {
    next: u64,
    map: HashMap<String, Session>,
}

impl SessionRegistry {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next: 1,
            map: HashMap::new(),
        }
    }

    /// Allocate and store one session; returns its id.
    pub fn start(&mut self, kind: &str, config: &orator_core::OratorConfig) -> String {
        let id = format!("s{}", self.next);
        self.next += 1;
        let session = Session {
            id: id.clone(),
            kind: kind.to_string(),
            machine: orator_turn::TurnMachine::new(config.turn.clone()),
            abort: StreamAbort::new(),
            last_active: Instant::now(),
        };
        self.map.insert(id.clone(), session);
        id
    }

    /// Fire the session abort and drop the entry.
    pub fn stop(&mut self, id: &str) -> Result<(), String> {
        let Some(session) = self.map.remove(id) else {
            return Err(format!("unknown session: {id}"));
        };
        session.abort.cancel();
        Ok(())
    }

    /// Fire the in-flight abort and re-arm for the next turn.
    pub fn interrupt(&mut self, id: &str) -> Result<(), String> {
        let Some(session) = self.map.get_mut(id) else {
            return Err(format!("unknown session: {id}"));
        };
        session.abort.cancel();
        session.abort = StreamAbort::new();
        session.last_active = Instant::now();
        Ok(())
    }

    /// All live ids in stable order.
    #[must_use]
    pub fn list(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.map.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Live ids of one kind in stable order.
    #[must_use]
    pub fn of_kind(&self, kind: &str) -> Vec<String> {
        let mut ids: Vec<String> = self
            .map
            .iter()
            .filter(|(_, session)| session.kind == kind)
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids
    }

    /// Whether `id` is live.
    #[must_use]
    pub fn contains(&self, id: &str) -> bool {
        self.map.contains_key(id)
    }

    /// Touch activity for `id` after a completed turn.
    pub fn touch(&mut self, id: &str) {
        if let Some(session) = self.map.get_mut(id) {
            session.last_active = Instant::now();
        }
    }

    /// Number of live sessions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether no sessions are live.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}
