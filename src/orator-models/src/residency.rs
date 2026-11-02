//! Model residency: budget enforcement and voice-model holds.
//!
//! The conversational model stays warm while a session is open; larger
//! or assisting models load on demand and unload past the budget or
//! their idle timeout. Eviction order reuses the shared
//! `eviction_score` math (big cold footprints go first); voice holds
//! follow the leased shape (75 s leases, 30 s sweep, unload when the
//! lease set empties).

use std::collections::HashMap;

/// Seconds a voice-model hold lasts after acquire.
pub const LEASE_SECS: i64 = 75;

/// Milliseconds between lease sweeps.
pub const SWEEP_MS: u64 = 30_000;

/// One resident model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resident {
    /// Catalog tag.
    pub name: String,
    /// Resident bytes.
    pub bytes: u64,
    /// Last use (seconds since Unix epoch, `-1` when never used).
    pub last_used: i64,
}

impl Resident {
    /// Eviction priority at `now`: higher means evict first.
    #[must_use]
    pub fn score(&self, now: i64) -> u64 {
        common_core::cache::eviction_score(self.bytes, self.last_used, now)
    }
}

/// Budget enforcer over resident models.
#[derive(Debug, Clone, Default)]
pub struct ResidencyManager {
    /// Budget in bytes.
    pub budget_bytes: u64,
    residents: HashMap<String, Resident>,
}

impl ResidencyManager {
    /// Manager with `budget_bytes`.
    #[must_use]
    pub fn new(budget_bytes: u64) -> Self {
        Self {
            budget_bytes,
            residents: HashMap::new(),
        }
    }

    /// Register (or re-register) a resident.
    pub fn insert(&mut self, name: &str, bytes: u64, last_used: i64) {
        self.residents.insert(
            name.to_string(),
            Resident {
                name: name.to_string(),
                bytes,
                last_used,
            },
        );
    }

    /// Mark `name` used at `now`. Unknown names are ignored.
    pub fn touch(&mut self, name: &str, now: i64) {
        if let Some(resident) = self.residents.get_mut(name) {
            resident.last_used = now;
        }
    }

    /// Drop `name` from residency.
    pub fn remove(&mut self, name: &str) {
        self.residents.remove(name);
    }

    /// Resident bytes in total.
    #[must_use]
    pub fn used_bytes(&self) -> u64 {
        self.residents.values().map(|resident| resident.bytes).sum()
    }

    /// Resident names, best-eviction-first at `now`.
    #[must_use]
    pub fn eviction_order(&self, now: i64) -> Vec<String> {
        let mut residents: Vec<&Resident> = self.residents.values().collect();
        residents.sort_by(|a, b| {
            b.score(now)
                .cmp(&a.score(now))
                .then_with(|| b.last_used.cmp(&a.last_used))
        });
        residents.into_iter().map(|resident| resident.name.clone()).collect()
    }

    /// Names to evict (best-first) until `used <= budget`, at most all.
    #[must_use]
    pub fn plan_evictions(&self, now: i64) -> Vec<String> {
        let mut used = self.used_bytes();
        let mut plan = Vec::new();
        for name in self.eviction_order(now) {
            if used <= self.budget_bytes {
                break;
            }
            if let Some(resident) = self.residents.get(&name) {
                used = used.saturating_sub(resident.bytes);
                plan.push(name);
            }
        }
        plan
    }
}

/// Serial voice-model holds: acquire extends the lease, the sweep
/// drops expired entries, and the voice stack unloads when empty.
#[derive(Debug, Clone, Default)]
pub struct VoiceLeases {
    /// Lease expiry per model (seconds since Unix epoch).
    pub until: HashMap<String, i64>,
}

impl VoiceLeases {
    /// Empty lease set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Hold `name` for [`LEASE_SECS`] from `now`.
    pub fn acquire(&mut self, name: &str, now: i64) {
        self.until.insert(name.to_string(), now + LEASE_SECS);
    }

    /// Whether `name` holds a live lease at `now`.
    #[must_use]
    pub fn held(&self, name: &str, now: i64) -> bool {
        self.until.get(name).is_some_and(|until| *until > now)
    }

    /// Drop expired leases at `now`. Returns names released.
    pub fn sweep(&mut self, now: i64) -> Vec<String> {
        let expired: Vec<String> = self
            .until
            .iter()
            .filter(|(_, until)| **until <= now)
            .map(|(name, _)| name.clone())
            .collect();
        for name in &expired {
            self.until.remove(name);
        }
        expired
    }

    /// Live hold count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.until.len()
    }

    /// Whether no holds exist.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.until.is_empty()
    }
}

#[cfg(test)]
#[path = "../tests/residency.rs"]
mod tests;
