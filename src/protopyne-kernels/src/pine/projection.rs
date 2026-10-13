//! Overlay projection: engine captures become paint-ready primitives.
//!
//! [`project`] converts one [`RunOutcome`] into [`PlotPrimitive`] values in
//! domain coordinates (evaluated-bar index plus price). Construct
//! support/refusal is decided solely by the parity matrix
//! ([`parity_for`]): a construct with no matrix standing, or a `Refuse`
//! verdict, refuses with the matrix's own reason — no second
//! construct-support list lives here. Constructs the engine supports but the
//! overlay cannot paint yet (tables first, then candles, bar colors, fills,
//! and lines) refuse with a typed [`ProjectionRefusal::Unsupported`],
//! because dropping a drawing silently would miscompute by omission.
//!
//! Drawing lifecycle replays the engine's own semantics: `.delete` drops the
//! object (absence paints nothing), while an object mutated after creation
//! refuses — the projection owns no setter table, so painting the stale
//! creation geometry would miscompute. Drawings with unplaceable geometry
//! (`na` or non-integral bar coordinates) are skipped, matching the
//! reference behavior of drawing nothing for them.

use thiserror::Error;

use super::egui_parity::{ParityVerdict, parity_for};
use super::eval::{
    DrawingArg, DrawingEvent, DrawingEventKind, HlineCapture, PlotCapture, PlotShapeCapture,
    RunOutcome,
};
use crate::value::PineValue;

/// One paint-ready overlay primitive in domain coordinates: an evaluated-bar
/// index plus a price. Colors ride no primitive: Pine colors lower to opaque
/// handles with no theme-key mapping, so the paint layer assigns one style
/// key per primitive kind (display-only, documented at the paint site).
#[derive(Debug, Clone, PartialEq)]
pub enum PlotPrimitive {
    /// One `plot()` call site: title plus one value per evaluated bar,
    /// `na` holes preserved as `None`.
    Series {
        /// Call-site title.
        title: String,
        /// One value per evaluated bar, in order.
        values: Vec<Option<f64>>,
    },
    /// One constant `hline()`: title plus its single level.
    Level {
        /// Call-site title.
        title: String,
        /// The agreed level.
        price: f64,
    },
    /// One `plotshape()` marker: title plus its placed bar and price.
    Marker {
        /// Call-site title.
        title: String,
        /// Evaluated-bar index of the marker.
        bar: usize,
        /// Price of the marker.
        price: f64,
    },
    /// One placed `label.new`: bar index, price, and text.
    Label {
        /// Evaluated-bar index of the label.
        bar: usize,
        /// Price of the label.
        price: f64,
        /// Label text.
        text: String,
    },
    /// One placed `box.new`: bar-index span plus price span.
    Area {
        /// Left evaluated-bar index.
        left: usize,
        /// Top price.
        top: f64,
        /// Right evaluated-bar index.
        right: usize,
        /// Bottom price.
        bottom: f64,
    },
}

/// One projected run: primitives in capture order (plots, shapes, levels,
/// then drawings in creation order).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OverlayProjection {
    /// Projected primitives, in capture order.
    pub primitives: Vec<PlotPrimitive>,
}

/// Why a run has no overlay projection. Every variant refuses instead of
/// miscomputing; the paint layer renders the reason as an "unsupported" chip.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum ProjectionRefusal {
    /// No parity-matrix standing for the construct: unknown ground, refuse.
    #[error("unknown construct {construct:?}: no parity-matrix standing, refusing instead of miscomputing")]
    UnknownConstruct {
        /// Construct name the gate looked up.
        construct: String,
    },
    /// The matrix itself refuses the construct, with its reason.
    #[error("parity matrix refuses {construct:?}: {reason}")]
    ParityRefused {
        /// Construct name the gate looked up.
        construct: String,
        /// Matrix refusal reason.
        reason: &'static str,
    },
    /// The engine captures the construct but the overlay has no primitive
    /// for it yet (tables first): refuse instead of dropping the drawing.
    #[error("unsupported construct {construct:?}: {reason}")]
    Unsupported {
        /// Construct name (`table.new`, `plotcandle`, ...).
        construct: String,
        /// What the overlay cannot paint yet.
        reason: &'static str,
    },
    /// A level that moves bar to bar has no single level to paint.
    #[error("varying hline {title:?} has no level primitive")]
    VaryingLevel {
        /// Call-site title of the moving level.
        title: String,
    },
    /// A drawing mutated after creation has no single placed state the
    /// projection owns: refuse instead of painting stale geometry.
    #[error("drawing {key:?} mutated after creation stays unprojected")]
    Mutated {
        /// Stable object key (`label#3`).
        key: String,
    },
}

/// Gate one construct through the parity matrix: unknown constructs and
/// `Refuse` verdicts refuse with the matrix's own standing. This is the only
/// support decision in the projection — there is no second list.
fn gate(construct: &str) -> Result<(), ProjectionRefusal> {
    match parity_for(construct) {
        None => Err(ProjectionRefusal::UnknownConstruct { construct: construct.to_owned() }),
        Some(row) if row.verdict == ParityVerdict::Refuse => Err(ProjectionRefusal::ParityRefused {
            construct: construct.to_owned(),
            reason: row.reason,
        }),
        Some(_) => Ok(()),
    }
}

/// Lower one captured cell to a domain number: `na`, non-finite floats, and
/// non-numeric values are holes (`None`), never substituted values.
fn number(value: Option<&PineValue>) -> Option<f64> {
    match value {
        Some(PineValue::Float(value)) if value.is_finite() => Some(*value),
        Some(PineValue::Int(value)) => Some(*value as f64),
        _ => None,
    }
}

/// Lower one captured cell to an evaluated-bar index: integral,
/// non-negative integers only. Anything else has no bar to sit on, so the
/// drawing stays unplaced (skipped, like `na` coordinates).
fn bar_index(value: Option<&PineValue>) -> Option<usize> {
    match value {
        Some(PineValue::Int(value)) => usize::try_from(*value).ok(),
        _ => None,
    }
}

/// Read one drawing coordinate by position with a named fallback, so
/// `label.new(x, y, text)` and `label.new(x = .., y = .., text = ..)` agree.
/// A present-but-`na` positional stays `na`; the named form only fills a
/// missing position.
fn drawing_arg(args: &[DrawingArg], position: usize, name: &str) -> Option<PineValue> {
    if let Some(arg) = args.iter().filter(|arg| arg.name.is_none()).nth(position) {
        return arg.value.clone();
    }
    args.iter()
        .find(|arg| arg.name.as_deref() == Some(name))
        .and_then(|arg| arg.value.clone())
}

/// One `plot()` call site becomes one series; holes preserved.
fn series_of(capture: &PlotCapture) -> PlotPrimitive {
    PlotPrimitive::Series {
        title: capture.title.clone(),
        values: capture.values.iter().map(|cell| number(cell.as_ref())).collect(),
    }
}

/// One `plotshape()` call site becomes one marker per resolved cell.
fn markers_of(capture: &PlotShapeCapture) -> Vec<PlotPrimitive> {
    capture
        .values
        .iter()
        .enumerate()
        .filter_map(|(bar, value)| {
            number(value.as_ref()).map(|price| PlotPrimitive::Marker {
                title: capture.title.clone(),
                bar,
                price,
            })
        })
        .collect()
}

/// One `hline()` becomes one level when every resolved leg agrees
/// (bit-identical: the question is collapse, not tolerance); a moving level
/// refuses, and an all-`na` level draws nothing.
fn level_of(capture: &HlineCapture) -> Result<Option<PlotPrimitive>, ProjectionRefusal> {
    let mut level: Option<f64> = None;
    for value in capture.values.iter().map(|cell| number(cell.as_ref())) {
        match (level, value) {
            (_, None) => {}
            (None, Some(price)) => level = Some(price),
            (Some(seen), Some(price)) if seen.to_bits() == price.to_bits() => {}
            (Some(_), Some(_)) => {
                return Err(ProjectionRefusal::VaryingLevel { title: capture.title.clone() });
            }
        }
    }
    Ok(level.map(|price| PlotPrimitive::Level { title: capture.title.clone(), price }))
}

/// One placed `label.new`, or `None` when its geometry is unplaceable.
fn label_of(event: &DrawingEvent) -> Option<PlotPrimitive> {
    let bar = bar_index(drawing_arg(&event.args, 0, "x").as_ref())?;
    let price = number(drawing_arg(&event.args, 1, "y").as_ref())?;
    let Some(PineValue::Str(text)) = drawing_arg(&event.args, 2, "text") else {
        return None;
    };
    Some(PlotPrimitive::Label { bar, price, text })
}

/// One placed `box.new` (`x1`, `y1`, `x2`, `y2` read as left/top/right/
/// bottom), or `None` when its geometry is unplaceable.
fn area_of(event: &DrawingEvent) -> Option<PlotPrimitive> {
    let left = bar_index(drawing_arg(&event.args, 0, "x1").as_ref())?;
    let top = number(drawing_arg(&event.args, 1, "y1").as_ref())?;
    let right = bar_index(drawing_arg(&event.args, 2, "x2").as_ref())?;
    let bottom = number(drawing_arg(&event.args, 3, "y2").as_ref())?;
    Some(PlotPrimitive::Area { left, top, right, bottom })
}

/// One live drawing object replayed from its lifecycle: creation plus
/// mutation/deletion flags, in creation order.
struct DrawingState {
    /// Minting namespace (`label`, `table`, `box`, `line`).
    object: String,
    /// The creation event (geometry source).
    created: DrawingEvent,
    /// A setter touched the object after creation.
    mutated: bool,
    /// `.delete` removed the object.
    deleted: bool,
}

/// Replay the lifecycle log into per-object states in creation order.
/// Unknown namespaces ride along: the projection step refuses them.
fn replay_drawings(events: &[DrawingEvent]) -> Vec<DrawingState> {
    let mut states: Vec<DrawingState> = Vec::new();
    for event in events {
        match event.kind {
            DrawingEventKind::Created => states.push(DrawingState {
                object: event.object.clone(),
                created: event.clone(),
                mutated: false,
                deleted: false,
            }),
            DrawingEventKind::Updated => {
                if let Some(state) = states.iter_mut().find(|state| state.created.key == event.key) {
                    state.mutated = true;
                }
            }
            DrawingEventKind::Deleted => {
                if let Some(state) = states.iter_mut().find(|state| state.created.key == event.key) {
                    state.deleted = true;
                }
            }
        }
    }
    states
}

/// Project replayed drawings: labels and boxes place; deletions drop;
/// mutations refuse; every other namespace refuses as unsupported (tables
/// first). Unplaceable geometry skips, like `na` coordinates.
fn drawings_of(events: &[DrawingEvent]) -> Result<Vec<PlotPrimitive>, ProjectionRefusal> {
    let mut primitives = Vec::new();
    for state in replay_drawings(events) {
        if state.object != "label" && state.object != "box" {
            gate(&format!("{}.new", state.object))?;
            return Err(ProjectionRefusal::Unsupported {
                construct: format!("{}.new", state.object),
                reason: "no overlay primitive yet; refusing instead of dropping the drawing",
            });
        }
        if state.deleted {
            continue;
        }
        if state.mutated {
            return Err(ProjectionRefusal::Mutated { key: state.created.key.clone() });
        }
        gate(&format!("{}.new", state.object))?;
        let placed = if state.object == "label" {
            label_of(&state.created)
        } else {
            area_of(&state.created)
        };
        primitives.extend(placed);
    }
    Ok(primitives)
}

/// Refusal for an engine-supported capture group with no overlay primitive.
fn unprojectable(construct: &str) -> ProjectionRefusal {
    ProjectionRefusal::Unsupported {
        construct: construct.to_owned(),
        reason: "no overlay primitive yet; refusing instead of dropping the drawing",
    }
}

/// Project one engine run into overlay primitives, in capture order (plots,
/// shapes, levels, then drawings in creation order). Any unsupported
/// construct refuses the whole projection with its name on the reason.
pub fn project(outcome: &RunOutcome) -> Result<OverlayProjection, ProjectionRefusal> {
    let mut primitives = Vec::new();
    if !outcome.plots.is_empty() {
        gate("plot")?;
        primitives.extend(outcome.plots.iter().map(series_of));
    }
    if !outcome.plotcandles.is_empty() {
        gate("plotcandle")?;
        return Err(unprojectable("plotcandle"));
    }
    if !outcome.barcolors.is_empty() {
        gate("barcolor")?;
        return Err(unprojectable("barcolor"));
    }
    if !outcome.fills.is_empty() {
        gate("fill")?;
        return Err(unprojectable("fill"));
    }
    if !outcome.plotshapes.is_empty() {
        gate("plotshape()")?;
        primitives.extend(outcome.plotshapes.iter().flat_map(markers_of));
    }
    if !outcome.hlines.is_empty() {
        gate("hline()")?;
        for capture in &outcome.hlines {
            primitives.extend(level_of(capture)?);
        }
    }
    primitives.extend(drawings_of(&outcome.draw_events)?);
    Ok(OverlayProjection { primitives })
}

#[cfg(test)]
#[path = "../../tests/pine_projection.rs"]
mod tests;
