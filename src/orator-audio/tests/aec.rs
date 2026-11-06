//! `aec` contract tests: passthrough, shapes, bounds, stale-ref
//! discipline — all hermetic (synthetic frames, no device, no model).
//!
//! Cancellation *depth* is not pinned here: suppression quality needs
//! a live loopback measurement (see the module docs), and a scripted
//! assertion of it would be a false measurement.

use crate::aec::{AecFilter, AEC_REF_DELAY_MS};

#[test]
fn disabled_filter_passes_audio_through_identically() {
    let (mut filter, tap) = AecFilter::disabled();
    assert!(!filter.is_enabled(), "disabled reports itself");
    tap.feed(&vec![0.5; 320]);
    filter.pump();
    let mut frame = vec![0.25; 320];
    filter.process(&mut frame);
    assert_eq!(frame, vec![0.25; 320], "passthrough is byte-identical");
}

#[test]
fn enabled_filter_preserves_frame_shape() {
    let (mut filter, tap) = AecFilter::new(AEC_REF_DELAY_MS);
    assert!(filter.is_enabled(), "enabled reports itself");
    assert_eq!(AEC_REF_DELAY_MS, 80);
    tap.feed(&vec![0.1; 3200]);
    filter.pump();
    let mut frame = vec![0.3; 320];
    filter.process(&mut frame);
    assert_eq!(frame.len(), 320, "frame length preserved");
    assert!(
        frame.iter().all(|sample| *sample >= -1.0 && *sample <= 1.0),
        "filter output stays in [-1, 1], no NaN/Inf escapes"
    );
}

#[test]
fn stale_reference_never_cancels() {
    let (mut filter, tap) = AecFilter::new(AEC_REF_DELAY_MS);
    // Reference arrives once, then the speakers go quiet: after the
    // fresh window drains, mic passes through untouched.
    tap.feed(&vec![0.4; 160]);
    filter.pump();
    let mut first = vec![0.4; 320];
    filter.process(&mut first);
    for _ in 0..8 {
        let mut later = vec![0.4; 320];
        filter.process(&mut later);
        assert_eq!(
            later,
            vec![0.4; 320],
            "exhausted reference passes through, never stale-cancels"
        );
    }
}

#[test]
fn tap_never_blocks_playout_and_bounds_memory() {
    let (mut filter, tap) = AecFilter::new(AEC_REF_DELAY_MS);
    // Flood the tap: feeds never block and the ring stays capped.
    for _ in 0..64 {
        tap.feed(&vec![0.2; 4800]);
    }
    filter.pump();
    let mut frame = vec![0.2; 320];
    filter.process(&mut frame);
    assert_eq!(frame.len(), 320);
}

#[test]
fn partial_trailing_subframe_passes_through() {
    let (mut filter, tap) = AecFilter::new(AEC_REF_DELAY_MS);
    tap.feed(&vec![0.1; 3200]);
    filter.pump();
    let mut frame = vec![0.3; 200];
    filter.process(&mut frame);
    assert_eq!(frame.len(), 200);
    assert_eq!(
        frame[160..].to_vec(),
        vec![0.3; 40],
        "trailing partial subframe is never fabricated"
    );
}
