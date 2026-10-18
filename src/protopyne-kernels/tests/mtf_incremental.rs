//! Incremental frames: an append-at-a-time streaming build must equal the
//! from-scratch build exactly (frames, caches, and values — revisions
//! modulo the monotonic counter, whose absolute values depend on bump
//! history, not content), fence edges must confirm exactly at `now`, and a
//! no-op append must leave every revision untouched.
//!
//! The properties below fail against full-rebuild-per-`ensure` drift (a
//! forming bucket leaking in, a tail bucket missed on `now` advance, a
//! spurious bump on identical recompute) and pass only when the append path
//! extends confirmed buckets incrementally.
//! `#[allow(clippy::float_cmp)]` covers the hand-computed SMA literals.
#![allow(clippy::float_cmp)]

use super::*;
use crate::indicator::{builtin_registry, ParamSet};
use protopyne_types::Bar;
use rust_decimal::Decimal;

/// Minute-aligned base for synthetic tapes.
const T0: i64 = 1_699_999_980_000;
const MIN: i64 = 60_000;
const FIVE: i64 = 5 * MIN;
const FIFTEEN: i64 = 15 * MIN;

/// One synthetic closed bar: arithmetic-progression OHLCV (hand-computable
/// aggregates), strictly advancing stamps.
fn bar_at(open_ms: i64, step: i64, volume: u64) -> Bar {
    let cents = 10_000 + step * 5;
    Bar::new()
        .ts(chrono::DateTime::from_timestamp(open_ms / 1000, 0).expect("in range"))
        .o(Decimal::new(cents, 2))
        .h(Decimal::new(cents + 12, 2))
        .l(Decimal::new(cents - 12, 2))
        .c(Decimal::new(cents + 2, 2))
        .volume(volume)
        .maybe_adj(None)
        .build()
}

fn tape(count: i64) -> Vec<Bar> {
    (0..count)
        .map(|i| bar_at(T0 + i * MIN, i, 100 * (i as u64 + 1)))
        .collect()
}

/// SMA-14 requests over the 5- and 15-minute spans (builtin kernel, so the
/// property measures the store, never a double).
fn requests() -> Vec<(SeriesKey, ParamSet)> {
    let params = ParamSet::new().with_int("length", 14);
    vec![
        (
            SeriesKey::new(FIVE, "sma", &params).expect("span valid"),
            params.clone(),
        ),
        (
            SeriesKey::new(FIFTEEN, "sma", &params).expect("span valid"),
            params,
        ),
    ]
}

fn ensure_now(
    store: &mut MtfStore,
    requests: &[(SeriesKey, ParamSet)],
    now_ms: i64,
) -> Vec<SeriesKey> {
    let registry = builtin_registry().expect("builtins load");
    store
        .ensure(&registry, requests, now_ms, Lookahead::Off)
        .expect("ensure runs")
}

/// One frame's observable legs: stamps plus five OHLCV legs. Plain
/// struct (not a tuple) so equality across stores reads directly.
#[derive(Debug, Clone, PartialEq)]
struct FrameSnapshot {
    stamps: Vec<i64>,
    open: Vec<f64>,
    high: Vec<f64>,
    low: Vec<f64>,
    close: Vec<f64>,
    volume: Vec<f64>,
}

/// Frame legs for equality across stores.
fn frame_legs(store: &MtfStore, tf_ms: i64) -> FrameSnapshot {
    let stamps = store
        .frame_stamps(tf_ms)
        .expect("frame materialized")
        .to_vec();
    let frame = store.frame(tf_ms).expect("frame legs present");
    FrameSnapshot {
        stamps,
        open: frame.open.clone(),
        high: frame.high.clone(),
        low: frame.low.clone(),
        close: frame.close.clone(),
        volume: frame.volume.clone(),
    }
}

/// Cached series values for one key.
fn cached_values(store: &MtfStore, key: &SeriesKey) -> Vec<Option<f64>> {
    store.cache(key).expect("series computed").values.clone()
}

#[test]
fn incremental_append_matches_from_scratch() {
    let bars = tape(40);
    let reqs = requests();
    // Streaming build: 25-bar base, then one bar appended at a time with
    // `now` tracking each append's close (every bucket confirmed on arrival).
    let mut streamed = MtfStore::new();
    streamed.set_base(&bars[..25]).expect("base loads");
    let mut ordered_first = ensure_now(&mut streamed, &reqs, T0 + 25 * MIN);
    ordered_first.sort_by_key(|key| (key.tf_ms, key.kernel.clone()));
    for (at, bar) in bars.iter().enumerate().skip(25) {
        streamed.append_bar(bar).expect("tape advances");
        let ordered = ensure_now(&mut streamed, &reqs, bar.ts.timestamp_millis() + MIN);
        let mut sorted = ordered.clone();
        sorted.sort_by_key(|key| (key.tf_ms, key.kernel.clone()));
        assert_eq!(sorted, ordered_first, "request order stable at append {at}");
    }
    // From-scratch build over the whole tape at the same final `now`.
    let final_now = bars[39].ts.timestamp_millis() + MIN;
    let mut fresh = MtfStore::new();
    fresh.set_base(&bars).expect("base loads");
    let ordered = ensure_now(&mut fresh, &reqs, final_now);
    let mut sorted = ordered.clone();
    sorted.sort_by_key(|key| (key.tf_ms, key.kernel.clone()));
    assert_eq!(sorted, ordered_first, "request order matches from-scratch");
    // Observable state equal per span and per series; revisions excluded —
    // absolute counter values depend on bump history, not content.
    for tf_ms in [FIVE, FIFTEEN] {
        assert_eq!(
            frame_legs(&streamed, tf_ms),
            frame_legs(&fresh, tf_ms),
            "span {tf_ms} frame equal"
        );
    }
    for (key, _) in &reqs {
        assert_eq!(
            cached_values(&streamed, key),
            cached_values(&fresh, key),
            "series {key} equal"
        );
    }
}

#[test]
fn fence_bar_closing_exactly_at_now_confirms() {
    let bars = tape(10);
    let reqs = requests();
    let mut store = MtfStore::new();
    store.set_base(&bars).expect("base loads");
    // First 5m bucket grid-floors before T0 (T0 is minute- but not
    // span-aligned); it closes exactly at `fence`.
    let grid = std::num::NonZeroI64::new(FIVE).expect("span positive");
    let bucket = floor_to_grid(T0, grid);
    let fence = bucket + FIVE;
    // One millisecond early the bucket is forming and excluded; on the
    // fence it confirms — `open + span <= now` counts the exact close.
    ensure_now(&mut store, &reqs, fence - 1);
    let early: Vec<i64> = store.frame_stamps(FIVE).expect("frame present").to_vec();
    assert!(!early.contains(&bucket), "forming bucket excluded at now-1");
    ensure_now(&mut store, &reqs, fence);
    let fenced: Vec<i64> = store.frame_stamps(FIVE).expect("frame present").to_vec();
    assert!(
        fenced.contains(&bucket),
        "fence-close bucket confirms exactly at now"
    );
    assert_eq!(
        fenced.len(),
        early.len() + 1,
        "only the fence bucket newly confirms"
    );
}

#[test]
fn forming_tail_excluded_then_confirms_on_now_advance() {
    let bars = tape(12);
    let reqs = requests();
    // Grid-floored 15m buckets: the head bucket is confirmed while `now`
    // sits mid-bucket; the tail bucket stays out.
    let grid = std::num::NonZeroI64::new(FIFTEEN).expect("span positive");
    let head = floor_to_grid(T0, grid);
    let tail = head + FIFTEEN;
    let mid_now = T0 + 11 * MIN + 30_000;
    assert!(
        head + FIFTEEN <= mid_now && mid_now < tail + FIFTEEN,
        "now sits mid-tail-bucket"
    );
    let mut store = MtfStore::new();
    store.set_base(&bars).expect("base loads");
    ensure_now(&mut store, &reqs, mid_now);
    let revision_before = store.revision();
    let forming: Vec<i64> = store.frame_stamps(FIFTEEN).expect("frame present").to_vec();
    assert!(forming.contains(&head), "confirmed head present");
    assert!(!forming.contains(&tail), "forming tail excluded");
    // Advance `now` to the tail's exact close with no base change: the tail
    // confirms, and the result equals a from-scratch build at the new `now`.
    let late_now = tail + FIFTEEN;
    ensure_now(&mut store, &reqs, late_now);
    let confirmed: Vec<i64> = store.frame_stamps(FIFTEEN).expect("frame present").to_vec();
    assert!(
        confirmed.contains(&tail),
        "advanced fence confirms the tail"
    );
    let mut fresh = MtfStore::new();
    fresh.set_base(&bars).expect("base loads");
    ensure_now(&mut fresh, &reqs, late_now);
    assert_eq!(
        frame_legs(&store, FIFTEEN),
        frame_legs(&fresh, FIFTEEN),
        "tail-only advance matches"
    );
    assert!(
        store.revision() >= revision_before,
        "revision never runs backward across the fence"
    );
}

#[test]
fn unchanged_append_leaves_revisions_untouched() {
    let bars = tape(30);
    let reqs = requests();
    let mut store = MtfStore::new();
    store.set_base(&bars[..25]).expect("base loads");
    // `now` mid-bucket: buckets through [T0+20m, T0+25m) confirmed.
    let now_ms = T0 + 25 * MIN + 30_000;
    ensure_now(&mut store, &reqs, now_ms);
    let settled = store.revision();
    // Same `now`, same base: pure no-op.
    ensure_now(&mut store, &reqs, now_ms);
    assert_eq!(
        store.revision(),
        settled,
        "identical ensure backdates everything"
    );
    // Append bars that complete no new bucket at this `now`: the forming
    // bucket extends, confirmed output stands still, revisions hold.
    for bar in bars.iter().skip(25).take(3) {
        store.append_bar(bar).expect("tape advances");
        ensure_now(&mut store, &reqs, now_ms);
        assert_eq!(
            store.revision(),
            settled,
            "bucket-incomplete append bumps nothing"
        );
    }
    for tf_ms in [FIVE, FIFTEEN] {
        let stamps = store.frame_stamps(tf_ms).expect("frame present");
        assert!(
            stamps.iter().all(|stamp| stamp + tf_ms <= now_ms),
            "no forming bucket leaks"
        );
    }
}

#[test]
fn set_base_replace_rebuilds_changed_history() {
    let bars = tape(30);
    let reqs = requests();
    let now_ms = T0 + 30 * MIN;
    let mut store = MtfStore::new();
    store.set_base(&bars).expect("base loads");
    ensure_now(&mut store, &reqs, now_ms);
    let before = frame_legs(&store, FIVE);
    let settled = store.revision();
    // Replace history with one bar's close nudged: the rebuild must reflect
    // the new content (a stale prefix would serve the old buckets).
    let mut edited = bars.clone();
    edited[7] = bar_at(T0 + 7 * MIN, 7 + 40, 800);
    store.set_base(&edited).expect("replacement loads");
    ensure_now(&mut store, &reqs, now_ms);
    let after = frame_legs(&store, FIVE);
    assert_ne!(before, after, "replaced history rebuilds buckets");
    assert!(
        store.revision() > settled,
        "changed content bumps the revision"
    );
    // Replacing with identical content backdates instead of bumping.
    let quiet = store.revision();
    store
        .set_base(&edited)
        .expect("identical replacement loads");
    ensure_now(&mut store, &reqs, now_ms);
    assert_eq!(store.revision(), quiet, "identical replace backdates");
}
