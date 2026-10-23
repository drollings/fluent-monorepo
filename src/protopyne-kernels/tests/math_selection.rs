//! Golden pins for the selection-based math helpers.
//!
//! Float equality is the point here: these pin exact returned values.
#![allow(clippy::float_cmp)]
//!
//! `median_of`, `percentile_linear`, and `mode_of` now *select* instead of
//! sorting (and `mode_of` counts runs instead of rescanning the prefix), so
//! these pin the **values** the old shapes produced. `select_nth_unstable` is
//! unstable, which is exactly why nothing here asserts the order of equal
//! elements — only the value each helper returns.

use super::*;

/// Odd, even, duplicates, and the all-equal window: the four shapes a median
/// can take.
#[test]
fn median_pins_odd_even_duplicates_and_all_equal() {
    assert_eq!(median_of(&[3.0, 1.0, 2.0]), Some(2.0));
    assert_eq!(median_of(&[4.0, 1.0, 3.0, 2.0]), Some(2.5));
    // Duplicates on the boundary: same value on both sides of the middle.
    assert_eq!(median_of(&[1.0, 1.0, 1.0, 9.0]), Some(1.0));
    assert_eq!(median_of(&[5.0, 5.0, 5.0, 5.0, 5.0]), Some(5.0));
    assert_eq!(median_of(&[7.0]), Some(7.0));
    assert_eq!(median_of(&[]), None);
    // The window is copied, never reordered in place.
    let values = [3.0, 1.0, 2.0];
    assert_eq!(median_of(&values), Some(2.0));
    assert_eq!(values, [3.0, 1.0, 2.0]);
}

/// The interpolation index at both ends and in the middle: `rank = p/100 *
/// (n - 1)`, so 0 and 100 are the plain extreme elements (no interpolation).
#[test]
fn percentile_pins_the_interpolation_index_at_both_ends() {
    let values = [10.0, 20.0, 30.0, 40.0];
    assert_eq!(percentile_linear(&values, 0.0), Some(10.0));
    assert_eq!(percentile_linear(&values, 100.0), Some(40.0));
    assert_eq!(percentile_linear(&values, 50.0), Some(25.0));
    assert_eq!(percentile_linear(&values, 25.0), Some(17.5));
    // Three points: the middle rank is an exact element, not an interpolation.
    assert_eq!(percentile_linear(&[10.0, 20.0, 40.0], 50.0), Some(20.0));
    assert_eq!(percentile_linear(&[42.0], 73.0), Some(42.0));
    // Duplicates at the boundary.
    assert_eq!(percentile_linear(&[1.0, 1.0, 2.0], 50.0), Some(1.0));
    assert_eq!(percentile_linear(&[], 50.0), None);
    assert_eq!(percentile_linear(&values, f64::NAN), None);
    assert_eq!(percentile_linear(&values, f64::INFINITY), None);
}

/// The tie-break is part of the contract: frequency ties keep the value that
/// appears **first in the input**, and `NaN` forms no bucket.
#[test]
fn mode_pins_the_first_appearance_tie_break() {
    assert_eq!(mode_of(&[1.0, 2.0, 2.0, 3.0, 3.0]), Some(2.0));
    assert_eq!(mode_of(&[3.0, 2.0, 2.0, 3.0, 3.0]), Some(3.0));
    // Two values share the top count: the earliest one wins, not the smallest
    // and not the one the sorted pass happens to see first.
    assert_eq!(mode_of(&[9.0, 1.0, 1.0, 9.0, 5.0]), Some(9.0));
    // `-0.0` and `0.0` are one bucket, and the first value's sign is what is
    // returned, so compare bits even though `-0.0 == 0.0`.
    let zero_mode = mode_of(&[0.0, -0.0, -0.0, 1.0]).expect("the zero bucket wins");
    assert_eq!(zero_mode.to_bits(), 0.0_f64.to_bits());
    assert_eq!(mode_of(&[4.0, 4.0, 4.0]), Some(4.0));
    assert_eq!(mode_of(&[]), None);
    // `NaN` is never equal to itself, so it never wins a count bucket; an
    // all-`NaN` window keeps the old answer (its first value).
    assert!(mode_of(&[f64::NAN]).expect("all-NaN").is_nan());
    assert_eq!(mode_of(&[f64::NAN, 1.0, 1.0]), Some(1.0));
    assert_eq!(mode_of(&[1.0, f64::NAN, 1.0, 2.0]), Some(1.0));
}

/// The scratch contract: after the buffer has reached its high-water capacity,
/// every helper runs without reallocating it. That is the observable half of
/// "no per-call allocation" under this crate's `#![forbid(unsafe_code)]` — a
/// counting global allocator would need an `unsafe impl` the crate forbids, so
/// the pin is the buffer's own capacity plus signatures that take no owned
/// inputs.
#[test]
fn scratch_calls_never_grow_the_buffer_after_the_first_fill() {
    let longest: Vec<f64> = (0..64).map(|index| f64::from(index) * 0.5).collect();
    let mut scratch = Vec::new();
    let _ = median_of_with(&longest, &mut scratch);
    let capacity = scratch.capacity();
    assert!(capacity >= longest.len());

    for length in [1_usize, 2, 3, 4, 7, 16, 31, 63, 64] {
        let window = &longest[..length];
        assert!(median_of_with(window, &mut scratch).is_some());
        assert!(percentile_linear_with(window, 37.5, &mut scratch).is_some());
        assert!(mode_of_with(window, &mut scratch).is_some());
        assert_eq!(
            scratch.capacity(),
            capacity,
            "a reuse at length {length} reallocated the scratch buffer"
        );
    }

    let mut out = Vec::new();
    standardize_scores_into(&longest, &mut out);
    let out_capacity = out.capacity();
    for length in [1_usize, 5, 32, 64] {
        standardize_scores_into(&longest[..length], &mut out);
        assert_eq!(out.len(), length);
        assert_eq!(
            out.capacity(),
            out_capacity,
            "a standardize reuse at length {length} reallocated its buffer"
        );
    }
}

/// `standardize_scores` and its buffer form agree, and a zero spread fills the
/// buffer with `None` rather than refusing (the language's `0 / 0` is `na`).
#[test]
fn standardize_scores_matches_its_buffer_form() {
    let values = [1.0, 2.0, 3.0, 4.0];
    let mut buffered = Vec::new();
    standardize_scores_into(&values, &mut buffered);
    assert_eq!(standardize_scores(&values), buffered);
    let flat = [5.0, 5.0, 5.0];
    assert_eq!(standardize_scores(&flat), vec![None, None, None]);
    assert_eq!(standardize_scores(&[]), Vec::<Option<f64>>::new());
}
