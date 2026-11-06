//! Backend tests: enumeration never fails; the ring contract holds.

use rtrb::RingBuffer;

use crate::backend::list_input_devices;

#[test]
fn device_listing_never_fails() {
    // Headless CI yields an empty list; hardware yields names. Either way,
    // listing itself must not raise.
    let _devices = list_input_devices();
}

#[test]
fn ring_push_pop_is_lossless_in_order() {
    let (mut producer, mut consumer) = RingBuffer::new(8);
    for sample in 0..8 {
        producer.push(sample).expect("ring has room");
    }
    assert!(producer.push(99).is_err());
    for sample in 0..8 {
        assert_eq!(consumer.pop().expect("ring has data"), sample);
    }
    assert!(consumer.pop().is_err());
}

#[test]
fn rt_overflow_drops_instead_of_blocking() {
    let (mut producer, mut consumer) = RingBuffer::new(2);
    producer.push(1.0f32).expect("room");
    producer.push(2.0).expect("room");
    // Full ring: the RT callback drops, counted downstream as a gap.
    assert!(producer.push(3.0).is_err());
    assert!((consumer.pop().expect("data") - 1.0).abs() < f32::EPSILON);
    producer.push(3.0).expect("room again");
}

#[test]
fn stereo_capture_averages_to_mono() {
    use crate::backend::CpalBackend;
    // Dual-mono speech: identical channels collapse to the signal.
    let stereo = [0.5f32, 0.5, -0.25, -0.25, 0.0, 0.0];
    let mut out = Vec::new();
    let dropped = CpalBackend::collapse_to_mono(&stereo, |sample| sample, 2, &mut out);
    assert_eq!(dropped, 0);
    assert_eq!(out, vec![0.5, -0.25, 0.0]);
    // Hard-panned speech (dead right channel) halves, never drops:
    // the words survive at lower level instead of arriving
    // double-speed and interleaved.
    let panned = [0.8f32, 0.0, 0.4, 0.0];
    let mut out = Vec::new();
    let dropped = CpalBackend::collapse_to_mono(&panned, |sample| sample, 2, &mut out);
    assert_eq!(dropped, 0);
    assert_eq!(out, vec![0.4, 0.2]);
}

#[test]
fn mono_capture_passes_through_untouched() {
    use crate::backend::CpalBackend;
    let mono = [0.1f32, -0.2, 0.3];
    let mut out = Vec::new();
    let dropped = CpalBackend::collapse_to_mono(&mono, |sample| sample, 1, &mut out);
    assert_eq!(dropped, 0);
    assert_eq!(out, mono);
    // Degenerate channel counts behave as mono, never divide by zero.
    let mut out = Vec::new();
    let dropped = CpalBackend::collapse_to_mono(&mono, |sample| sample, 0, &mut out);
    assert_eq!(dropped, 0);
    assert_eq!(out, mono);
}

#[test]
fn torn_frames_count_as_dropped_not_audio() {
    use crate::backend::CpalBackend;
    let mut out = Vec::new();
    let dropped = CpalBackend::collapse_to_mono(&[0.5f32, 0.5, 0.25], |sample| sample, 2, &mut out);
    assert_eq!(dropped, 1, "one trailing sample cannot form a frame");
    assert_eq!(out, vec![0.5]);
    let mut out = Vec::new();
    let dropped = CpalBackend::collapse_to_mono(&[] as &[f32], |sample| sample, 2, &mut out);
    assert_eq!(dropped, 0);
    assert!(out.is_empty());
}
