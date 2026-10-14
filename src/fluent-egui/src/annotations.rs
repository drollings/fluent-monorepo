//! Append-only annotations with bounded undo — and nothing else.
//!
//! [`AnnotationSet`] is the render-side journal: annotations append, never
//! edit in place, and only the most recent [`MAX_UNDO_DEPTH`] appends
//! retract (older entries stay published). Appends arrive from two places —
//! the operator (click-to-annotate through [`annotate_at`]) and the terminal
//! bridge that converts Pine drawings after a successful projection — and
//! both are plain appends: a Pine move has no home here because the
//! projection refuses it before conversion, so only operator edits ever flow
//! through the bounded undo log. Undo is concrete over this one type —
//! journal and order types get no history impl, pinned by the compile-fail
//! case in `tests/ui/no_generic_undo.rs`. Resolution ([`resolve_annotation`])
//! maps domain keys to pane pixels per frame through one [`TimeIndex`]; the
//! key never moves when the pixels do. The set moves [`Annotation`] values
//! opaquely and only ever reads display floats — exact prices never enter
//! this crate, keeping the one-way display boundary.

use std::collections::HashMap;

use protopyne_chart_model::{Annotation, AnnotationKey, AnnotationStyle, ChartRow};

use crate::hittest::hit_test;
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
/// viewport. `None` when the bar sits outside the window, when its timestamp
/// repeats in the window (duplicates bind nothing, never the first match),
/// or when the price is unmappable — markers decorate published bars only,
/// never neighbors.
#[must_use]
pub fn resolve_annotation(
    annotation: &Annotation,
    rows: &[ChartRow],
    viewport: &Viewport,
) -> Option<(f32, f32)> {
    TimeIndex::of(rows).resolve(annotation, viewport)
}

/// One frame's timestamp-to-bar map, built once.
///
/// The batch path ([`crate::paint::marker_dots`]) builds one of these per
/// frame and resolves every annotation through it instead of scanning the
/// window per marker. A timestamp with exactly one row maps to that row; a
/// timestamp with none — or with more than one — maps to nothing. Duplicate
/// timestamps are a data fault (two bars claiming one close time), and
/// binding either row would decorate a bar the key does not name, so both
/// stay unpainted rather than silently binding the first match.
#[derive(Debug, Clone, Default)]
pub struct TimeIndex {
    /// Timestamps with exactly one row, plus their bar index.
    unique: HashMap<i64, usize>,
}

impl TimeIndex {
    /// Map one window's timestamps. Duplicate timestamps bind nothing.
    #[must_use]
    pub fn of(rows: &[ChartRow]) -> Self {
        let mut unique: HashMap<i64, usize> = HashMap::with_capacity(rows.len());
        let mut duplicated: HashMap<i64, ()> = HashMap::new();
        for (index, row) in rows.iter().enumerate() {
            if duplicated.contains_key(&row.time) {
                continue;
            }
            if unique.remove(&row.time).is_some() {
                duplicated.insert(row.time, ());
            } else {
                unique.insert(row.time, index);
            }
        }
        Self { unique }
    }

    /// Bar index of one timestamp: `None` when the bar sits outside the
    /// window or its timestamp repeats in the window.
    #[must_use]
    pub fn lookup(&self, timestamp: i64) -> Option<usize> {
        self.unique.get(&timestamp).copied()
    }

    /// Resolve one annotation through this map: the key's timestamp names
    /// the bar, the key's price maps through the viewport. `None` under the
    /// same conditions as [`resolve_annotation`].
    #[must_use]
    pub fn resolve(&self, annotation: &Annotation, viewport: &Viewport) -> Option<(f32, f32)> {
        let index = self.lookup(annotation.key.bar_timestamp)?;
        let y = viewport.price_to_y(annotation.key.price_float()?)?;
        Some((viewport.index_to_x(index), y))
    }
}

/// Click-to-annotate: a radius-confirmed pointer hit becomes an operator
/// annotation, or `None` when the click names no bar.
///
/// The hit radius answers "did the operator mean this candle" — doubt about
/// intent, never about which bar is right. The candidate bar comes from the
/// pointer's slot alone ([`hit_test`]); the radius only decides whether that
/// candidate counts. Once a hit returns, the bar's data fixes the key
/// (close time plus close price) and the radius has no further say. The
/// radius never selects the bar directly and never gates whether a bar is
/// annotatable — Pine-converted and programmatic appends bypass it
/// entirely. A click whose bar has no representable price (non-finite close)
/// creates nothing instead of pinning a substituted value.
#[must_use]
pub fn annotate_at(
    pointer: (f32, f32),
    viewport: &Viewport,
    rows: &[ChartRow],
) -> Option<Annotation> {
    let hit = hit_test(pointer, viewport, rows)?;
    let row = rows.get(hit.index)?;
    Some(Annotation {
        key: AnnotationKey::from_display(row.time, row.close)?,
        label: String::new(),
        style: AnnotationStyle::Note,
    })
}

#[cfg(test)]
#[path = "../tests/history.rs"]
mod tests;
