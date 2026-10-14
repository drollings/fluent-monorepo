use super::*;
use crate::hittest::HIT_EPSILON_PX;
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
fn undo_refuses_exactly_past_the_depth() {
    // The boundary is exact: the first `MAX_UNDO_DEPTH` retractions succeed
    // and the next one refuses — the log holds 32, never 33, never 31.
    let mut set = AnnotationSet::new();
    for index in 0..=MAX_UNDO_DEPTH {
        set.append(note(0, &format!("mark-{index}")));
    }
    for undone in 0..MAX_UNDO_DEPTH {
        set.undo().unwrap_or_else(|| panic!("retraction {undone} succeeds"));
    }
    assert!(set.undo().is_none(), "the 33rd retraction refuses: the log holds 32");
    assert_eq!(set.len(), 1, "one entry stays published");
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
fn duplicate_timestamps_bind_nothing() {
    // Two bars claiming one close time is a data fault: the duplicated
    // timestamp resolves to nothing through every path — never the first
    // match — while unique neighbors keep resolving.
    let pane = pane();
    let mut rows = rows();
    rows[4].time = rows[3].time;
    let index = TimeIndex::of(&rows);
    assert_eq!(index.lookup(rows[0].time), Some(0), "unique timestamps still map");
    assert_eq!(
        index.lookup(rows[3].time),
        None,
        "a duplicated timestamp binds nothing, never the first match"
    );
    let duplicated = Annotation {
        key: AnnotationKey { bar_timestamp: rows[3].time, price: price("100.50") },
        label: "two bars, one time".to_owned(),
        style: AnnotationStyle::Note,
    };
    assert!(index.resolve(&duplicated, &pane).is_none(), "map path binds nothing");
    assert!(
        resolve_annotation(&duplicated, &rows, &pane).is_none(),
        "single-resolve path detects too"
    );
    let neighbor = note(5, "fifth bar");
    assert!(
        resolve_annotation(&neighbor, &rows, &pane).is_some(),
        "unique neighbors are unaffected"
    );
    let dots = marker_dots(&[duplicated, neighbor], &rows, &pane);
    assert_eq!(dots.len(), 1, "the batch path skips the duplicate, paints the neighbor");
    assert_eq!(dots[0].index, 5);
}

#[test]
fn clicks_without_a_candle_create_nothing() {
    // The control group that must NOT create: empty pane past the last bar,
    // the price gutter just past the last bar's slot, and a click above the
    // pane where no price maps. Any creation here is a hard failure.
    let pane = pane();
    let rows = rows();
    let mid_y = pane.price_to_y(100.0).expect("100 maps");
    for (name, pointer) in [
        ("past the last bar", (pane.index_to_x(15), mid_y)),
        ("gutter past the last slot", (pane.index_to_x(10) + 2.0, mid_y)),
        ("above the pane", (pane.index_to_x(3), -40.0)),
    ] {
        assert!(
            annotate_at(pointer, &pane, &rows).is_none(),
            "{name} creates nothing: {pointer:?}"
        );
    }
}

#[test]
fn clicks_on_an_unrepresentable_price_create_nothing() {
    // A bar whose close is non-finite still shows a mappable wick, so the
    // hit lands — but the annotation key would carry a price no float can
    // hold, so no annotation is created instead of a substituted value.
    let pane = pane();
    let mut rows = rows();
    rows[3].close = f64::NAN;
    let tip_y = pane.price_to_y(101.0).expect("wick tip maps");
    assert!(
        annotate_at((pane.index_to_x(3), tip_y), &pane, &rows).is_none(),
        "an unrepresentable close creates nothing"
    );
    rows[3].close = 100.0;
    assert!(
        annotate_at((pane.index_to_x(3), tip_y), &pane, &rows).is_some(),
        "the same click creates once the close maps"
    );
}

#[test]
fn placement_radius_confirms_intent_monotonically() {
    // Calibration of the placement radius: walking up from bar 3's wick tip
    // in 1 px steps, creation must hold through the radius and stop right
    // after — one transition, no gaps, precision and recall both 1.0
    // against the known bar. The slot never moves on this walk, so the
    // radius is the only thing under test.
    let pane = pane();
    let rows = rows();
    let tip_y = pane.price_to_y(101.0).expect("wick tip maps");
    let x = pane.index_to_x(3);
    let mut true_positives = 0;
    let mut false_positives = 0;
    let mut false_negatives = 0;
    let mut transitions = 0;
    let mut previously_created = true;
    for offset in 0..=20_u8 {
        let created = annotate_at((x, tip_y - f32::from(offset)), &pane, &rows);
        let lands_on_bar_3 = created
            .as_ref()
            .is_some_and(|annotation| annotation.key.bar_timestamp == rows[3].time);
        let expected = f32::from(offset) <= HIT_EPSILON_PX;
        match (expected, lands_on_bar_3) {
            (true, true) => true_positives += 1,
            (false, false) => {}
            (false, true) => false_positives += 1,
            (true, false) => false_negatives += 1,
        }
        let now_created = created.is_some();
        if now_created != previously_created {
            transitions += 1;
            previously_created = now_created;
        }
    }
    assert_eq!(transitions, 1, "one hit-to-miss transition, never a gap and return");
    let precision = f64::from(true_positives) / f64::from(true_positives + false_positives);
    let recall = f64::from(true_positives) / f64::from(true_positives + false_negatives);
    assert_eq!(
        (true_positives, false_positives, false_negatives),
        (9, 0, 0),
        "precision {precision} and recall {recall} are both 1.0 against the known bar"
    );
}

#[test]
fn clicks_land_on_bar_data_not_pointer_data() {
    // The radius confirms intent; the bar fixes the key. A click low on the
    // wick still keys the bar's close time and close price — never the
    // pointer's slot fraction, never the pointer's price.
    let pane = pane();
    let rows = rows();
    let low_y = pane.price_to_y(99.5).expect("low wick maps");
    let created =
        annotate_at((pane.index_to_x(3), low_y), &pane, &rows).expect("low-wick click creates");
    assert_eq!(created.key.bar_timestamp, rows[3].time, "time is the bar's");
    assert_eq!(created.key.price, price("100"), "price is the bar's close, not the pointer's");
    assert_eq!(created.style, AnnotationStyle::Note, "clicks land neutral");
}

#[test]
fn resolve_maps_domain_keys_to_window_pixels() {
    let pane = pane();
    let rows = rows();
    let annotation = note(3, "third bar");
    let (x, y) = resolve_annotation(&annotation, &rows, &pane).expect("bar 3 resolves");
    // Bar 3 sits on slot 3 of the 8 px spacing; the key price 100.50 sits
    // halfway between 100 and 101 over the 90..110 window on 400 px
    // (y = (110 - 100.5) / 20 * 400 = 190) — hand-derived literals, never a
    // second mapping call. A resolution through the bar close (100.0) would
    // land a full 10 px lower, so the tolerance still discriminates.
    assert_eq!(x.to_bits(), 24.0f32.to_bits(), "x is the bar center");
    assert!((y - 190.0).abs() < 1e-3, "y is the key-price pixel, got {y}");
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
    // Bar 4's slot under each viewport's own spacing — literals on the
    // documented bar-zero contract, so the stability assert below compares
    // against independently derived pixels, not a second mapping call.
    for (viewport, slot) in [(&pan, 32.0f32), (&zoomed, 64.0f32), (&resized, 32.0f32)] {
        let (x, y) = resolve_annotation(&annotation, &rows, viewport).expect("bar 4 resolves");
        assert_eq!(x.to_bits(), slot.to_bits(), "same bar under every viewport");
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
