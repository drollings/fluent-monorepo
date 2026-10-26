//! Uniform `Component` envelopes over trigger and filter slots.
//!
//! A slot (`Trigger`, `Filter`) is a pure function of the tape with a stable
//! catalog id. An envelope binds one slot id once — through the existing
//! [`SlotRegistry`] at bind time, never per bar — and exposes the uniform
//! control-plane surface (`name` is the slot id; `depends`/`provides` are
//! empty because slots produce no assets). `execute` evaluates the bound
//! slot against a [`TapeSnapshot`](crate::tape::TapeSnapshot) carried on the
//! `WorkContext` under [`TAPE_KEY`] and returns the verdict as typed
//! `WorkOutput::data`.
//!
//! The envelope never holds bar state: per-bar cursors stay in
//! `SignalInstance`. What is runtime-assignable is the *binding* — which
//! slot id backs this handle — resolved once via [`TriggerUnit::resolve`] or
//! [`FilterUnit::resolve`]. Research slots built outside the catalog wrap
//! directly with an explicit axis via [`TriggerUnit::new`]/[`FilterUnit::new`].
//!
//! Every envelope states its [`GateAxis`](crate::axes::GateAxis). All
//! thirteen catalog slots judge market state (`TaskValue`); the table is
//! explicit per id so a future genuine producer-health gate has a place to
//! differ. Unknown catalog ids refuse at bind time; a slot added to the
//! registry without an axis entry refuses with a named reason instead of
//! silently defaulting.

use std::sync::Arc;

use fluent_wvr::prelude::*;
use serde_json::Value;

use crate::axes::GateAxis;
use crate::proposal::Direction;
use crate::slots::filter::{filters, Admission, Filter};
use crate::slots::trigger::{triggers, Trigger, TriggerVerdict};
use crate::slots::SlotError;
use crate::tape::{Tape, TapeSnapshot};

/// `WorkContext` key carrying the [`TapeSnapshot`](crate::tape::TapeSnapshot)
/// a slot envelope evaluates.
pub const TAPE_KEY: &str = "tape";

fn missing_tape(name: &str) -> WorkError {
    WorkError::Dependency(format!("{name} needs a tape snapshot under \"{TAPE_KEY}\""))
}

/// The [`GateAxis`](crate::axes::GateAxis) of a catalog trigger id.
///
/// Every catalog trigger is a market-state thesis (`TaskValue`). A trigger
/// added to the registry without an entry here refuses at bind time.
fn trigger_axis(id: &'static str) -> Result<GateAxis, SlotError> {
    match id {
        "momentum" | "ribbon_flip" | "osc_cross" | "exhaustion" | "dual_exhaustion" => {
            Ok(GateAxis::TaskValue)
        }
        _ => Err(SlotError::invalid(
            id,
            "trigger has no documented axis; add one before it can back an envelope",
        )),
    }
}

/// The [`GateAxis`](crate::axes::GateAxis) of a catalog filter id.
///
/// Every catalog filter judges market state (`TaskValue`), including the
/// health filters — low health is a fact about the market, not doubt in the
/// producer. Producer doubt lives in the refusal paths (unwarmed legs,
/// unobserved facts), never in these gates.
fn filter_axis(id: &'static str) -> Result<GateAxis, SlotError> {
    match id {
        "ribbon" | "ma_health" | "ribbon_health" | "slow_side" | "delta_side" | "volume_min"
        | "atr_distance_max" | "regime_skip" => Ok(GateAxis::TaskValue),
        _ => Err(SlotError::invalid(
            id,
            "filter has no documented axis; add one before it can back an envelope",
        )),
    }
}

fn snapshot_of<'a>(ctx: &'a WorkContext, name: &str) -> Result<Tape<'a>, WorkError> {
    let snapshot = ctx
        .get::<TapeSnapshot>(TAPE_KEY)
        .ok_or_else(|| missing_tape(name))?;
    snapshot.view().ok_or_else(|| {
        WorkError::Execution(format!("{name}: tape snapshot cursor is out of bounds"))
    })
}

/// A uniform handle over one trigger slot.
pub struct TriggerUnit {
    id: &'static str,
    trigger: Arc<dyn Trigger>,
    axis: GateAxis,
}

impl TriggerUnit {
    /// Bind a catalog trigger id with its build parameters. Resolution
    /// happens once, here; unknown ids and axis-less ids refuse.
    ///
    /// # Errors
    ///
    /// [`SlotError`] when the id is unknown, its parameters refuse, or it
    /// has no documented axis.
    pub fn resolve(id: &'static str, params: &Value) -> Result<Self, SlotError> {
        let axis = trigger_axis(id)?;
        Ok(Self {
            id,
            trigger: triggers().build(id, params)?,
            axis,
        })
    }

    /// Wrap a trigger built outside the catalog (research path) with an
    /// explicitly stated axis. The id is the trigger's own.
    #[must_use]
    pub fn new(trigger: Arc<dyn Trigger>, axis: GateAxis) -> Self {
        Self {
            id: trigger.id(),
            trigger,
            axis,
        }
    }

    /// The axis this envelope judges on.
    #[must_use]
    pub fn axis(&self) -> GateAxis {
        self.axis
    }

    /// Evaluate the bound slot directly, without a context round-trip.
    #[must_use]
    pub fn eval(&self, tape: &Tape<'_>) -> TriggerVerdict {
        self.trigger.eval(tape)
    }
}

impl WorkUnit for TriggerUnit {
    fn name(&self) -> &str {
        self.id
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        let tape = snapshot_of(ctx, self.id)?;
        let verdict = self.trigger.eval(&tape);
        WorkOutput::typed("trigger verdict", &verdict)
    }
}

impl_fieldless!(TriggerUnit);

impl Describable for TriggerUnit {
    fn describe(&self) -> serde_json::Value {
        fluent_wvr::describe_work_unit(self.id, &[], &[], None)
    }
}

impl_component!(TriggerUnit);

/// A uniform handle over one filter slot.
pub struct FilterUnit {
    id: &'static str,
    filter: Arc<dyn Filter>,
    axis: GateAxis,
    direction: Direction,
}

impl FilterUnit {
    /// Bind a catalog filter id with its build parameters and the side it
    /// admits. Resolution happens once, here; unknown ids and axis-less ids
    /// refuse.
    ///
    /// # Errors
    ///
    /// [`SlotError`] when the id is unknown, its parameters refuse, or it
    /// has no documented axis.
    pub fn resolve(
        id: &'static str,
        params: &Value,
        direction: Direction,
    ) -> Result<Self, SlotError> {
        let axis = filter_axis(id)?;
        Ok(Self {
            id,
            filter: filters().build(id, params)?,
            axis,
            direction,
        })
    }

    /// Wrap a filter built outside the catalog (research path) with an
    /// explicitly stated axis and side. The id is the filter's own.
    #[must_use]
    pub fn new(filter: Arc<dyn Filter>, axis: GateAxis, direction: Direction) -> Self {
        Self {
            id: filter.id(),
            filter,
            axis,
            direction,
        }
    }

    /// The axis this envelope judges on.
    #[must_use]
    pub fn axis(&self) -> GateAxis {
        self.axis
    }

    /// Evaluate the bound slot directly, without a context round-trip.
    #[must_use]
    pub fn eval(&self, tape: &Tape<'_>) -> Admission {
        self.filter.admit(tape, self.direction)
    }
}

impl WorkUnit for FilterUnit {
    fn name(&self) -> &str {
        self.id
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        let tape = snapshot_of(ctx, self.id)?;
        let admission = self.filter.admit(&tape, self.direction);
        WorkOutput::typed("filter admission", &admission)
    }
}

impl_fieldless!(FilterUnit);

impl Describable for FilterUnit {
    fn describe(&self) -> serde_json::Value {
        fluent_wvr::describe_work_unit(self.id, &[], &[], None)
    }
}

impl_component!(FilterUnit);

#[cfg(test)]
#[path = "../../tests/envelope_pins.rs"]
mod envelope_pins_tests;
