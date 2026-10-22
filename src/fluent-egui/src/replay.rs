//! Replay and checkpoints over a caller-built pane plan.
//!
//! Frames stay dumb during replay too: a [`ReplayController`] owns the
//! [`PanePlan`] (which owns the canonical checkpoint graph) plus the
//! [`PaneCache`], so scrub-back is [`rewind_to`] on pane steps and catch-up
//! is [`ready`] waves in plan order — never a hand-rolled sorter. Re-running
//! a rewound suffix republishes bit-identical series, and content-hash
//! backdating keeps every revision, proving the re-run reproduced the
//! cached panes exactly.
//!
//! [`ReplayState`] is the toolbar-facing cursor: a `Component` with
//! `Describable`, so the replay toolbar renders from its schema like every
//! other control-plane struct. Domain export (server-shaped CSV/JSON)
//! composes the cache revisions in the terminal crate.
//!
//! [`rewind_to`]: ReplayController::rewind_to
//! [`ready`]: ReplayController::ready

use fluent_wvr::prelude::*;
use fluent_wvr::validate::{check_for_execute, valid_output, Validate};

use crate::error::EguiError;
use crate::plan::{PaneCache, PanePlan};

/// Toolbar-facing replay cursor: which bar is on screen, over how many, at
/// which named mark. Renders from [`Describable`] schema; `execute`
/// validates through the shared bridge and reports the cursor.
#[derive(
    Debug, Clone, bon::Builder, serde::Serialize, serde::Deserialize, FieldAccess, Describable,
)]
#[builder(start_fn = new)]
pub struct ReplayState {
    /// Bar index on screen (0-based, at most `total_bars`).
    #[field(desc = "Bar index on screen", min = 0)]
    #[builder(default = 0)]
    pub cursor: u64,
    /// Bars in the replay window.
    #[field(desc = "Bars in the replay window", min = 0)]
    #[builder(default = 0)]
    pub total_bars: u64,
    /// Named replay mark at the cursor.
    #[field(desc = "Named replay mark at the cursor", max_len = 64)]
    #[builder(default = "live".to_owned())]
    #[serde(default = "default_mark")]
    pub mark: String,
}

fn default_mark() -> String {
    "live".to_owned()
}

impl Default for ReplayState {
    fn default() -> Self {
        Self::new().build()
    }
}

impl ReplayState {
    /// Fail-closed self-consistency check.
    pub fn validate(&self) -> Result<(), EguiError> {
        if self.mark.trim().is_empty() {
            return Err(EguiError::InvalidParam {
                what: "mark".to_owned(),
                reason: "replay mark must not be empty".to_owned(),
            });
        }
        if self.cursor > self.total_bars {
            return Err(EguiError::InvalidParam {
                what: "cursor".to_owned(),
                reason: format!(
                    "cursor {} runs past {} replay bars",
                    self.cursor, self.total_bars
                ),
            });
        }
        Ok(())
    }
}

impl Validate for ReplayState {
    type Err = EguiError;
    fn validate(&self) -> Result<(), Self::Err> {
        ReplayState::validate(self)
    }
}

impl WorkUnit for ReplayState {
    fn name(&self) -> &str {
        "replay_state"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        check_for_execute(self, "replay state invalid")?;
        Ok(valid_output(format!(
            "replay ready (cursor {}/{})",
            self.cursor, self.total_bars
        )))
    }
}

impl_component!(ReplayState);

/// Scrub-back controller over one pane plan plus its series cache.
///
/// Marks name checkpoints; rewinding to a mark un-completes its suffix for
/// re-run (the mark is consumed — re-mark to rewind twice). Catch-up waves
/// complete [`ready`] panes in plan order. Publishing re-runs into the
/// cache proves identity: unchanged content keeps every revision.
pub struct ReplayController {
    plan: PanePlan,
    cache: PaneCache,
}

impl ReplayController {
    /// Create an empty controller; register the plan topology before use.
    #[must_use]
    pub fn new() -> Self {
        Self {
            plan: PanePlan::new(),
            cache: PaneCache::new(),
        }
    }

    /// Create a controller over an already-built plan.
    #[must_use]
    pub fn with_plan(plan: PanePlan) -> Self {
        Self {
            plan,
            cache: PaneCache::new(),
        }
    }

    /// Borrow the pane plan (order, readiness, checkpoints).
    #[must_use]
    pub fn plan(&self) -> &PanePlan {
        &self.plan
    }

    /// Mutably borrow the pane plan (topology registration, checkpoints).
    #[must_use]
    pub fn plan_mut(&mut self) -> &mut PanePlan {
        &mut self.plan
    }

    /// Borrow the series cache (revisions, backdating).
    #[must_use]
    pub fn cache(&self) -> &PaneCache {
        &self.cache
    }

    /// Record `checkpoint` as a replay mark; fails for unregistered panes.
    pub fn mark(&mut self, checkpoint: &str) -> Result<(), EguiError> {
        self.plan.checkpoint(checkpoint)
    }

    /// Whether `name` is a recorded mark.
    #[must_use]
    pub fn is_checkpoint(&self, name: &str) -> bool {
        self.plan.is_checkpoint(name)
    }

    /// Record `pane` as computed; its dependents become ready.
    pub fn complete(&mut self, pane: &str) {
        self.plan.complete(pane);
    }

    /// Panes with all dependencies complete that still need a run, in plan
    /// order: the next catch-up wave. Unlike the raw `ready_steps`, already
    /// completed panes are excluded — a wave is work to do, not work done.
    #[must_use]
    pub fn ready(&self) -> Vec<String> {
        self.plan
            .ready_steps()
            .into_iter()
            .filter(|pane| !self.plan.is_completed(pane))
            .collect()
    }

    /// Scrub back to mark `checkpoint`: returns the ordered panes from the
    /// mark onward (inclusive) and un-completes them for re-run. The mark is
    /// consumed. Fails for unknown marks.
    pub fn rewind_to(&mut self, checkpoint: &str) -> Result<Vec<String>, EguiError> {
        self.plan.rewind_to(checkpoint)
    }

    /// Publish one pane's series under its cache key, returning its
    /// revision. Bit-identical content backdates (revision unchanged).
    /// Callers compute keys with the shared [`cache_key`](crate::plan::cache_key)
    /// helper so keying can never drift from the plan cache.
    pub fn publish(&mut self, key: u64, values: Vec<Option<f64>>) -> u64 {
        self.cache.publish(key, values)
    }

    /// One cached pane's revision, if published.
    #[must_use]
    pub fn revision_of(&self, key: u64) -> Option<u64> {
        self.cache.get(key).map(|entry| entry.revision)
    }
}

impl Default for ReplayController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "../tests/replay.rs"]
mod tests;
