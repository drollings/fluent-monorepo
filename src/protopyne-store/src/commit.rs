//! The provisional/committed bar lifecycle.
//!
//! A [`Bar`](protopyne_types::Bar) is by construction a finished bar: the
//! decision path runs on closed bars and nothing else. That leaves a gap — a
//! live venue republishes the bar that is still forming many times per
//! interval, and today those republications are simply rejected, because
//! `validate_new_bar` demands strictly increasing stamps and a republish
//! carries the stamp it is republishing.
//!
//! [`ClosedEmitter`] is where that gap is closed, and it is a state machine
//! rather than a timer for three reasons.
//!
//! - **Closure needs positive evidence.** A bar closes when a *strictly later*
//!   observation of the same key arrives, not when a wall-clock interval
//!   elapses. A timer that closed on schedule would invent an OHLCV row the
//!   venue never printed.
//! - **A truncated bar destroys the hole signal.** If a thin feed let a
//!   timer emit a partial bar at a missing stamp, the gap detector would see
//!   a continuous series and no repair would ever run. Refusing to close
//!   without evidence keeps the hole *detectable*, which is the property the
//!   repair loop depends on.
//! - **Bar boundaries are ET, not UTC.** Session-end persistence goes through
//!   [`TradingSession`](protopyne_types::session::TradingSession), whose
//!   daylight rules are already encoded. A second, DST-unaware clock is the
//!   failure this avoids.
//!
//! # The classification
//!
//! [`Arrival`] is a **task-value** statement, never a confidence reading. It
//! says where an observation landed relative to the bar currently forming.
//! There is no "maybe" and no score: an observation is a first sighting, a
//! restatement of the forming bar, the proof that the held bar closed, or a
//! hole.
//!
//! # Handoff
//!
//! The emitter hands out two queues and borrows the third. [`held`] is a
//! borrow, so a caller cannot persist a bar the emitter is still able to
//! revise. [`take_closed`] and [`take_late`] yield values: the bars that are
//! committed, and the observations that arrived too late to join them. Those
//! two queues are exactly the inputs `BarWindow::push` needs, so the store
//! never re-derives gap arithmetic.

use std::cmp::Ordering;
use std::fmt::Debug;

use chrono::{DateTime, Utc};
use protopyne_types::{Bar, TradingSession};

use crate::error::SnapshotError;

/// What "one thing on a tape" means for a snapshot: an identity to order by,
/// and a rule for reconciling two readings of that identity.
pub trait KeyedSnapshot: Sized {
    /// The identity two readings must share to be the same snapshot.
    type Key: Ord + Clone + Debug;

    /// This snapshot's identity.
    fn key(&self) -> Self::Key;

    /// Fold a later reading of the same identity into this one.
    ///
    /// Implementations **compose** the platform's existing structural
    /// validation; they never restate it. A refusal leaves `self` untouched.
    fn merge(&self, newer: &Self) -> Result<Self, SnapshotError>;
}

/// Where one observation landed relative to the bar currently forming.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arrival<K> {
    /// First sighting of this key; it is now forming.
    Opened(K),
    /// Same instant, different content: the venue restating the bar that has
    /// not finished. Ordinary streaming, and explicitly **not** a hole.
    Revised(K),
    /// The held bar closed, proved by this strictly later observation.
    Closed {
        /// The key of the bar that just closed.
        at: K,
    },
    /// Strictly older than the bar currently forming: a hole in the committed
    /// series. Retained, never dropped.
    Late {
        /// The key of the observation that arrived out of order.
        at: K,
        /// The forming bar it did not displace, or `None` when the emitter
        /// was holding nothing.
        newest_open: Option<K>,
    },
}

/// Why a forming bar was committed without a later bar proving it closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealReason {
    /// The held bar's stamp fell outside the trading session.
    SessionEnd,
    /// The process is ending; the bar is committed rather than discarded.
    Shutdown,
    /// An operator or a caller asked for it.
    Explicit,
}

/// What the emitter has seen, one counter per [`Arrival`] shape plus seals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClosedCounts {
    /// First sightings.
    pub opened: u64,
    /// Restatements of a forming bar.
    pub revised: u64,
    /// Bars closed by a strictly later observation.
    pub closed: u64,
    /// Out-of-order observations retained as hole signals.
    pub late: u64,
    /// Bars committed by a seal.
    pub sealed: u64,
}

/// Turns a stream of observations into a stream of committed snapshots.
///
/// The emitter owns the one bar that can still change. Nothing is emitted
/// until closure is proved or a [`SealReason`] is stated, so a caller can
/// never receive a provisional value by accident.
pub struct ClosedEmitter<S: KeyedSnapshot> {
    held: Option<S>,
    closed: Vec<S>,
    late: Vec<S>,
    counts: ClosedCounts,
    sealed_by: Option<SealReason>,
}

impl<S: KeyedSnapshot> ClosedEmitter<S> {
    /// An emitter that has seen nothing.
    #[must_use]
    pub fn new() -> Self {
        Self {
            held: None,
            closed: Vec::new(),
            late: Vec::new(),
            counts: ClosedCounts::default(),
            sealed_by: None,
        }
    }

    /// Feed one observation and report where it landed.
    ///
    /// A strictly later observation closes the held bar, commits it to the
    /// closed queue, and becomes the new held bar. An equal one restates the
    /// held bar through [`KeyedSnapshot::merge`]; a refusal leaves the held
    /// bar exactly as it was and surfaces the shared validator's reason. A
    /// strictly earlier one is retained as a hole signal and changes nothing.
    pub fn observe(&mut self, next: S) -> Result<Arrival<S::Key>, SnapshotError> {
        let key = next.key();
        let Some(held) = self.held.as_ref() else {
            self.held = Some(next);
            self.counts.opened += 1;
            return Ok(Arrival::Opened(key));
        };

        match key.cmp(&held.key()) {
            Ordering::Greater => {
                let held_key = self
                    .held
                    .as_ref()
                    .expect("held was observed to be present above")
                    .key();
                let closed = self
                    .held
                    .take()
                    .expect("held was observed to be present above");
                self.counts.closed += 1;
                self.closed.push(closed);
                self.held = Some(next);
                Ok(Arrival::Closed { at: held_key })
            }
            Ordering::Equal => {
                let merged = held.merge(&next)?;
                self.held = Some(merged);
                self.counts.revised += 1;
                Ok(Arrival::Revised(key))
            }
            Ordering::Less => {
                let newest_open = self.held.as_ref().map(KeyedSnapshot::key);
                self.counts.late += 1;
                self.late.push(next);
                Ok(Arrival::Late {
                    at: key,
                    newest_open,
                })
            }
        }
    }

    /// Borrow the bar that is still forming. A borrow, not a clone: the
    /// emitter keeps ownership precisely so a caller cannot persist a value
    /// the venue may still restate.
    #[must_use]
    pub fn held(&self) -> Option<&S> {
        self.held.as_ref()
    }

    /// Commit the held bar under an explicit reason, releasing the emitter.
    ///
    /// Returns the bar exactly once: a second seal finds nothing held and is
    /// a no-op, so a caller cannot double-count a bar.
    pub fn seal(&mut self, reason: SealReason) -> Option<S> {
        let sealed = self.held.take()?;
        self.counts.sealed += 1;
        self.sealed_by = Some(reason);
        Some(sealed)
    }

    /// Take every bar that has been committed so far.
    pub fn take_closed(&mut self) -> Vec<S> {
        std::mem::take(&mut self.closed)
    }

    /// Take every observation that arrived out of order. These are the hole
    /// signals; they are retained, never dropped, because a hole the emitter
    /// swallowed is a hole the repair loop will never close.
    pub fn take_late(&mut self) -> Vec<S> {
        std::mem::take(&mut self.late)
    }

    /// What the emitter has seen so far.
    #[must_use]
    pub fn counts(&self) -> ClosedCounts {
        self.counts
    }

    /// Why the last seal fired, or `None` if none has.
    #[must_use]
    pub fn sealed_by(&self) -> Option<SealReason> {
        self.sealed_by
    }
}

impl<S: KeyedSnapshot> Default for ClosedEmitter<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl ClosedEmitter<Bar> {
    /// Commit the held bar if its stamp has fallen outside the trading
    /// session.
    ///
    /// The boundary is evaluated by [`TradingSession`] on its own ET wall
    /// clock, which already encodes the daylight rules. Nothing here computes
    /// an offset or a timer period, so this cannot disagree with the session
    /// every other crate reads.
    pub fn seal_session_end(&mut self, session: TradingSession) -> Option<Bar> {
        let outside = self
            .held
            .as_ref()
            .is_some_and(|bar| !session.contains(bar.ts.timestamp_millis()));
        if outside {
            self.seal(SealReason::SessionEnd)
        } else {
            None
        }
    }
}

impl KeyedSnapshot for Bar {
    type Key = DateTime<Utc>;

    fn key(&self) -> DateTime<Utc> {
        self.ts
    }

    /// Fold a later reading of this bar into this one: the open stays the
    /// first print's, the close becomes the latest print's, and the range
    /// only ever widens, because a high or low that was really printed is a
    /// fact that cannot be retracted by a later print.
    ///
    /// The incoming reading is checked **before** the fold, and the folded
    /// result is checked after it. The first check is what refuses a
    /// malformed print; it cannot be left to the second, because a union fold
    /// of two *valid* readings is always valid again — the widest high and
    /// the lowest low cover whatever either side opened or closed at — so
    /// validating only the result would silently absorb a garbage print into
    /// a forming bar.
    ///
    /// Both checks call [`Bar::validate`], the one place OHLC invariants live
    /// in this workspace; this method does not restate the cover checks. The
    /// refusal carries that validator's reason verbatim.
    fn merge(&self, newer: &Self) -> Result<Self, SnapshotError> {
        if self.ts != newer.ts {
            return Err(SnapshotError::KeyConflict {
                held: self.ts.to_rfc3339(),
                newer: newer.ts.to_rfc3339(),
            });
        }
        newer
            .validate()
            .map_err(|reason| SnapshotError::Inconsistent {
                reason: reason.to_string(),
            })?;
        let merged = Bar {
            ts: self.ts,
            o: self.o,
            h: if newer.h > self.h { newer.h } else { self.h },
            l: if newer.l < self.l { newer.l } else { self.l },
            c: newer.c,
            volume: newer.volume,
            adj: newer.adj.clone().or_else(|| self.adj.clone()),
        };
        merged
            .validate()
            .map_err(|reason| SnapshotError::Inconsistent {
                reason: reason.to_string(),
            })?;
        Ok(merged)
    }
}

#[cfg(test)]
#[path = "../tests/common/tapes.rs"]
pub(crate) mod test_tapes;

#[cfg(test)]
#[path = "../tests/commit_lifecycle.rs"]
mod tests_lifecycle;

#[cfg(test)]
#[path = "../tests/commit_seal.rs"]
mod tests_seal;

#[cfg(test)]
#[path = "../tests/commit_merge_refusal.rs"]
mod tests_merge_refusal;
