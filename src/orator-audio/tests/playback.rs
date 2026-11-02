//! Blocking playout contract: silent no-op on empty input, prompt
//! return with an actionable error when no device exists — never a
//! panic, never per-sample allocation in the callback.

use crate::playback::{play_blocking, StreamPlayer};
use crate::AudioError;

#[test]
fn empty_buffer_returns_without_touching_a_device() {
    let started = std::time::Instant::now();
    play_blocking(&[], 24_000).expect("empty input is a no-op");
    play_blocking(&[], 0).expect("empty input ignores the rate");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "no device lookup, no wait"
    );
}

#[test]
fn playout_reports_instead_of_panicking() {
    // Headless CI has no output device (NoDevice); a workstation
    // drains a few milliseconds of audio (Ok). Either way the call
    // returns promptly with an actionable outcome.
    let started = std::time::Instant::now();
    match play_blocking(&vec![0.0; 240], 24_000) {
        Ok(()) => {}
        Err(AudioError::NoDevice(detail)) => {
            assert!(detail.contains("output"), "names the missing end: {detail}");
        }
        Err(error) => panic!("actionable device/stream error only: {error:?}"),
    }
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "bounded playout, never a hang"
    );
}

#[test]
fn streaming_counts_pushes_and_drains_or_reports() {
    let started = std::time::Instant::now();
    match StreamPlayer::open(24_000) {
        Ok(player) => {
            player.push(&vec![0.1; 1200]);
            player.push(&vec![0.2; 1200]);
            player.push(&[]);
            assert_eq!(player.pushed(), 2400);
            player.finish().expect("drain completes");
            assert!(
                started.elapsed() < std::time::Duration::from_secs(10),
                "bounded drain, never a hang"
            );
        }
        Err(AudioError::NoDevice(detail)) => {
            assert!(detail.contains("output"), "names the missing end: {detail}");
        }
        Err(error) => panic!("actionable device/stream error only: {error:?}"),
    }
}

#[test]
fn over_cap_pushes_drop_oldest_with_counter() {
    use crate::playback::StreamPlayer;
    assert_eq!(
        StreamPlayer::MAX_QUEUE_SAMPLES,
        480_000,
        "realtime queue cap pinned"
    );
    match StreamPlayer::open(24_000) {
        Ok(player) => {
            player.push(&vec![0.1; StreamPlayer::MAX_QUEUE_SAMPLES + 1000]);
            assert!(
                player.dropped() >= 1000,
                "over-cap drops oldest: {}",
                player.dropped()
            );
            let _ = player.finish();
        }
        Err(AudioError::NoDevice(_)) => {}
        Err(error) => panic!("actionable error only: {error:?}"),
    }
}
