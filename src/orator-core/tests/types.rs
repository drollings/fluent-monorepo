//! Unit tests for [`crate::types`]: timestamps, sharing, shapes.

use std::num::NonZeroU32;
use std::sync::Arc;

use crate::types::{Channel, Event, EventKind, Frame, MediaTime, Word, WordUpdate};

fn rate() -> NonZeroU32 {
    NonZeroU32::new(16_000).expect("test rate nonzero")
}

#[test]
fn media_time_measures_seconds() {
    let stamp = MediaTime {
        samples: 16_000,
        rate: rate(),
    };
    assert!((stamp.as_secs_f64() - 1.0).abs() < f64::EPSILON);
}

#[test]
fn media_time_add_saturates() {
    let stamp = MediaTime {
        samples: u64::MAX,
        rate: rate(),
    };
    assert_eq!(stamp.add_samples(1).samples, u64::MAX);
    assert_eq!(MediaTime::zero(rate()).add_samples(320).samples, 320);
}

#[test]
fn media_time_orders() {
    let early = MediaTime::zero(rate());
    let late = early.add_samples(320);
    assert!(early < late);
}

#[test]
fn frame_shares_samples_without_copying() {
    let frame = Frame::new(Channel::Mic, vec![0.0; 320], 16_000).expect("valid rate builds");
    assert_eq!(frame.len(), 320);
    assert!(!frame.is_empty());
    let clone = frame.clone();
    assert!(Arc::ptr_eq(&frame.samples, &clone.samples));
    assert_eq!(frame.channel, Channel::Mic);
}

#[test]
fn frame_rejects_a_zero_sample_rate() {
    let error = Frame::new(Channel::Mic, vec![0.0; 320], 0).expect_err("zero rate is not audio");
    assert_eq!(error, crate::socket::FrameError::InvalidSampleRate(0));
    assert!(error.to_string().contains('0'));
    // Valid rates still build with identical shape.
    let frame = Frame::new(Channel::Mic, vec![], 16_000).expect("valid rate builds");
    assert!(frame.is_empty());
    assert_eq!(frame.sample_rate, 16_000);
}

#[test]
fn word_update_marks_provisional_tail() {
    let stamp = MediaTime::zero(rate());
    let update = WordUpdate {
        word: Word {
            text: "hello".to_string(),
            start: stamp,
            end: stamp.add_samples(320),
            confidence: 0.9,
            channel: Channel::Mic,
        },
        provisional: true,
    };
    assert!(update.provisional);
    assert_eq!(update.word.channel, Channel::Mic);
}

#[test]
fn channels_stay_distinct() {
    assert_ne!(Channel::Mic, Channel::Sys);
    assert_ne!(Channel::Sys, Channel::Reply);
}

fn event(seq: i64, kind: EventKind) -> Event {
    Event {
        seq,
        session: "s".to_string(),
        epoch: 1,
        wall_ms: 0,
        kind,
    }
}

#[test]
fn durable_events_persist_and_live_ones_do_not() {
    assert!(event(0, EventKind::SessionStarted).durable());
    assert!(event(41, EventKind::BargeIn).durable());
    assert!(!event(-1, EventKind::SttPartial("hi".to_string())).durable());
    assert!(!event(-2, EventKind::LlmDelta("tok".to_string())).durable());
    assert!(!event(7, EventKind::VadStart).durable());
}
