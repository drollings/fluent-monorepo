//! Jitter tests: drop-oldest on overflow, growth on underrun, exact stats.

use orator::{Channel, Frame};

use crate::jitter::JitterBuffer;

fn frame(value: f32) -> Frame {
    Frame::new(Channel::Mic, vec![value; 320], 16_000).expect("valid rate builds")
}

#[test]
fn overflow_drops_oldest_and_counts() {
    let mut jitter = JitterBuffer::for_20ms_frames();
    for index in 0..6 {
        jitter.push(frame(index as f32));
    }
    // Ceiling starts at 1+1+1=3: pushes 3, 4, 5 each drop one frame to the
    // 1+1 target while the ceiling adapts upward.
    assert_eq!(jitter.drops, 3);
    let first = jitter.pop().expect("buffered audio");
    assert!((first.samples[0] - 3.0).abs() < f32::EPSILON);
}

#[test]
fn fresh_frames_are_not_dropped() {
    let mut jitter = JitterBuffer::for_20ms_frames();
    jitter.push(frame(1.0));
    jitter.push(frame(2.0));
    assert_eq!(jitter.drops, 0);
    assert!((jitter.pop().expect("first").samples[0] - 1.0).abs() < f32::EPSILON);
    assert!((jitter.pop().expect("second").samples[0] - 2.0).abs() < f32::EPSILON);
    assert!(jitter.pop().is_none());
}

#[test]
fn underrun_grows_the_floor_and_counts() {
    let mut jitter = JitterBuffer::for_20ms_frames();
    assert!(jitter.pop().is_none());
    assert_eq!(jitter.underruns, 1);
}

#[test]
fn depth_stats_track_min_and_max() {
    let mut jitter = JitterBuffer::for_20ms_frames();
    jitter.push(frame(1.0));
    jitter.push(frame(2.0));
    assert!(jitter.max_depth_samples >= 640);
    assert!(jitter.min_depth_samples <= 640);
    jitter.pop();
    jitter.pop();
    assert_eq!(jitter.min_depth_samples, 0);
}

#[test]
fn eighty_ms_cap_bounds_growth() {
    use crate::jitter::JitterBuffer;
    let mut jitter = JitterBuffer::for_20ms_frames();
    for index in 0..20 {
        jitter.push(frame(index as f32));
    }
    // Ceiling adapts 1+1+max with max capped at 4 frames (80 ms):
    // length stays bounded and every excess frame is counted.
    assert!(jitter.len() <= 6, "bounded at ceiling: {}", jitter.len());
    assert_eq!(
        jitter.drops,
        20 - jitter.len() as u64,
        "every over-cap frame drops with a counter"
    );
    assert!(jitter.max_depth_samples <= 6 * 320, "depth tracks the cap");
}
