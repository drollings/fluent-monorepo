//! Run-scoped builtin state: the one owner of every stateful builtin store.
//!
//! Both execution engines share these stores by construction. The tree-walk
//! and the compiled loop still own different *representations* (a merged
//! variable map vs dense columns), but a stateful builtin answers from one
//! history per run: two engines in one run must observe the same `ta.*`
//! kernel state, the same `fixnan` memory, and (through the run's input
//! registry) the same first-call-wins inputs, and the same injected request
//! tapes / strategy dialogue / drawing registry.
//!
//! The owner is created by the run and passed explicitly down the call
//! paths. It is never a static, never a `RefCell`, and never stored on a
//! context object: a run that does not use stateful builtins pays nothing,
//! and two runs can never observe each other.

use super::builtins_ta::TaBridge;
use super::drawing::DrawingState;
use super::eval::InjectedDataset;
use super::request::RequestState;
use super::strategy::StrategyState;
use crate::value::PineValue;
use crate::KernelError;
use std::collections::{HashMap, HashSet};

/// Every builtin store that lives exactly as long as one run.
///
/// Growth discipline: stores are created on first use (`ta`) or on first
/// present value (`fixnan`), so a purely numeric run allocates nothing here.
/// Strategy, drawing, and request state joined this owner with their shared
/// stores; every stateful builtin a run observes has exactly one home.
/// Enum registration rides along too: both engines consult one
/// declare-once table (first execution wins), so a member read on either
/// engine observes the same registrations in mixed runs.
#[derive(Default)]
pub(crate) struct RunBuiltins {
    /// Lazily bound `ta.*` bridge: kernel state and per-site histories.
    ta: Option<TaBridge>,
    /// Per-site `fixnan` memory: the last present value per call-site
    /// shape, replayed through gaps. Keyed exactly like the tree-walk's
    /// historical store so both engines name one site one way.
    fixnan: HashMap<String, Option<PineValue>>,
    /// Headless `strategy.*` capture: declaration capital, the empty-book
    /// answers, and the recorded broker dialogue. One sink per run serves
    /// both engines, so a mixed run interleaves its events in execution
    /// order.
    strategy: StrategyState,
    /// Headless drawing capture: the live object registry, the per-run id
    /// counter, and the lifecycle sink. One registry per run means a
    /// handle minted by either engine stays live for the other.
    drawing: DrawingState,
    /// Injected request tapes for this run: `request.*` serves exactly
    /// these bars and refuses the live fetch when no pair matches.
    request: RequestState,
    /// Registered enum members by enum name: declaration marks on either
    /// engine fill this exactly like the tree-walk's declare-once table
    /// (first execution wins), and member reads on either engine serve
    /// identity handles from it.
    enums: HashMap<String, HashSet<String>>,
}

impl RunBuiltins {
    /// Empty run-scoped store: no bridge, no `fixnan` memory, no tapes.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Run-scoped store carrying the engine's injected tapes. Datasets are
    /// shared by handle, so this copies two small vectors, never a tape.
    #[must_use]
    pub(crate) fn with_requests(
        security: &[InjectedDataset],
        lower_tf: &[InjectedDataset],
    ) -> Self {
        let mut builtins = Self::new();
        builtins.request = RequestState::new(security, lower_tf);
        builtins
    }

    /// Mutably borrow the `ta.*` bridge, binding it on first use.
    ///
    /// Fails only when the shared builtin registry cannot be constructed;
    /// the refusal wording matches the tree-walk's historical one so a
    /// failure reads identically from either engine.
    pub(crate) fn ta_mut(&mut self) -> Result<&mut TaBridge, KernelError> {
        if self.ta.is_none() {
            match TaBridge::new() {
                Ok(bridge) => self.ta = Some(bridge),
                Err(error) => {
                    return Err(KernelError::Eval(format!("ta bridge unavailable: {error}")));
                }
            }
        }
        Ok(self.ta.as_mut().expect("bridge bound above"))
    }

    /// One `fixnan` step: keep the last present value for `site`, replay it
    /// through gaps. Delegates to the shared step both engines already use,
    /// so the store has one implementation and one key discipline.
    pub(crate) fn fixnan(
        &mut self,
        site: &str,
        values: &[Option<PineValue>],
    ) -> Result<Option<PineValue>, KernelError> {
        super::eval::fixnan_call(&mut self.fixnan, site, values)
    }

    /// Borrow the headless strategy capture.
    pub(crate) fn strategy(&self) -> &StrategyState {
        &self.strategy
    }

    /// Mutably borrow the headless strategy capture (declarations and calls
    /// record into it; reads never need the mutable form).
    pub(crate) fn strategy_mut(&mut self) -> &mut StrategyState {
        &mut self.strategy
    }

    /// Mutably borrow the headless drawing capture.
    pub(crate) fn drawing_mut(&mut self) -> &mut DrawingState {
        &mut self.drawing
    }

    /// Borrow this run's injected request tapes.
    pub(crate) fn request(&self) -> &RequestState {
        &self.request
    }

    /// Register one enum's member set on first execution: later
    /// declarations of the same name find the table occupied and keep it,
    /// exactly like the tree-walk's declare-once table — whichever engine
    /// executes the declaration first wins for both.
    pub(crate) fn register_enum(&mut self, enm: String, members: HashSet<String>) {
        self.enums.entry(enm).or_insert(members);
    }

    /// Whether an enum member is registered on this run, on either engine.
    pub(crate) fn enum_member_registered(&self, enm: &str, member: &str) -> bool {
        self.enums
            .get(enm)
            .is_some_and(|members| members.contains(member))
    }
}

#[cfg(test)]
#[path = "../../tests/pine_builtins_state.rs"]
mod state_tests;
