//! Pine drawings enter the operator's annotation store: labels keep their
//! text keyed by bar time and exact price, areas become corner pairs, and
//! continuous script output (series, levels, markers) converts to nothing.
//! Appends are idempotent per content, so a re-run adds nothing.
//!
//! The end-to-end test observes the projection guarantee through
//! this bridge: a `label.new` followed by a cosmetic `set_textcolor`
//! projects, converts, and appends instead of refusing.

use super::*;
use chrono::TimeZone as _;
use fluent_egui::AnnotationSet;
use protopyne_chart_model::AnnotationStyle;
use protopyne_kernels::{InputSet, PineEngine, PineRuntimeConfig, PlotPrimitive, parse_program, project};
use protopyne_types::Bar;
use rust_decimal::Decimal;

/// Five minutely bars from epoch `1_700_000_000`, closes `100 + index`.
fn bars() -> Vec<Bar> {
    (0..5_i64)
        .map(|index| {
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
        })
        .collect()
}

fn run(script: &str) -> Vec<PlotPrimitive> {
    let bars = bars();
    let program = parse_program(script).expect("script parses");
    let outcome = PineEngine::new(PineRuntimeConfig::new().build())
        .run(&program, &bars, InputSet::new())
        .expect("script runs");
    project(&outcome).expect("projection succeeds").primitives
}

#[test]
fn labels_convert_with_text_time_and_exact_price() {
    // One label: the bar's close time, the exact decimal price, its text.
    let bars = bars();
    let converted = annotations_of(
        &[PlotPrimitive::Label { bar: 2, price: 99.5, text: "hi".to_owned() }],
        &bars,
    );
    assert_eq!(converted.len(), 1);
    let annotation = &converted[0];
    assert_eq!(annotation.key.bar_timestamp, 1_700_000_000 + 2 * 60, "time is the bar's close time");
    assert_eq!(annotation.key.price, Decimal::new(995, 1), "price is exact, never a float echo");
    assert_eq!(annotation.label, "hi");
    assert_eq!(annotation.style, AnnotationStyle::Note);
}

#[test]
fn areas_convert_to_corner_pairs() {
    // A span has no single-key encoding: both corners journal, with empty
    // labels because the primitive carries no text.
    let bars = bars();
    let converted = annotations_of(
        &[PlotPrimitive::Area { left: 1, top: 101.0, right: 3, bottom: 99.0 }],
        &bars,
    );
    assert_eq!(converted.len(), 2);
    assert_eq!(converted[0].key.bar_timestamp, 1_700_000_060);
    assert_eq!(converted[0].key.price, Decimal::new(1010, 1));
    assert_eq!(converted[1].key.bar_timestamp, 1_700_000_000 + 3 * 60);
    assert_eq!(converted[1].key.price, Decimal::new(990, 1));
    assert!(converted.iter().all(|annotation| annotation.label.is_empty()));
}

#[test]
fn continuous_output_converts_to_nothing() {
    // Series, levels, and markers repaint every frame from the projection;
    // journaling them would flood the store, so they convert to nothing.
    let bars = bars();
    let converted = annotations_of(
        &[
            PlotPrimitive::Series { title: "S".to_owned(), values: vec![Some(100.0)] },
            PlotPrimitive::Level { title: "L".to_owned(), price: 100.0 },
            PlotPrimitive::Marker { title: "M".to_owned(), bar: 0, price: 100.0 },
        ],
        &bars,
    );
    assert!(converted.is_empty(), "no continuous output journals");
}

#[test]
fn misaligned_primitives_drop_instead_of_substituting() {
    // A bar past the window, or a price no decimal can hold, drops — the
    // bridge this consumes guarantees both, so a drop names a misaligned
    // input, never a quiet substitution.
    let bars = bars();
    let converted = annotations_of(
        &[
            PlotPrimitive::Label { bar: 99, price: 100.0, text: "past".to_owned() },
            PlotPrimitive::Label { bar: 0, price: f64::NAN, text: "vapor".to_owned() },
            PlotPrimitive::Label { bar: 1, price: 100.0, text: "kept".to_owned() },
        ],
        &bars,
    );
    assert_eq!(converted.len(), 1);
    assert_eq!(converted[0].label, "kept");
}

#[test]
fn appends_are_idempotent_per_content() {
    // Re-running an unchanged script appends nothing; a new drawing appends
    // once. Stale entries are never removed by a re-run.
    let bars = bars();
    let mut set = AnnotationSet::new();
    let one = annotations_of(
        &[PlotPrimitive::Label { bar: 0, price: 100.0, text: "a".to_owned() }],
        &bars,
    );
    assert_eq!(append_new(&mut set, &one), 1);
    let rerun = append_new(&mut set, &one);
    assert_eq!(rerun, 0, "a re-run adds nothing");
    assert_eq!(set.len(), 1);
    let two = annotations_of(
        &[PlotPrimitive::Label { bar: 4, price: 100.0, text: "b".to_owned() }],
        &bars,
    );
    assert_eq!(append_new(&mut set, &two), 1, "a new drawing appends");
    assert_eq!(set.len(), 2);
}

#[test]
fn repainted_labels_reach_the_store() {
    // End-to-end observation of the projection guarantee: `label.new`
    // followed by a cosmetic `set_textcolor` does not refuse, so the label
    // converts and appends with its creation geometry.
    let bars = bars();
    let primitives = run(
        "lb = label.new(bar_index, high, \"x\")\n\
         label.set_textcolor(lb, color.red)",
    );
    let mut set = AnnotationSet::new();
    let converted = annotations_of(&primitives, &bars);
    assert_eq!(append_new(&mut set, &converted), 5, "all five repainted labels append");
    let times: Vec<i64> = set.iter().map(|annotation| annotation.key.bar_timestamp).collect();
    assert_eq!(
        times,
        (0..5_i64).map(|index| 1_700_000_000 + index * 60).collect::<Vec<_>>(),
        "one annotation per bar, in order"
    );
    assert!(
        set.iter().all(|annotation| annotation.label == "x"),
        "creation text rides along"
    );
}

#[test]
fn moved_labels_never_reach_the_store() {
    // The decided side of the append-only conflict: a geometric move refuses
    // the projection before conversion, so nothing appends — the store never
    // holds a silently stale placement.
    let bars = bars();
    let program = parse_program(
        "lb = label.new(bar_index, high, \"x\")\n\
         label.set_x(lb, 7)",
    )
    .expect("script parses");
    let outcome = PineEngine::new(PineRuntimeConfig::new().build())
        .run(&program, &bars, InputSet::new())
        .expect("script runs");
    assert!(project(&outcome).is_err(), "the move refuses before conversion");
}
