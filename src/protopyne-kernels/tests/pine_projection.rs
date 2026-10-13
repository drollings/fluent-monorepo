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
    let program = parse_program(script).expect("script parses");
    let outcome = PineEngine::new(PineRuntimeConfig::new().build())
        .run(&program, &five_bars(), InputSet::new())
        .expect("script runs");
    project(&outcome)
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
