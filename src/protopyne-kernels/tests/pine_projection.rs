//! Projection goldens: `RunOutcome` captures become `PlotPrimitive` values in
//! domain coordinates (bar index plus price), gated solely by the parity
//! matrix — no second construct-support list lives here.
//!
//! Representative `plot`/`label`/`box` programs project golden geometry; a
//! table construct refuses with a typed reason (the engine captures tables,
//! the overlay has no table primitive); `parity_for` stays consistent with
//! the matrix rows the gate reads.
//!
//! Multi-line drivers below carry explicit `\n    ` indents: Rust `\`
//! continuations strip leading whitespace, which would drop guarded bodies
//! to column zero.
//!
//! Exact float comparisons below are over exact small integral prices and
//! counters, never measured quantities.
//! `#[allow(clippy::float_cmp)]` covers those literals.
#![allow(clippy::float_cmp)]

use super::*;
use crate::{InputSet, PineEngine, PineRuntimeConfig, parse_program};
use chrono::TimeZone as _;
use protopyne_types::Bar;

fn bar_at(index: i64) -> Bar {
    Bar::new()
        .ts(
            chrono::Utc
                .timestamp_opt(1_700_000_000 + index * 60, 0)
                .single()
                .expect("valid test timestamp"),
        )
        .o("100.00".parse().expect("decimal"))
        .h("101.00".parse().expect("decimal"))
        .l("99.00".parse().expect("decimal"))
        .c(format!("{}.00", 100 + index).parse().expect("decimal"))
        .volume(1_000_u64)
        .maybe_adj(None)
        .build()
}

fn five_bars() -> Vec<Bar> {
    (0..5).map(bar_at).collect()
}

fn project_script(script: &str) -> Result<OverlayProjection, ProjectionRefusal> {
    project_script_with_warmup(script, 0)
}

/// Same projection, but with a warmup the engine actually enforces: the
/// first `warmup_bars` bars advance the context without evaluation, so the
/// data cursor an evaluated bar sees is offset from its evaluate position.
fn project_script_with_warmup(
    script: &str,
    warmup_bars: u64,
) -> Result<OverlayProjection, ProjectionRefusal> {
    project(&project_outcome_with_warmup(script, warmup_bars))
}

fn project_outcome_with_warmup(script: &str, warmup_bars: u64) -> RunOutcome {
    let program = parse_program(script).expect("script parses");
    let config = PineRuntimeConfig::new().warmup_bars(warmup_bars).build();
    PineEngine::new(config)
        .run(&program, &five_bars(), InputSet::new())
        .expect("script runs")
}

#[test]
fn plot_projects_to_series_golden() {
    // One `plot` call site becomes one series: title plus one domain value
    // per evaluated bar, `na` holes preserved as `None`.
    let projection = project_script("plot(close, title=\"Close\")\nplot(close - 200, title=\"Low\")")
        .expect("plots project");
    assert_eq!(projection.primitives.len(), 2);
    let PlotPrimitive::Series { title, values } = &projection.primitives[0] else {
        panic!("plot projects to a series");
    };
    assert_eq!(title, "Close");
    assert_eq!(values, &vec![Some(100.0), Some(101.0), Some(102.0), Some(103.0), Some(104.0)]);
    let PlotPrimitive::Series { title, values } = &projection.primitives[1] else {
        panic!("second plot projects to a series");
    };
    assert_eq!(title, "Low");
    assert_eq!(values, &vec![Some(-100.0), Some(-99.0), Some(-98.0), Some(-97.0), Some(-96.0)]);
}

#[test]
fn constant_hline_projects_to_level() {
    // A constant `hline` becomes one level; the per-bar recording collapses
    // because every resolved leg agrees.
    let projection = project_script("hline(101.5, \"Flat\")").expect("hline projects");
    assert_eq!(projection.primitives.len(), 1);
    let PlotPrimitive::Level { title, price } = &projection.primitives[0] else {
        panic!("hline projects to a level");
    };
    assert_eq!(title, "Flat");
    assert_eq!(*price, 101.5);
}

#[test]
fn varying_hline_refuses() {
    // A level that moves bar to bar has no level primitive: refuse with the
    // misbehavior named instead of pinning one leg silently.
    let refused = project_script("hline(close, title=\"V\")").expect_err("varying hline refuses");
    assert!(
        matches!(refused, ProjectionRefusal::VaryingLevel { .. }),
        "varying hline refuses as varying, got {refused}"
    );
}

#[test]
fn label_projects_to_placed_points() {
    // `label.new` in global scope mints one label per bar: bar index, price,
    // and text in domain coordinates.
    let projection =
        project_script("label.new(bar_index, high, \"hi\")").expect("labels project");
    assert_eq!(projection.primitives.len(), 5);
    for (index, primitive) in projection.primitives.iter().enumerate() {
        let PlotPrimitive::Label { bar, price, text } = primitive else {
            panic!("label.new projects to a label");
        };
        assert_eq!(*bar, index);
        assert_eq!(*price, 101.0);
        assert_eq!(text, "hi");
    }
}

#[test]
fn box_projects_to_area_golden() {
    // `box.new` corners become one area per bar: bar indices plus prices.
    let projection =
        project_script("box.new(bar_index, high, bar_index, low)").expect("boxes project");
    assert_eq!(projection.primitives.len(), 5);
    let PlotPrimitive::Area { left, top, right, bottom } = &projection.primitives[2] else {
        panic!("box.new projects to an area");
    };
    assert_eq!((*left, *right), (2, 2));
    assert_eq!((*top, *bottom), (101.0, 99.0));
}

#[test]
fn table_refuses_with_typed_reason() {
    // The engine captures tables (matrix `Match`); the overlay has no table
    // primitive, so the construct refuses here with its name on the reason.
    let refused =
        project_script("table.new(position.top_right, 2, 2)").expect_err("table refuses");
    assert!(
        matches!(&refused, ProjectionRefusal::Unsupported { construct, .. } if construct == "table.new"),
        "table refuses as unsupported, got {refused}"
    );
    assert!(refused.to_string().contains("table.new"), "reason names the construct");
}

#[test]
fn moved_label_refuses() {
    // A label the script mutates has no single placed state the projection
    // owns: refuse instead of painting the stale creation geometry.
    let refused = project_script(
        "lb = label.new(bar_index, high, \"x\")\n\
         label.set_x(lb, 7)",
    )
    .expect_err("moved label refuses");
    assert!(
        matches!(refused, ProjectionRefusal::Mutated { .. }),
        "moved label refuses as mutated, got {refused}"
    );
}

#[test]
fn deleted_label_drops() {
    // `.delete` is the engine's own lifecycle: a deleted drawing paints
    // nothing, which is absence, not a substituted value.
    let projection = project_script(
        "lb = label.new(bar_index, high, \"x\")\n\
         label.delete(lb)",
    )
    .expect("deleted label drops");
    assert!(
        projection.primitives.iter().all(|primitive| !matches!(
            primitive,
            PlotPrimitive::Label { text, .. } if text == "x"
        )),
        "no label survives its delete"
    );
}

#[test]
fn parity_lookup_stays_consistent_with_the_table() {
    // The gate reads these rows: engine-supported constructs project, and
    // the table row's `Match` documents that tables refuse at the overlay,
    // never in the engine. `plot` stands `Correct` (per-site series, not the
    // reference merged series) — still projectable, pinned here.
    let standing = |construct: &str| {
        parity_for(construct).unwrap_or_else(|| panic!("matrix covers {construct}")).verdict
    };
    assert_eq!(standing("plot"), ParityVerdict::Correct, "plot stands corrected");
    for construct in ["hline()", "label.new", "box.new", "table.new"] {
        assert_eq!(standing(construct), ParityVerdict::Match, "{construct} stands matched");
    }
    assert!(parity_for("no.such.construct").is_none(), "unknown constructs gate shut");
}

/// Bar indices of every `Label` primitive in a projection.
fn label_bars(projection: &OverlayProjection) -> Vec<usize> {
    projection
        .primitives
        .iter()
        .filter_map(|primitive| match primitive {
            PlotPrimitive::Label { bar, .. } => Some(*bar),
            _ => None,
        })
        .collect()
}

/// Every `[left, right]` span of every `Area` primitive in a projection.
fn area_spans(projection: &OverlayProjection) -> Vec<(usize, usize)> {
    projection
        .primitives
        .iter()
        .filter_map(|primitive| match primitive {
            PlotPrimitive::Area { left, right, .. } => Some((*left, *right)),
            _ => None,
        })
        .collect()
}

/// Domain of the first projected series: the bar indices a consumer can
/// actually read a value at.
fn series_domain(projection: &OverlayProjection) -> std::ops::Range<usize> {
    let values = projection
        .primitives
        .iter()
        .find_map(|primitive| match primitive {
            PlotPrimitive::Series { values, .. } => Some(values),
            _ => None,
        })
        .expect("script projects a series");
    0..values.len()
}

#[test]
fn warmup_pads_the_series_with_holes_and_labels_ride_the_cursor() {
    // One index space: the engine honours warmup (two of five bars advance
    // without evaluation, snapshots carry the absolute bar), and every
    // primitive speaks that absolute bar. The series pads the two warmup
    // bars with `None` holes so it aligns slot-for-slot with the bar window
    // the run covered; labels sit on the cursor, never on the evaluate
    // position.
    let outcome = project_outcome_with_warmup(
        "plot(close, title=\"Close\")\nlabel.new(bar_index, high, \"hi\")",
        2,
    );
    assert_eq!(outcome.evaluated, 3, "five bars minus two warmup bars");
    assert_eq!(outcome.snapshots.len(), 3);
    assert_eq!(
        outcome.snapshots.iter().map(|snapshot| snapshot.bar).collect::<Vec<_>>(),
        vec![2, 3, 4],
        "snapshots carry the absolute bar, not the evaluate position"
    );

    let projection = project(&outcome).expect("projection succeeds");
    let PlotPrimitive::Series { values, .. } = &projection.primitives[0] else {
        panic!("plot projects to a series");
    };
    assert_eq!(
        values,
        &vec![None, None, Some(102.0), Some(103.0), Some(104.0)],
        "warmup bars pad as holes; evaluated bars read their closes"
    );
    assert_eq!(label_bars(&projection), vec![2, 3, 4], "labels ride the cursor");
}

#[test]
fn drawing_geometry_stays_inside_the_projected_series_domain() {
    const WARMUP: u64 = 2;
    for script in [
        "plot(close, title=\"Close\")\nlabel.new(bar_index, high, \"hi\")",
        "plot(close, title=\"Close\")\nbox.new(bar_index, high, bar_index, low)",
    ] {
        let projection = project_script_with_warmup(script, WARMUP)
            .unwrap_or_else(|e| panic!("{script:?} projects: {e}"));
        let domain = series_domain(&projection);
        for bar in label_bars(&projection) {
            assert!(
                domain.contains(&bar),
                "{script:?}: label on bar {bar} falls outside the series domain {domain:?}"
            );
        }
        for (left, right) in area_spans(&projection) {
            assert!(
                domain.contains(&left) && domain.contains(&right),
                "{script:?}: area span {left}..={right} falls outside the series domain {domain:?}"
            );
        }
    }
}

/// Absolute bars of every `Marker` primitive in a projection.
fn marker_bars(projection: &OverlayProjection) -> Vec<usize> {
    projection
        .primitives
        .iter()
        .filter_map(|primitive| match primitive {
            PlotPrimitive::Marker { bar, .. } => Some(*bar),
            _ => None,
        })
        .collect()
}

#[test]
fn plotshape_markers_ride_the_cursor_and_skip_na() {
    // `markers_of` had no coverage at all: markers land on the absolute bar
    // of their evaluated position (not the enumerate position), and `na`
    // cells stay absent instead of pinning a zero marker.
    let projection = project_script("plotshape(close > open ? high : na, title=\"Up\")")
        .expect("plotshape projects");
    let bars = marker_bars(&projection);
    assert_eq!(bars, vec![1, 2, 3, 4], "up bars mark, the down bar stays absent");
    for primitive in &projection.primitives {
        let PlotPrimitive::Marker { title, price, .. } = primitive else {
            panic!("plotshape projects to markers");
        };
        assert_eq!(title, "Up");
        assert_eq!(*price, 101.0);
    }

    let warmed = project_script_with_warmup(
        "plotshape(close > open ? high : na, title=\"Up\")",
        2,
    )
    .expect("warmed plotshape projects");
    assert_eq!(
        marker_bars(&warmed),
        vec![2, 3, 4],
        "warming by two moves every marker by two, never re-indexes to zero"
    );
}

#[test]
fn hidden_plot_projects_nothing_and_refuses_nothing() {
    // `display = display.none` is a skip, not a refusal: the hidden site
    // paints nothing, and a visible sibling still projects.
    let hidden_only =
        project_script("plot(close, title=\"H\", display=display.none)").expect("hidden plots skip");
    assert!(
        hidden_only.primitives.is_empty(),
        "a hidden plot leaves no primitive"
    );

    let mixed = project_script(
        "plot(close, title=\"V\")\nplot(open, title=\"H\", display=display.none)",
    )
    .expect("a hidden sibling refuses nothing");
    assert_eq!(mixed.primitives.len(), 1, "only the visible site projects");
    let PlotPrimitive::Series { title, values } = &mixed.primitives[0] else {
        panic!("the visible plot projects to a series");
    };
    assert_eq!(title, "V");
    assert_eq!(values, &vec![Some(100.0), Some(101.0), Some(102.0), Some(103.0), Some(104.0)]);

    // A narrowed flag combination still shows its legs: only the explicit
    // zero hides.
    let narrowed = project_script(
        "plot(close, title=\"N\", display=display.all - display.status_line)",
    )
    .expect("narrowed display projects");
    assert_eq!(narrowed.primitives.len(), 1, "nonzero flags stay visible");
}

#[test]
fn unplaceable_drawings_drop_instead_of_substituting() {
    // Each of these has no bar or no text to sit on: the drawing drops, and
    // the projection still succeeds with nothing placed — never a refusal
    // of unrelated plots, never a substituted coordinate. (Fractional
    // floats denote no bar; integral floats place exactly — pinned by the
    // warmup test — so the float controls below are fractional.)
    for script in [
        "plot(close, title=\"S\")\nlabel.new(bar_index, high, 42)",
        "plot(close, title=\"S\")\nlabel.new(1.5, high, \"x\")",
        "plot(close, title=\"S\")\nlabel.new(-1, high, \"x\")",
        "plot(close, title=\"S\")\nbox.new(1.5, high, 4, low)",
        "plot(close, title=\"S\")\nbox.new(-1, high, 4, low)",
    ] {
        let projection =
            project_script(script).unwrap_or_else(|e| panic!("{script:?} drops, not refuses: {e}"));
        assert_eq!(series_domain(&projection), 0..5, "{script:?}: the plot survives");
        assert!(
            label_bars(&projection).is_empty() && area_spans(&projection).is_empty(),
            "{script:?}: nothing unplaceable is placed"
        );
    }
}

#[test]
fn drawings_on_warmup_bars_drop() {
    // The script can name a bar the run never evaluated: `bar_index - 2`
    // reaches back into the warmup holes, which carry no projected value to
    // sit on. Those drawings drop; the one anchored on an evaluated bar
    // still places. (`bar_index - 2` also proves integral floats place: the
    // engine widens integer arithmetic with a literal to `Float`, staying
    // integral, and an integral float denotes exactly one bar.)
    let projection = project_script_with_warmup(
        "plot(close, title=\"Close\")\nlabel.new(bar_index - 2, high, \"w\")",
        2,
    )
    .expect("warmup-hole drawings drop");
    assert_eq!(
        label_bars(&projection),
        vec![2],
        "only the label on the first evaluated bar places"
    );
}

#[test]
fn positional_prefix_sets_creation_geometry() {
    // Creation binds the positional prefix by namespace and demands it
    // (`drawing_new` refuses a short prefix), so a named extra riding along
    // never moves the coordinate the projection reads: the labels sit on
    // their positional `bar_index`, never on the named `x=5`.
    let projection =
        project_script("label.new(bar_index, high, \"t\", x=5)").expect("mixed creation runs");
    assert_eq!(
        label_bars(&projection),
        vec![0, 1, 2, 3, 4],
        "the positional bar places, not the named extra"
    );
}

#[test]
fn repaints_keep_the_creation_geometry() {
    // Cosmetic-only setters change no placement and no content: the label
    // and the box place at their creation geometry instead of refusing the
    // whole overlay (and discarding the unrelated plot series with it).
    let projection = project_script(
        "plot(close, title=\"S\")\n\
         lb = label.new(bar_index, high, \"x\")\n\
         label.set_textcolor(lb, color.red)\n\
         label.set_style(lb, label.style_label_down)\n\
         bx = box.new(bar_index, high, bar_index, low)\n\
         box.set_bgcolor(bx, color.red)",
    )
    .expect("repaints refuse nothing");
    assert_eq!(series_domain(&projection), 0..5, "the plot survives the repaints");
    assert_eq!(label_bars(&projection), vec![0, 1, 2, 3, 4], "repainted labels place");
    assert_eq!(
        area_spans(&projection),
        vec![(0, 0), (1, 1), (2, 2), (3, 3), (4, 4)],
        "repainted boxes place"
    );
}

#[test]
fn content_changes_still_refuse() {
    // `set_text` changes what the overlay would blit: painting the creation
    // string would miscompute content, so the run still refuses. Text is
    // content, not a repaint.
    let refused = project_script(
        "lb = label.new(bar_index, high, \"x\")\n\
         label.set_text(lb, \"y\")",
    )
    .expect_err("renamed label refuses");
    assert!(
        matches!(refused, ProjectionRefusal::Mutated { .. }),
        "renamed label refuses as mutated, got {refused}"
    );
}
