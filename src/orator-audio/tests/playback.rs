//! Blocking playout contract: silent no-op on empty input, prompt
//! return with an actionable error when no device exists — never a
//! panic, never per-sample allocation in the callback.
//!
//! Queue-lag inventory: `push` returns at once into a bounded
//! drop-oldest queue while the device callback drains behind it, so
//! queued audio can precede audibility by up to the queue cap —
//! 480 000 samples, i.e. 20 s at the 24 kHz voice rate, 10 s at a
//! 48 kHz device rate. The test below pins the bound that lead
//! rests on.

use crate::playback::{play_blocking, play_blocking_abortable, StreamPlayer};
use crate::AudioError;

#[test]
fn abortable_empty_returns_drained_without_touching_a_device() {
    use std::sync::atomic::AtomicBool;
    let live = AtomicBool::new(false);
    assert!(
        play_blocking_abortable(&[], 24_000, &live).expect("empty input is a no-op"),
        "empty input drains vacuously even when not aborted"
    );
    let cut = AtomicBool::new(true);
    assert!(
        play_blocking_abortable(&[], 24_000, &cut).expect("empty input is a no-op"),
        "empty input never touches the device, pre-cancelled or not"
    );
}

#[test]
fn abortable_pre_cancelled_returns_cut_without_touching_a_device() {
    use std::sync::atomic::AtomicBool;
    // A fired abort refuses before the device opens: hermetic on
    // headless CI (no output device), prompt either way.
    let cut = AtomicBool::new(true);
    let started = std::time::Instant::now();
    assert!(
        !play_blocking_abortable(&vec![0.0; 2400], 24_000, &cut)
            .expect("cut is not an error"),
        "pre-cancelled playout cuts before the device opens"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "no device lookup, no wait"
    );
}

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

/// Push-ahead lag rests on the queue cap alone: right after two
/// over-cap pushes return, the queue still holds a full cap of
/// unplayed audio. No-device machines skip; the arithmetic bound is
/// pinned by the cap test above.
#[test]
fn push_ahead_lag_is_bounded_only_by_the_queue_cap() {
    use crate::playback::StreamPlayer;
    match StreamPlayer::open(24_000) {
        Ok(player) => {
            player.push(&vec![0.1; StreamPlayer::MAX_QUEUE_SAMPLES]);
            player.push(&vec![0.1; StreamPlayer::MAX_QUEUE_SAMPLES]);
            let ahead = player.pushed().saturating_sub(player.played());
            assert!(
                ahead >= StreamPlayer::MAX_QUEUE_SAMPLES,
                "a full cap still queued behind the callback: {ahead}"
            );
            // Drop, don't drain: the premise is the queued depth, and
            // draining 20 s of scripted audio would contend the device
            // with sibling tests. Dropping stops the stream at once.
            drop(player);
        }
        Err(AudioError::NoDevice(_)) => {}
        Err(error) => panic!("actionable error only: {error:?}"),
    }
}
