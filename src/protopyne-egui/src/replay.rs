//! Terminal replay: the scrub over the standard pane topology.
//!
//! Replay arrives in two pieces that meet here and nowhere else. The
//! **core** publishes the time axis — which bars a run can be scrubbed to,
//! and which of them carry an anchor — as the neutral [`ReplayView`]. The
//! **terminal** owns the compute axis: a [`ReplayController`] over the
//! standard pane topology plus its series cache.
//!
//! A scrub therefore does two separate things, and this module keeps them
//! apart on purpose:
//!
//! - `cursor` is the task-value answer to "what did the strategy see at this
//!   bar". It is an exact integer the core supplied, never derived from
//!   anything the terminal believes.
//! - "which panes recompute" is producer-confidence bookkeeping. Every pane
//!   series is a function of the visible window, so a moved cursor makes the
//!   whole topology due; that set is read from the pane DAG through the
//!   plan's own rewind anchors (the panes with no upstream pane, which is
//!   where a moved window enters the topology) rather than written out by
//!   hand. The cache revisions that settle them say a pane's *content* is
//!   unchanged — never that a trading decision changed.
//!
//! The control renders from [`ReplayState`]'s schema like every other
//! control-plane struct: [`paint_replay_scrub`] lifts that schema through
//! the shared field editor and names no field of its own.

use std::collections::HashSet;
use std::fmt::{Display, Formatter};

use fluent_egui::{paint_schema_fields, schema_fields, EguiError, ReplayController, ReplayState};
use fluent_wvr::Describable;
use protopyne_kernels::indicator::ParamSet;
use protopyne_view::{GatewayError, ReplayView, TerminalGateway};

use crate::topology::{cache_key_for, standard_replay};

/// Why a scrub refused. Every refusal names what was wrong, and none of
/// them leaves a half-applied cursor behind.
#[derive(Debug, Clone, PartialEq)]
pub enum ScrubRefusal {
    /// No window has been read from the gateway yet, so there is nothing to
    /// scrub over.
    NoTimeline,
    /// The gateway's window is not internally consistent: its step list
    /// does not span its own bar count, its cursor runs past its bars, or
    /// its steps are not the bars they claim to be, in order.
    InconsistentWindow(String),
    /// The requested bar is not in the window.
    UnknownBar {
        /// The bar the operator asked for.
        bar: u64,
        /// Bars the window actually spans.
        total_bars: u64,
    },
    /// The pane topology refused the rewind.
    Plan(EguiError),
    /// A pane cannot be settled where it stands: unknown to this topology,
    /// or with inputs that have not been settled yet.
    NotSettleable {
        /// The pane named.
        pane: String,
        /// Why it cannot be settled now.
        reason: String,
    },
    /// The cursor itself refuses the requested state (a blank mark, or a
    /// cursor past the window).
    Cursor(String),
    /// An edited draft tried to resize the window. Bar count is the core's
    /// read, never something a scrub control types over it.
    WindowNotResizable,
}

impl Display for ScrubRefusal {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoTimeline => {
                write!(
                    formatter,
                    "no replay window read yet: nothing to scrub over"
                )
            }
            Self::InconsistentWindow(reason) => {
                write!(formatter, "replay window is inconsistent: {reason}")
            }
            Self::UnknownBar { bar, total_bars } => {
                write!(
                    formatter,
                    "bar {bar} is outside the {total_bars}-bar replay window"
                )
            }
            Self::Plan(error) => write!(formatter, "pane topology refused the rewind: {error}"),
            Self::NotSettleable { pane, reason } => {
                write!(formatter, "pane {pane:?} cannot be settled: {reason}")
            }
            Self::Cursor(reason) => write!(formatter, "replay cursor refused: {reason}"),
            Self::WindowNotResizable => write!(
                formatter,
                "replay window size comes from the core's read: the scrub cannot resize it"
            ),
        }
    }
}

impl std::error::Error for ScrubRefusal {}

/// What one accepted scrub did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScrubOutcome {
    /// The cursor did not move, so no pane was rewound: republishing the
    /// same series backdates every revision instead of bumping one.
    Held {
        /// Bar index the cursor stands at.
        cursor: u64,
    },
    /// The cursor moved; these panes recompute, in plan order.
    Rewound {
        /// Bar index the cursor now stands at.
        cursor: u64,
        /// Panes whose series the move made due, in plan order.
        panes: Vec<String>,
    },
}

/// The terminal's scrub: a core-supplied window, an applied cursor, and
/// the pane replay that cursor drives.
pub struct ReplayScrub {
    controller: ReplayController,
    cursor: ReplayState,
    window: Option<ReplayView>,
    last_refusal: Option<String>,
}

impl ReplayScrub {
    /// A scrub over the standard nine-pane topology.
    pub fn standard() -> Result<Self, EguiError> {
        Ok(Self {
            controller: standard_replay()?,
            cursor: ReplayState::default(),
            window: None,
            last_refusal: None,
        })
    }

    /// Read the window from the gateway and adopt it.
    pub fn ingest(&mut self, gateway: &dyn TerminalGateway) -> Result<(), GatewayError> {
        let view = gateway.replay()?;
        self.apply_window(view)
            .map_err(|refusal| GatewayError::Malformed(refusal.to_string()))
    }

    /// Adopt one window read.
    ///
    /// The window must be internally consistent, because the scrub indexes
    /// its step list by bar: a window whose steps are not the bars they claim
    /// would answer a scrub with another bar's step. The first read also
    /// adopts the core's recorded cursor; later reads only widen the window,
    /// so a repeated read cannot yank an operator's scrub back to the head. A
    /// window that no longer spans the applied cursor refuses whole — the
    /// position moves because the operator moved it, or not at all.
    pub fn apply_window(&mut self, view: ReplayView) -> Result<(), ScrubRefusal> {
        self.last_refusal = None;
        let total = view.total_bars;
        if view.steps.len() as u64 != total {
            return self.refuse(ScrubRefusal::InconsistentWindow(format!(
                "{} steps over {total} bars",
                view.steps.len()
            )));
        }
        for (position, step) in view.steps.iter().enumerate() {
            if step.bar_index != position as u64 {
                return self.refuse(ScrubRefusal::InconsistentWindow(format!(
                    "step at position {position} claims bar {}",
                    step.bar_index
                )));
            }
        }
        if view.cursor > total {
            return self.refuse(ScrubRefusal::InconsistentWindow(format!(
                "cursor {} runs past {total} bars",
                view.cursor
            )));
        }
        let cursor = if self.window.is_none() {
            view.cursor
        } else {
            self.cursor.cursor
        };
        if cursor > total {
            return self.refuse(ScrubRefusal::InconsistentWindow(format!(
                "the applied cursor {cursor} runs past the new {total}-bar window"
            )));
        }
        self.cursor.cursor = cursor;
        self.cursor.total_bars = total;
        if let Err(error) = self.cursor.validate() {
            return self.refuse(ScrubRefusal::Cursor(error.to_string()));
        }
        self.window = Some(view);
        Ok(())
    }

    /// Move the scrub to `bar`.
    ///
    /// The bar must be in the adopted window, where `total_bars` is the head
    /// (one past the last step). A cursor that does not move rewinds nothing,
    /// so a caller can republish and watch every revision backdate. A cursor
    /// that moves rewinds the plan's own anchors and returns the panes the
    /// move made due, in plan order.
    pub fn scrub_to(&mut self, bar: u64) -> Result<ScrubOutcome, ScrubRefusal> {
        self.last_refusal = None;
        let Some(window) = self.window.as_ref() else {
            return self.refuse(ScrubRefusal::NoTimeline);
        };
        let total = window.total_bars;
        if bar > total {
            return self.refuse(ScrubRefusal::UnknownBar {
                bar,
                total_bars: total,
            });
        }
        if bar == self.cursor.cursor {
            return Ok(ScrubOutcome::Held { cursor: bar });
        }
        let anchors = self.rewind_anchors()?;
        // Build and validate the next cursor before touching the plan: a
        // refused cursor must not leave panes un-completed behind it.
        let mut next = self.cursor.clone();
        next.cursor = bar;
        next.total_bars = total;
        // The mark names the terminal's own checkpoint this recompute
        // anchored at — a label on the pane axis. The core's bar anchors
        // stay in the window's steps and are never folded into this name.
        if let Some(anchor) = anchors.first() {
            next.mark.clone_from(anchor);
        }
        if let Err(error) = next.validate() {
            return self.refuse(ScrubRefusal::Cursor(error.to_string()));
        }
        let mut rewound: HashSet<String> = HashSet::new();
        for anchor in &anchors {
            self.controller.mark(anchor).map_err(ScrubRefusal::Plan)?;
            for pane in self
                .controller
                .rewind_to(anchor)
                .map_err(ScrubRefusal::Plan)?
            {
                rewound.insert(pane);
            }
        }
        // Plan order, not rewind order: catch-up waves run in plan order, so
        // the list the caller works through must match it.
        let order = self.controller.plan().order().map_err(ScrubRefusal::Plan)?;
        let panes: Vec<String> = order
            .into_iter()
            .filter(|pane| rewound.contains(pane))
            .collect();
        self.cursor = next;
        Ok(ScrubOutcome::Rewound { cursor: bar, panes })
    }

    /// Apply an edited cursor draft: a moved cursor scrubs, a changed mark
    /// renames, and anything else is nothing at all. Returns the scrub
    /// outcome when the cursor really moved.
    ///
    /// Taking the whole draft is what keeps the control honest: it is the
    /// only path that reads the control's edits, so a field the scrub
    /// cannot act on is refused here rather than dropped where the painter
    /// cannot see it. The mark validates before the cursor moves, so a
    /// draft with both a new cursor and an invalid mark applies neither.
    pub fn apply_draft(
        &mut self,
        draft: &ReplayState,
    ) -> Result<Option<ScrubOutcome>, ScrubRefusal> {
        self.last_refusal = None;
        if draft.total_bars != self.cursor.total_bars {
            return self.refuse(ScrubRefusal::WindowNotResizable);
        }
        if draft.mark != self.cursor.mark {
            let mut next = self.cursor.clone();
            draft.mark.clone_into(&mut next.mark);
            if let Err(error) = next.validate() {
                return self.refuse(ScrubRefusal::Cursor(error.to_string()));
            }
        }
        let mut outcome = None;
        if draft.cursor != self.cursor.cursor {
            outcome = Some(self.scrub_to(draft.cursor)?);
        }
        if draft.mark != self.cursor.mark {
            self.rename(&draft.mark)?;
        }
        Ok(outcome)
    }

    /// Name the position the cursor stands at. A label, not an axis:
    /// renaming rewinds nothing and recomputes nothing.
    pub fn rename(&mut self, mark: &str) -> Result<(), ScrubRefusal> {
        self.last_refusal = None;
        let mut next = self.cursor.clone();
        mark.clone_into(&mut next.mark);
        if let Err(error) = next.validate() {
            return self.refuse(ScrubRefusal::Cursor(error.to_string()));
        }
        self.cursor = next;
        Ok(())
    }

    /// Record one recomputed pane: publish its series under the shared cache
    /// key, then mark it complete — in that order, so a pane is never
    /// complete without the series the frame will blit.
    ///
    /// Republishing bit-identical content backdates, so a re-run that
    /// reproduces its input costs nothing downstream. The pane must be in
    /// this topology with its inputs settled, which is the order the plan's
    /// own ready waves hand out.
    pub fn settle(
        &mut self,
        pane: &str,
        params: &ParamSet,
        values: Vec<Option<f64>>,
    ) -> Result<u64, ScrubRefusal> {
        self.last_refusal = None;
        let plan = self.controller.plan();
        if !plan.contains(pane) {
            return self.refuse(ScrubRefusal::NotSettleable {
                pane: pane.to_owned(),
                reason: "no such pane in this topology".to_owned(),
            });
        }
        if !plan.is_ready(pane) {
            return self.refuse(ScrubRefusal::NotSettleable {
                pane: pane.to_owned(),
                reason: "its inputs are not settled yet".to_owned(),
            });
        }
        let revision = self.controller.publish(cache_key_for(pane, params), values);
        self.controller.complete(pane);
        Ok(revision)
    }

    /// The next catch-up wave: unblocked panes that still need a run, in
    /// plan order.
    #[must_use]
    pub fn ready(&self) -> Vec<String> {
        self.controller.ready()
    }

    /// Whether every pane is computed — the scrub has nothing left to run.
    #[must_use]
    pub fn settled(&self) -> bool {
        self.ready().is_empty()
    }

    /// One pane's cached revision, if it has published a series.
    #[must_use]
    pub fn revision_of(&self, pane: &str, params: &ParamSet) -> Option<u64> {
        self.controller.revision_of(cache_key_for(pane, params))
    }

    /// The applied cursor: the bar on screen, the window it sits in, and the
    /// mark it is named by.
    #[must_use]
    pub fn state(&self) -> &ReplayState {
        &self.cursor
    }

    /// The pane replay the cursor drives (order, readiness, checkpoints,
    /// cache).
    #[must_use]
    pub fn controller(&self) -> &ReplayController {
        &self.controller
    }

    /// The adopted window, if one has been read.
    #[must_use]
    pub fn window(&self) -> Option<&ReplayView> {
        self.window.as_ref()
    }

    /// The last refusal, for a painter to echo. Every refusal overwrites it,
    /// so it names the most recent thing the operator tried.
    #[must_use]
    pub fn last_refusal(&self) -> Option<&str> {
        self.last_refusal.as_deref()
    }

    /// The panes the visible window feeds directly: those with no upstream
    /// pane. A moved window enters the topology through them, so they are
    /// the rewind anchors. Derived from the plan, so a topology that adds,
    /// drops, or moves a root changes its anchors without an edit here.
    fn rewind_anchors(&self) -> Result<Vec<String>, ScrubRefusal> {
        let plan = self.controller.plan();
        let order = plan.order().map_err(ScrubRefusal::Plan)?;
        Ok(order
            .into_iter()
            .filter(|pane| plan.dependencies(pane).is_empty())
            .collect())
    }

    /// Record a refusal and hand it back, so every failing call leaves the
    /// same visible trace.
    fn refuse<T>(&mut self, refusal: ScrubRefusal) -> Result<T, ScrubRefusal> {
        self.last_refusal = Some(refusal.to_string());
        Err(refusal)
    }
}

impl std::fmt::Debug for ReplayScrub {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReplayScrub")
            .field("state", &self.cursor)
            .field(
                "steps",
                &self.window.as_ref().map_or(0, |view| view.steps.len()),
            )
            .field("last_refusal", &self.last_refusal)
            // The controller prints as the cache it holds plus the panes it
            // still owes, so a scrub never dumps pane state into a log.
            .field("panes_pending", &self.ready())
            .field("cache", &self.controller.cache())
            .finish()
    }
}

/// Paint the scrub control: the cursor's own fields, the last refusal, and
/// where the scrub stands. Returns the outcome when the operator moved the
/// cursor.
///
/// The fields come from the cursor's schema through the shared editor, so
/// this painter holds no field list: it reads a typed draft, hands a moved
/// cursor to the scrub, and shows what happened. A refused cursor leaves the
/// applied position untouched, so the next frame shows the position the
/// operator can actually scrub to.
pub fn paint_replay_scrub(ui: &mut egui::Ui, scrub: &mut ReplayScrub) -> Option<ScrubOutcome> {
    ui.heading("Replay");
    let mut draft = scrub.state().clone();
    let fields = schema_fields(&draft.describe());
    let edits = paint_schema_fields(ui, &fields, &mut draft);
    for reason in &edits.refusals {
        ui.colored_label(egui::Color32::RED, reason.clone());
    }
    // The draft is a scratch copy of the applied cursor, so an edit the
    // scrub refuses cannot half-apply, and the field the scrub cannot act on
    // is refused out loud instead of dropped where the operator cannot see it.
    let outcome = match scrub.apply_draft(&draft) {
        Ok(outcome) => outcome,
        Err(refusal) => {
            ui.colored_label(egui::Color32::RED, refusal.to_string());
            None
        }
    };
    let pending = scrub.ready().len();
    let position = format!(
        "bar {} of {}",
        scrub.state().cursor,
        scrub.state().total_bars
    );
    let status = match &outcome {
        Some(ScrubOutcome::Rewound { panes, .. }) => {
            format!("{position} · {} panes recompute", panes.len())
        }
        _ if pending > 0 => format!("{position} · {pending} panes pending"),
        _ => format!("{position} · {}", scrub.state().mark),
    };
    ui.label(status);
    outcome
}

#[cfg(test)]
#[path = "../tests/replay.rs"]
mod tests;
