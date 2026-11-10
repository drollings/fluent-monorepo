//! Segmentation tests: window math, overlap, tails, mic shortcut.

use orator::Channel;

use crate::diary::segment::{segment, HOP_SAMPLES, WINDOW_SAMPLES};

#[test]
fn windows_overlap_by_half() {
    assert_eq!(WINDOW_SAMPLES, 24_000);
    assert_eq!(HOP_SAMPLES, 12_000);
    let segments = segment(Channel::Sys, 60_000);
    assert!(segments.len() >= 3);
    assert_eq!(segments[0].start_sample, 0);
    assert_eq!(segments[1].start_sample, HOP_SAMPLES);
    assert_eq!(segments[0].end_sample, WINDOW_SAMPLES);
}

#[test]
fn short_tails_merge() {
    // 35 000 samples: the third window would hold only 11 000, so it
    // joins the previous segment instead of standing alone.
    let segments = segment(Channel::Sys, 35_000);
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[1].end_sample, 35_000);
}

#[test]
fn mic_channel_shortcuts_to_user() {
    let segments = segment(Channel::Mic, 48_000);
    assert!(!segments.is_empty());
    assert!(segments.iter().all(super::Segment::is_user_channel));
    assert!(segments.iter().all(|segment| segment.midpoint() > segment.start_sample));
}

#[test]
fn empty_audio_segments_nothing() {
    assert!(segment(Channel::Sys, 0).is_empty());
}
