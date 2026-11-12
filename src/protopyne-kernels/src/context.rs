//! Explicit per-run evaluation context: the singleton killer.
//!
//! A [`RunCtx`] bundles the bar cursor, variable store, validated inputs,
//! and drawings for exactly one evaluation run. Nothing is process-global,
//! so two runs over the same bars cannot observe each other. Hot access uses
//! direct fields and borrow methods (Tier 1); no runtime field lookup happens
//! per bar.

use protopyne_types::{Bar, BarWindow};

use crate::drawing::Drawing;
use crate::inputs::InputSet;
use crate::series::SliceSeries;
use crate::vars::VarStore;

/// One isolated evaluation run over closed bars.
#[derive(Debug)]
/// One isolated evaluation run over closed bars.
pub struct RunCtx<'a> {
    bars: &'a [Bar],
    cursor: usize,
    vars: VarStore,
    inputs: InputSet,
    drawings: Vec<Drawing>,
}

impl<'a> RunCtx<'a> {
    /// Start a run at the first bar.
    #[must_use]
    pub fn new(bars: &'a [Bar], inputs: InputSet) -> Self {
        Self {
            bars,
            cursor: 0,
            vars: VarStore::new(),
            inputs,
            drawings: Vec::new(),
        }
    }

    /// Start a run over a validated window's bars.
    #[must_use]
    pub fn from_window(window: &'a BarWindow, inputs: InputSet) -> Self {
        Self::new(window.bars(), inputs)
    }

    /// Index of the current closed bar.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Total bars in this run.
    #[must_use]
    pub fn bar_count(&self) -> usize {
        self.bars.len()
    }

    /// The current closed bar, if the cursor is still inside the run.
    #[must_use]
    pub fn current(&self) -> Option<&'a Bar> {
        self.bars.get(self.cursor)
    }

    /// Backward-indexed view over the run's bars anchored at the cursor.
    #[must_use]
    pub fn bars_series(&self) -> SliceSeries<'a, Bar> {
        SliceSeries::new(self.bars, self.cursor)
    }

    /// Borrow the variable store.
    #[must_use]
    pub fn vars(&self) -> &VarStore {
        &self.vars
    }

    /// Mutably borrow the variable store.
    pub fn vars_mut(&mut self) -> &mut VarStore {
        &mut self.vars
    }

    /// Borrow the validated inputs.
    #[must_use]
    pub fn inputs(&self) -> &InputSet {
        &self.inputs
    }

    /// Mutably borrow the validated inputs.
    pub fn inputs_mut(&mut self) -> &mut InputSet {
        &mut self.inputs
    }

    /// Drawings emitted so far in this run.
    #[must_use]
    pub fn drawings(&self) -> &[Drawing] {
        &self.drawings
    }

    /// Emit a drawing anchored by the caller (typically at [`cursor`](Self::cursor)).
    pub fn draw(&mut self, drawing: Drawing) {
        self.drawings.push(drawing);
    }

    /// Move to the next closed bar, resetting intrabar state. Past the last
    /// bar [`current`](Self::current) yields `None`; advancing never panics.
    pub fn advance(&mut self) {
        self.cursor = self.cursor.saturating_add(1);
        self.vars.next_bar();
    }
}

#[cfg(test)]
#[path = "../tests/context.rs"]
mod tests;