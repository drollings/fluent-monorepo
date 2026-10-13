//! Append-only annotations with bounded undo — and nothing else.
//!
//! [`AnnotationSet`] is the render-side journal: annotations append, never
//! edit in place, and only the most recent [`MAX_UNDO_DEPTH`] appends
//! retract (older entries stay published). Undo is concrete over this one
//! type — journal and order types get no history impl, pinned by the
//! compile-fail case in `tests/ui/no_generic_undo.rs`. Resolution
//! ([`resolve_annotation`]) maps domain keys to pane pixels per frame; the
//! key never moves when the pixels do. The set moves [`Annotation`] values
//! opaquely and only ever reads display floats — exact prices never enter
//! this crate, keeping the one-way display boundary.

use protopyne_chart_model::{Annotation, ChartRow};

use crate::viewport::Viewport;

/// How many recent appends stay retractable (task value: operator
/// forgiveness). Older entries stay published; only the undo window is
/// bounded, never the set itself.
pub const MAX_UNDO_DEPTH: usize = 32;

/// Append-only annotation store with bounded undo.
///
/// Published entries are never edited or reordered; [`undo`](Self::undo)
/// retracts the most recent append into a bounded retraction log. There is
/// no redo: a retracted annotation re-enters only via [`append`](Self::append).
#[derive(Debug, Clone, Default)]
pub struct AnnotationSet {
    live: Vec<Annotation>,
    undone: Vec<Annotation>,
}

impl AnnotationSet {
    /// Empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish one annotation at the end of the set.
    pub fn append(&mut self, annotation: Annotation) {
        self.live.push(annotation);
    }

    /// Retract the most recent append, or `None` when the set is empty or
    /// the retraction log already holds [`MAX_UNDO_DEPTH`] entries.
    pub fn undo(&mut self) -> Option<Annotation> {
        if self.undone.len() >= MAX_UNDO_DEPTH {
            return None;
        }
        let annotation = self.live.pop()?;
        self.undone.push(annotation.clone());
        Some(annotation)
    }

    /// Published entry count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.live.len()
    }

    /// Whether nothing is published.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    /// Published annotations in append order.
    pub fn iter(&self) -> std::slice::Iter<'_, Annotation> {
        self.live.iter()
    }
}

impl<'a> IntoIterator for &'a AnnotationSet {
    type Item = &'a Annotation;
    type IntoIter = std::slice::Iter<'a, Annotation>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Resolve one annotation to pane-local pixels: the key's timestamp names
/// the bar index in the passed window, the key's price maps through the
/// viewport. `None` when the bar sits outside the window or the price is
/// unmappable — markers decorate published bars only, never neighbors.
#[must_use]
pub fn resolve_annotation(
    annotation: &Annotation,
    rows: &[ChartRow],
    viewport: &Viewport,
) -> Option<(f32, f32)> {
    let index = rows.iter().position(|row| row.time == annotation.key.bar_timestamp)?;
    let y = viewport.price_to_y(annotation.key.price_float()?)?;
    Some((viewport.index_to_x(index), y))
}

#[cfg(test)]
#[path = "../tests/history.rs"]
mod tests;
