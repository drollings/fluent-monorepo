//! Pine drawings enter the operator's annotation store — and nothing else.
//!
//! [`annotations_of`] converts one successful projection's `Label` and `Area`
//! primitives to [`Annotation`] values: the bar index resolves to the bar's
//! close time through the same bar window the engine ran over, and the price
//! lowers to its exact decimal. [`append_projection`] appends that conversion
//! to the terminal's [`AnnotationSet`]. This is the VISION §8
//! Pine-portable/native boundary made concrete: Pine output enters the
//! operator's store and is thereafter undoable like any other annotation.
//!
//! Only discrete drawings journal. `Label` becomes one annotation carrying
//! its text; `Area` becomes its two corner annotations (a span has no
//! single-key encoding, and the primitive carries no text, so corners ride
//! empty labels). Series, levels, and markers do not convert: they are
//! continuous script output that repaints every frame from the projection,
//! and journaling every plotted point would flood the store.
//!
//! # The append-only conflict, decided
//!
//! The store refuses in-place edits by design and the projection refuses
//! geometric moves before conversion, so a Pine move (a) never reaches this
//! bridge: the run refuses, nothing appends, and only operator edits flow
//! through the bounded undo log. A repaint-only update passes through as
//! creation geometry and converts normally. The alternative — a move becomes
//! a new append — would need the projection to paint moved drawings instead
//! of refusing them, un-landing the fail-closed guarantee built for the
//! projection; leaving the choice unmade would let a move silently vanish.
//! Recorded here because this bridge is the only place the two systems meet.
//!
//! # Refresh policy
//!
//! Conversion is idempotent per content: [`append_new`] appends only
//! annotations not already live, so re-running an unchanged script is a
//! no-op instead of duplicating the journal. New drawings append; stale ones
//! stay published (never silently dropped, like every other retired entry).
//! One edge follows from the rule and is documented rather than special-
//! cased: undoing a Pine annotation and re-running the script re-appends it,
//! because the script still emits it.

use fluent_egui::AnnotationSet;
use protopyne_chart_model::{Annotation, AnnotationKey, AnnotationStyle};
use protopyne_kernels::PlotPrimitive;
use protopyne_types::Bar;

/// Convert one successful projection's drawings to annotations: labels keep
/// their text, areas become corner pairs, everything else is skipped.
/// Primitives anchored outside the bar window, or carrying a price no
/// decimal can hold, drop — the projection this bridge consumes already
/// guarantees finite prices over evaluated bars, so a drop here names a
/// hand-built or misaligned input, never a quiet substitution.
#[must_use]
pub fn annotations_of(primitives: &[PlotPrimitive], bars: &[Bar]) -> Vec<Annotation> {
    let mut annotations = Vec::new();
    for primitive in primitives {
        match primitive {
            PlotPrimitive::Label { bar, price, text } => {
                if let Some(annotation) = note_at(*bar, *price, text.clone(), bars) {
                    annotations.push(annotation);
                }
            }
            PlotPrimitive::Area { left, top, right, bottom } => {
                if let Some(corner) = note_at(*left, *top, String::new(), bars) {
                    annotations.push(corner);
                }
                if let Some(corner) = note_at(*right, *bottom, String::new(), bars) {
                    annotations.push(corner);
                }
            }
            PlotPrimitive::Series { .. } | PlotPrimitive::Level { .. } | PlotPrimitive::Marker { .. } => {}
        }
    }
    annotations
}

/// Append converted drawings to the terminal's set, skipping annotations
/// already live. Returns the appended count, so a caller can tell a fresh
/// script (new entries) from a re-run (zero). Split from [`annotations_of`]
/// so a caller holding a bar-window borrow can convert first and append
/// after the borrow ends.
pub fn append_new(set: &mut AnnotationSet, annotations: &[Annotation]) -> usize {
    let mut appended = 0;
    for annotation in annotations {
        if !set.iter().any(|live| live == annotation) {
            set.append(annotation.clone());
            appended += 1;
        }
    }
    appended
}

/// One drawing point as an annotation: the bar's close time plus the exact
/// decimal price. `None` when the bar is outside the window or the price is
/// unrepresentable — both drop, never substitute.
fn note_at(bar: usize, price: f64, label: String, bars: &[Bar]) -> Option<Annotation> {
    let timestamp = bars.get(bar)?.ts.timestamp();
    Some(Annotation {
        key: AnnotationKey::from_display(timestamp, price)?,
        label,
        style: AnnotationStyle::Note,
    })
}

#[cfg(test)]
#[path = "../tests/annotations.rs"]
mod tests;
