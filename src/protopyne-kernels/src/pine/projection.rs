//! Overlay projection: engine captures become paint-ready primitives.
//!
//! [`project`] converts one [`RunOutcome`] into [`PlotPrimitive`] values in
//! domain coordinates (absolute-bar index plus price). Construct
//! support/refusal is decided solely by the parity matrix
//! ([`parity_for`]): a construct with no matrix standing, or a `Refuse`
//! verdict, refuses with the matrix's own reason — no second
//! construct-support list lives here. Constructs the engine supports but the
//! overlay cannot paint yet (tables first, then candles, bar colors, fills,
//! and lines) refuse with a typed [`ProjectionRefusal::Unsupported`],
//! because dropping a drawing silently would miscompute by omission.
//!
//! Drawing lifecycle replays the engine's own semantics: `.delete` drops the
//! object (absence paints nothing), while an object whose placement, size,
//! or content a setter touched after creation refuses — the projection owns
//! no setter table, so painting the stale creation geometry would
//! miscompute. A repaint-only update (colors, styles, widths, tooltips)
//! leaves the creation geometry paintable and refuses nothing. Drawings with
//! unplaceable geometry (`na` or non-integral bar coordinates) are skipped,
//! matching the reference behavior of drawing nothing for them. A drawing
//! anchored on a warmup hole the run skipped is also skipped — a deliberate
//! divergence from the reference, which could draw it: the hole has no
//! evaluated bar to sit on, and painting it at a neighbouring bar would
//! miscompute by misplacement.

use thiserror::Error;

use super::egui_parity::{parity_for, ParityVerdict};
use super::eval::{
    DrawingArg, DrawingEvent, DrawingEventKind, HlineCapture, PlotCapture, PlotShapeCapture,
    RunOutcome,
};
use crate::value::PineValue;

/// One paint-ready overlay primitive in domain coordinates: an absolute-bar
/// index plus a price. Colors ride no primitive: Pine colors lower to opaque
/// handles with no theme-key mapping, so the paint layer assigns one style
/// key per primitive kind (display-only, documented at the paint site).
#[derive(Debug, Clone, PartialEq)]
pub enum PlotPrimitive {
    /// One `plot()` call site: title plus one slot per bar of the run's
    /// cursor space, in order — warmup bars stay `None` holes, so the series
    /// aligns slot-for-slot with the bar window the run covered. `na` holes
    /// preserved as `None`.
    Series {
        /// Call-site title.
        title: String,
        /// One slot per bar through the last evaluated bar, in order.
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
        /// Absolute-bar index of the marker.
        bar: usize,
        /// Price of the marker.
        price: f64,
    },
    /// One placed `label.new`: bar index, price, and text.
    Label {
        /// Absolute-bar index of the label.
        bar: usize,
        /// Price of the label.
        price: f64,
        /// Label text.
        text: String,
    },
    /// One placed `box.new`: bar-index span plus price span.
    Area {
        /// Left absolute-bar index.
        left: usize,
        /// Top price.
        top: f64,
        /// Right absolute-bar index.
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
    /// The engine captures the construct but the overlay has no primitive
    /// for it yet (tables first): refuse instead of dropping the drawing.
    /// A construct with no parity-matrix standing, or a `Refuse` verdict,
    /// refuses here too, carrying the matrix's own reason.
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

/// Gate one construct through the parity matrix: only constructs with
/// matrix standing project. Every key this projection gates on is covered
/// (`Match` or `Correct`), so the live refusal is the matrix's own — a
/// construct with no standing, or a `Refuse` verdict, refuses as
/// unsupported with the reason attached. This is the only support decision
/// in the projection — there is no second list.
fn gate(construct: &str) -> Result<(), ProjectionRefusal> {
    match parity_for(construct) {
        Some(row) if row.verdict == ParityVerdict::Refuse => Err(ProjectionRefusal::Unsupported {
            construct: construct.to_owned(),
            reason: row.reason,
        }),
        Some(_) => Ok(()),
        None => Err(ProjectionRefusal::Unsupported {
            construct: construct.to_owned(),
            reason: "no parity-matrix standing, refusing instead of miscomputing",
        }),
    }
}

/// Whether a `plot()` call site hid itself: `display.none` lowers to the
/// record token `0` (every other flag combination still shows something —
/// the data window, the price scale, the status line, or the pane), so only
/// an explicit zero hides. An absent argument is the default `display.all`.
fn is_hidden(display: Option<&PineValue>) -> bool {
    matches!(display, Some(PineValue::Int(0)))
}

/// Whether a mutated drawing property only repaints: colors, styles, widths,
/// and tooltips change no placement and no content, so the creation geometry
/// stays paintable. Every other property — placement, size, text content, or
/// anything the engine adds next — keeps the `mutated` flag: painting stale
/// placement or a stale string would miscompute, and an unknown property
/// refuses rather than guessing that it repaints.
fn is_cosmetic(prop: &str) -> bool {
    matches!(
        prop,
        "textcolor" | "style" | "tooltip" | "color" | "bgcolor" | "border_color" | "width"
    )
}

/// The projection's one bar index space: the engine's data cursor, i.e. the
/// absolute bar. Warmup shifts the evaluated window off bar zero, so an
/// evaluated position and the script's `bar_index` disagree by exactly the
/// warmup offset; every primitive speaks the absolute bar, and this is the
/// only map between the two. Built once per projection from the run's
/// snapshots (`RunOutcome::evaluated` positions in order, each carrying its
/// absolute `BarSnapshot.bar`).
struct CursorSpace {
    /// Absolute bar of each evaluated position, in order.
    absolute: Vec<usize>,
}

impl CursorSpace {
    /// Map one run's evaluated positions to their absolute bars.
    fn of(outcome: &RunOutcome) -> Self {
        Self {
            absolute: outcome
                .snapshots
                .iter()
                .map(|snapshot| snapshot.bar)
                .collect(),
        }
    }

    /// Absolute bar of one evaluated position (`None` for a position the run
    /// never evaluated — defensive; engine-built captures always map).
    fn bar_at(&self, position: usize) -> Option<usize> {
        self.absolute.get(position).copied()
    }

    /// Slot count of a cursor-space series: one slot per bar through the
    /// last evaluated bar, so a padded series aligns slot-for-slot with the
    /// bar window the run covered.
    fn len(&self) -> usize {
        self.absolute.last().map_or(0, |last| last + 1)
    }

    /// Whether an absolute bar carries a projected value: evaluated, not a
    /// warmup hole the run skipped. A drawing the script anchored on a hole
    /// has no evaluated bar to sit on, so it stays unplaced like an `na`
    /// coordinate.
    fn projects(&self, bar: usize) -> bool {
        self.absolute.binary_search(&bar).is_ok()
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

/// Lower one captured cell to an absolute-bar index: integers, plus floats
/// that denote exactly one bar. Anything else has no bar to sit on, so the
/// drawing stays unplaced (skipped, like `na` coordinates).
fn bar_index(value: Option<&PineValue>) -> Option<usize> {
    match value {
        Some(PineValue::Int(value)) => usize::try_from(*value).ok(),
        Some(PineValue::Float(value)) if is_whole_bar(*value) => Some(*value as usize),
        _ => None,
    }
}

/// Whether a float denotes exactly one bar: finite, integral, and below
/// 2^63, so the cast widens neither by saturation nor by rounding. Integer
/// arithmetic with a literal widens to float in this engine (`bar_index - 2`
/// reads `Float` while staying integral), so without this the common offset
/// idiom would never place; a fractional float still denotes no bar and
/// stays unplaced.
#[allow(clippy::float_cmp)]
fn is_whole_bar(value: f64) -> bool {
    value.is_finite() && value.trunc() == value && value >= 0.0 && value < 2f64.powi(63)
}

/// Read one drawing coordinate by position. Creation binds the positional
/// prefix by namespace (`drawing_new` refuses a short prefix), so the
/// coordinate is always positional; named extras ride along as properties
/// and never move it.
fn drawing_arg(args: &[DrawingArg], position: usize) -> Option<PineValue> {
    args.iter()
        .filter(|arg| arg.name.is_none())
        .nth(position)
        .and_then(|arg| arg.value.clone())
}

/// One `plot()` call site becomes one cursor-space series (`None` when the
/// site hid itself with `display = display.none` — absence, like an
/// all-`na` level, never a refusal of the whole overlay).
fn series_of(capture: &PlotCapture, space: &CursorSpace) -> Option<PlotPrimitive> {
    if is_hidden(capture.display.as_ref()) {
        return None;
    }
    let mut values = vec![None; space.len()];
    for (position, cell) in capture.values.iter().enumerate() {
        if let Some(bar) = space.bar_at(position) {
            if let Some(slot) = values.get_mut(bar) {
                *slot = number(cell.as_ref());
            }
        }
    }
    Some(PlotPrimitive::Series {
        title: capture.title.clone(),
        values,
    })
}

/// One `plotshape()` call site becomes one marker per resolved cell, each
/// riding the absolute bar of its evaluated position.
fn markers_of(capture: &PlotShapeCapture, space: &CursorSpace) -> Vec<PlotPrimitive> {
    capture
        .values
        .iter()
        .enumerate()
        .filter_map(|(position, value)| {
            let bar = space.bar_at(position)?;
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
                return Err(ProjectionRefusal::VaryingLevel {
                    title: capture.title.clone(),
                });
            }
        }
    }
    Ok(level.map(|price| PlotPrimitive::Level {
        title: capture.title.clone(),
        price,
    }))
}

/// One placed `label.new`, or `None` when its geometry is unplaceable — or
/// when the script anchored it on a warmup hole, which has no projected bar
/// to sit on.
fn label_of(event: &DrawingEvent, space: &CursorSpace) -> Option<PlotPrimitive> {
    let bar = bar_index(drawing_arg(&event.args, 0).as_ref())?;
    if !space.projects(bar) {
        return None;
    }
    let price = number(drawing_arg(&event.args, 1).as_ref())?;
    let Some(PineValue::Str(text)) = drawing_arg(&event.args, 2) else {
        return None;
    };
    Some(PlotPrimitive::Label { bar, price, text })
}

/// One placed `box.new` (`x1`, `y1`, `x2`, `y2` read as left/top/right/
/// bottom), or `None` when its geometry is unplaceable — or when either end
/// sits on a warmup hole.
fn area_of(event: &DrawingEvent, space: &CursorSpace) -> Option<PlotPrimitive> {
    let left = bar_index(drawing_arg(&event.args, 0).as_ref())?;
    let top = number(drawing_arg(&event.args, 1).as_ref())?;
    let right = bar_index(drawing_arg(&event.args, 2).as_ref())?;
    let bottom = number(drawing_arg(&event.args, 3).as_ref())?;
    if !space.projects(left) || !space.projects(right) {
        return None;
    }
    Some(PlotPrimitive::Area {
        left,
        top,
        right,
        bottom,
    })
}

/// One live drawing object replayed from its lifecycle: creation plus
/// mutation/deletion flags, in creation order.
struct DrawingState {
    /// Minting namespace (`label`, `table`, `box`, `line`).
    object: String,
    /// The creation event (geometry source).
    created: DrawingEvent,
    /// A setter touched placement, size, or content after creation.
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
                if let Some(state) = states
                    .iter_mut()
                    .find(|state| state.created.key == event.key)
                {
                    // A repaint leaves the creation geometry paintable; a
                    // move, a resize, new content, an empty property list, or
                    // an unknown write keeps the flag. Empty means no producer
                    // named what changed, so it fails closed as a move.
                    if event.props.is_empty() || event.props.iter().any(|prop| !is_cosmetic(prop)) {
                        state.mutated = true;
                    }
                }
            }
            DrawingEventKind::Deleted => {
                if let Some(state) = states
                    .iter_mut()
                    .find(|state| state.created.key == event.key)
                {
                    state.deleted = true;
                }
            }
        }
    }
    states
}

/// Project replayed drawings: labels and boxes place; deletions drop;
/// placement/content mutations refuse; repaints place the creation
/// geometry; every other namespace refuses as unsupported (tables first).
/// Unplaceable geometry skips, like `na` coordinates.
fn drawings_of(
    events: &[DrawingEvent],
    space: &CursorSpace,
) -> Result<Vec<PlotPrimitive>, ProjectionRefusal> {
    let mut primitives = Vec::new();
    for state in replay_drawings(events) {
        if state.object != "label" && state.object != "box" {
            let construct = format!("{}.new", state.object);
            gate(&construct)?;
            return Err(unprojectable(&construct));
        }
        if state.deleted {
            continue;
        }
        if state.mutated {
            return Err(ProjectionRefusal::Mutated {
                key: state.created.key.clone(),
            });
        }
        gate(&format!("{}.new", state.object))?;
        let placed = if state.object == "label" {
            label_of(&state.created, space)
        } else {
            area_of(&state.created, space)
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
    let space = CursorSpace::of(outcome);
    let mut primitives = Vec::new();
    if !outcome.plots.is_empty() {
        gate("plot")?;
        primitives.extend(
            outcome
                .plots
                .iter()
                .filter_map(|plot| series_of(plot, &space)),
        );
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
        primitives.extend(
            outcome
                .plotshapes
                .iter()
                .flat_map(|shape| markers_of(shape, &space)),
        );
    }
    if !outcome.hlines.is_empty() {
        gate("hline()")?;
        for capture in &outcome.hlines {
            primitives.extend(level_of(capture)?);
        }
    }
    primitives.extend(drawings_of(&outcome.draw_events, &space)?);
    Ok(OverlayProjection { primitives })
}

#[cfg(test)]
#[path = "../../tests/pine_projection.rs"]
mod tests;
