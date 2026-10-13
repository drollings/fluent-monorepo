use super::*;
use crate::paint::marker_dots;
use crate::viewport::Viewport;
use protopyne_chart_model::{Annotation, AnnotationKey, AnnotationStyle};
use protopyne_chart_scale::PriceScaleMode;
use rust_decimal::Decimal;
use std::str::FromStr as _;

fn price(text: &str) -> Decimal {
    Decimal::from_str(text).expect("price parses")
}

/// Ten bars at epoch seconds `1_700_000_000 + index * 60`, all around 100.
fn rows() -> Vec<ChartRow> {
    (0..10_i64)
        .map(|index| ChartRow {
            time: 1_700_000_000 + index * 60,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.0,
            volume: 10,
        })
        .collect()
}

fn note(bar: i64, text: &str) -> Annotation {
    Annotation {
        key: AnnotationKey { bar_timestamp: 1_700_000_000 + bar * 60, price: price("100.50") },
        label: text.to_owned(),
        style: AnnotationStyle::Note,
    }
}

fn pane() -> Viewport {
    Viewport::new(PriceScaleMode::Normal, 90.0, 110.0, 400.0, 8.0, 90.0, false)
        .expect("valid price pane")
}

#[test]
fn undo_retracts_appends_last_in_first_out() {
    let mut set = AnnotationSet::new();
    assert!(set.undo().is_none(), "empty set has nothing to undo");
    set.append(note(1, "first"));
    set.append(note(2, "second"));
    assert_eq!(set.len(), 2);
    let undone = set.undo().expect("second append retracts");
    assert_eq!(undone.label, "second");
    assert_eq!(set.len(), 1, "only the retracted entry leaves");
    let live: Vec<String> = set.iter().map(|ann| ann.label.clone()).collect();
    assert_eq!(live, vec!["first".to_owned()], "survivors keep append order");
}

#[test]
fn undo_depth_is_bounded() {
    let mut set = AnnotationSet::new();
    for index in 0..(MAX_UNDO_DEPTH + 5) {
        set.append(note(0, &format!("mark-{index}")));
    }
    let mut retracted = 0;
    while set.undo().is_some() {
        retracted += 1;
    }
    assert_eq!(retracted, MAX_UNDO_DEPTH, "only the last N appends retract");
    assert_eq!(set.len(), 5, "older entries stay published, never silently dropped");
}

#[test]
fn resolve_maps_domain_keys_to_window_pixels() {
    let pane = pane();
    let rows = rows();
    let annotation = note(3, "third bar");
    let (x, y) = resolve_annotation(&annotation, &rows, &pane).expect("bar 3 resolves");
    assert_eq!(x.to_bits(), pane.index_to_x(3).to_bits(), "x is the bar center");
    assert_eq!(
        y.to_bits(),
        pane.price_to_y(100.5).expect("price maps").to_bits(),
        "y is the price pixel"
    );
    let missing = Annotation {
        key: AnnotationKey { bar_timestamp: 1_800_000_000, price: price("100.50") },
        label: "no such bar".to_owned(),
        style: AnnotationStyle::Note,
    };
    assert!(
        resolve_annotation(&missing, &rows, &pane).is_none(),
        "timestamps outside the window resolve to nothing, never a neighbor"
    );
}

#[test]
fn keys_survive_pan_zoom_and_resize() {
    // Pan (shifted window), zoom (tighter range, wider bars), resize (taller
    // pane): the same key must name the same bar every time while the pixels
    // move — the key does not move when the pixels do.
    let rows = rows();
    let annotation = note(4, "fourth bar");
    let pan = pane();
    let zoomed = Viewport::new(PriceScaleMode::Normal, 98.0, 102.0, 400.0, 16.0, 98.0, false)
        .expect("valid zoomed pane");
    let resized = Viewport::new(PriceScaleMode::Normal, 90.0, 110.0, 800.0, 8.0, 90.0, false)
        .expect("valid resized pane");
    let mut pixels = Vec::new();
    for viewport in [&pan, &zoomed, &resized] {
        let (x, y) = resolve_annotation(&annotation, &rows, viewport).expect("bar 4 resolves");
        assert_eq!(x.to_bits(), viewport.index_to_x(4).to_bits(), "same bar under every viewport");
        pixels.push((x, y));
    }
    assert!(
        pixels[0] != pixels[1] && pixels[0] != pixels[2],
        "pixels move across zoom/resize: {pixels:?}"
    );
    assert_eq!(annotation.key.bar_timestamp, 1_700_000_000 + 4 * 60, "key untouched");
}

#[test]
fn markers_render_from_the_append_only_set() {
    let pane = pane();
    let rows = rows();
    let mut set = AnnotationSet::new();
    set.append(note(1, "one"));
    set.append(Annotation {
        key: AnnotationKey { bar_timestamp: 1_700_000_000 + 5 * 60, price: price("99.25") },
        label: "five".to_owned(),
        style: AnnotationStyle::Bear,
    });
    let live: Vec<Annotation> = set.iter().cloned().collect();
    let dots = marker_dots(&live, &rows, &pane);
    assert_eq!(dots.len(), 2, "every published marker resolves");
    assert_eq!(dots[0].index, 1);
    assert_eq!(dots[1].index, 5);
    set.undo();
    let live: Vec<Annotation> = set.iter().cloned().collect();
    let dots = marker_dots(&live, &rows, &pane);
    assert_eq!(dots.len(), 1, "render reads the set: undone markers vanish");
    assert_eq!(dots[0].index, 1);
}

#[test]
fn no_generic_undo_over_authoritative_types() {
    // Compile-fail ratchet (see tests/ui/no_generic_undo.rs): undo is
    // concrete over `AnnotationSet`, so journal/order types can never gain
    // undo by instantiation. If the ui file ever compiles, this fails.
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/no_generic_undo.rs");
}
