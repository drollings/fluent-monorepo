//! Bounded notice queue: one live list with dedupe, expiry, and aborts.
//!
//! A [`Notice`] is one display item (text, optional action label, dwell,
//! dedupe key) behind the uniform component face, so a future command palette
//! stages it through `FieldAccess` like every other control-plane value. The
//! [`NoticeCenter`] owns the live list: offers merge on a repeated
//! non-empty dedupe key (the flapping source refreshes instead of flooding),
//! refuse past the bound instead of growing, expire on the owner's tick, and
//! carry one abort handle per notice so waiters observe dismissal.
//!
//! The center never sleeps and never spawns: the surrounding scope owns the
//! drive task and calls [`NoticeCenter::expire`] on its own cadence, while
//! painters read snapshots and stack them with [`offsets`].

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use common_core::sync::lock;
use fluent_concurrency::stream::StreamAbort;
use fluent_wvr::prelude::*;
use fluent_wvr::validate::{check_for_execute, valid_output, Validate};
use fluent_wvr::{Describable, FieldAccess};

use crate::error::EguiError;

fn default_action() -> String {
    String::new()
}

fn default_dedupe_key() -> String {
    String::new()
}

fn default_dwell_ms() -> u64 {
    5000
}

/// One notice: text plus an optional action label, a dwell, and the key
/// duplicate bursts coalesce on. An empty dedupe key opts out of merging —
/// keyless notices never absorb each other.
#[derive(
    Debug, Clone, bon::Builder, serde::Serialize, serde::Deserialize, FieldAccess, Describable,
)]
#[builder(start_fn = new)]
/// One notice: text plus an optional action label, a dwell, and the key
/// duplicate bursts coalesce on. An empty dedupe key opts out of merging —
/// keyless notices never absorb each other.
pub struct Notice {
    /// Notice text.
    #[field(desc = "Notice text", max_len = 280)]
    pub text: String,
    /// Action button label; empty paints no button.
    #[field(desc = "Action label; empty shows no button")]
    #[builder(default = default_action())]
    #[serde(default = "default_action")]
    pub action: String,
    /// How long the notice stays live, in milliseconds.
    #[field(desc = "Dwell time in milliseconds", min = 1)]
    #[builder(default = default_dwell_ms())]
    #[serde(default = "default_dwell_ms")]
    pub dwell: u64,
    /// Bursts sharing a non-empty key merge into one live notice.
    #[field(desc = "Dedupe key; empty never merges")]
    #[builder(default = default_dedupe_key())]
    #[serde(default = "default_dedupe_key")]
    pub dedupe_key: String,
}

impl Notice {
    /// Fail-closed self-consistency check.
    pub fn validate(&self) -> Result<(), EguiError> {
        if self.text.trim().is_empty() {
            return Err(EguiError::InvalidParam {
                what: "text".to_owned(),
                reason: "notice text must not be empty".to_owned(),
            });
        }
        if self.dwell == 0 {
            return Err(EguiError::InvalidParam {
                what: "dwell".to_owned(),
                reason: "notice dwell must be at least one millisecond".to_owned(),
            });
        }
        Ok(())
    }
}

impl Validate for Notice {
    type Err = EguiError;
    fn validate(&self) -> Result<(), Self::Err> {
        Notice::validate(self)
    }
}

impl WorkUnit for Notice {
    fn name(&self) -> &str {
        "notice"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        check_for_execute(self, "notice invalid")?;
        Ok(valid_output(format!("notice ready ({})", self.text)))
    }
}

impl_component!(Notice);

/// One live notice: the display value, its dismissal line, and its deadline.
struct LiveNotice {
    notice: Notice,
    abort: StreamAbort,
    expires_at: Instant,
}

/// Shared live list behind the cheaply-cloned center handle.
struct CenterState {
    limit: usize,
    queue: VecDeque<LiveNotice>,
}

/// A bounded notice center: deduped intake, per-notice aborts, owner-ticked
/// expiry. Clone shares the list; every method locks briefly and never
/// across an await.
#[derive(Clone)]
/// A bounded notice center: deduped intake, per-notice aborts, owner-ticked
/// expiry. Clone shares the list; every method locks briefly and never
/// across an await.
pub struct NoticeCenter {
    state: Arc<Mutex<CenterState>>,
}

impl NoticeCenter {
    /// Open a center holding at most `limit` live notices; refuses zero.
    pub fn new(limit: usize) -> Result<Self, EguiError> {
        if limit == 0 {
            return Err(EguiError::InvalidParam {
                what: "limit".to_owned(),
                reason: "notice center must hold at least one notice".to_owned(),
            });
        }
        Ok(Self {
            state: Arc::new(Mutex::new(CenterState {
                limit,
                queue: VecDeque::new(),
            })),
        })
    }

    /// Offer a notice: refusals (malformed, or a full list) name their
    /// cause. A repeated non-empty key refreshes the live entry in place —
    /// latest text wins, dwell restarts — instead of appending.
    pub fn offer(&self, notice: Notice) -> Result<(), EguiError> {
        notice.validate()?;
        let mut state = lock(&self.state);
        if !notice.dedupe_key.is_empty() {
            let position = state
                .queue
                .iter()
                .position(|live| live.notice.dedupe_key == notice.dedupe_key);
            if let Some(index) = position {
                let live = &mut state.queue[index];
                live.notice = notice.clone();
                live.expires_at = Instant::now() + std::time::Duration::from_millis(notice.dwell);
                return Ok(());
            }
        }
        if state.queue.len() >= state.limit {
            return Err(EguiError::InvalidParam {
                what: "notice".to_owned(),
                reason: "notice center is full".to_owned(),
            });
        }
        state.queue.push_back(LiveNotice {
            expires_at: Instant::now() + std::time::Duration::from_millis(notice.dwell),
            notice,
            abort: StreamAbort::new(),
        });
        Ok(())
    }

    /// Dismiss the first notice under `key`, firing its abort. Reports
    /// whether exactly one entry popped.
    pub fn close(&self, key: &str) -> bool {
        let mut state = lock(&self.state);
        let position = state
            .queue
            .iter()
            .position(|live| live.notice.dedupe_key == key);
        match position {
            Some(index) => {
                if let Some(live) = state.queue.remove(index) {
                    live.abort.cancel();
                }
                true
            }
            None => false,
        }
    }

    /// Expire every notice currently due, firing each abort. Returns how many
    /// popped. The surrounding scope's drive task calls this on its own
    /// cadence; the center itself never sleeps.
    pub fn expire_due(&self) -> usize {
        let now = Instant::now();
        let mut state = lock(&self.state);
        let mut kept = VecDeque::with_capacity(state.queue.len());
        let mut popped = 0;
        for live in state.queue.drain(..) {
            if live.expires_at <= now {
                live.abort.cancel();
                popped += 1;
            } else {
                kept.push_back(live);
            }
        }
        state.queue = kept;
        popped
    }

    /// Live notice count.
    #[must_use]
    pub fn len(&self) -> usize {
        lock(&self.state).queue.len()
    }

    /// Whether no notice is live.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        lock(&self.state).queue.is_empty()
    }

    /// The live notices in display order, cloned for the painter.
    #[must_use]
    pub fn notices(&self) -> Vec<Notice> {
        lock(&self.state)
            .queue
            .iter()
            .map(|live| live.notice.clone())
            .collect()
    }
}

/// Stacked row offsets for `count` notices from a stack origin: row `i`
/// sits at `i * (row_height + gap)`. Degenerate heights read as zero so a
/// malformed metric collapses the stride instead of flipping the stack.
#[must_use]
/// Stacked row offsets for `count` notices from a stack origin: row `i`
/// sits at `i * (row_height + gap)`. Degenerate heights read as zero so a
/// malformed metric collapses the stride instead of flipping the stack.
pub fn offsets(count: usize, row_height: f32, gap: f32) -> Vec<f32> {
    let row_height = if row_height.is_finite() && row_height > 0.0 {
        row_height
    } else {
        0.0
    };
    let gap = if gap.is_finite() && gap > 0.0 {
        gap
    } else {
        0.0
    };
    let stride = row_height + gap;
    (0..count)
        .map(|index| {
            // Display stacks are short, so the `usize`-to-`f32` cast is
            // exact for every realistic queue.
            index as f32 * stride
        })
        .collect()
}

#[cfg(test)]
#[path = "../tests/notice.rs"]
mod tests;