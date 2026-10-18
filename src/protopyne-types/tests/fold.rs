//! The complete-bucket rule, one home: `BucketAccum` against a slice reference.
//!
//! The streaming accumulator must agree with a straightforward slice-based
//! reading of the same rule on every bucket of a randomized corpus — whole,
//! partial, torn, and lone-child alike. The control group below names the
//! buckets that must NOT be complete; the corpus sweep proves the agreement
//! holds beyond the hand-picked cases.

use super::*;
use crate::Timeframe;

/// Straightforward slice reading of the complete-bucket rule: the first child
/// opens the bucket, every child gap is the same positive span, and the
/// children count out the full bucket span.
fn reference_complete(stamps: &[i64], start_ms: i64, step_ms: i64) -> bool {
    let Some((&first, rest)) = stamps.split_first() else {
        return false;
    };
    if first != start_ms || rest.is_empty() {
        return false;
    }
    let gap = rest[0] - first;
    if gap <= 0 {
        return false;
    }
    let mut prev = rest[0];
    for &stamp in &rest[1..] {
        if stamp - prev != gap {
            return false;
        }
        prev = stamp;
    }
    (stamps.len() as i64).checked_mul(gap) == Some(step_ms)
}

fn accumulate(stamps: &[i64], start_ms: i64) -> BucketAccum {
    let mut accum = BucketAccum::new(start_ms);
    for &stamp in stamps {
        accum.absorb(stamp);
    }
    accum
}

#[test]
fn a_whole_bucket_is_complete() {
    let step = Timeframe::M5.duration_ms();
    let start = Timeframe::M5.bucket_start_ms(1_700_000_000_000);
    let stamps: Vec<i64> = (0..5).map(|i| start + i * 60_000).collect();
    assert!(accumulate(&stamps, start).is_complete(step));
}

#[test]
fn a_short_leading_edge_is_not_complete() {
    let step = Timeframe::M5.duration_ms();
    let start = Timeframe::M5.bucket_start_ms(1_700_000_000_000);
    let stamps: Vec<i64> = (1..5).map(|i| start + i * 60_000).collect();
    assert!(!accumulate(&stamps, start).is_complete(step));
}

#[test]
fn a_short_trailing_edge_is_not_complete() {
    let step = Timeframe::M5.duration_ms();
    let start = Timeframe::M5.bucket_start_ms(1_700_000_000_000);
    let stamps: Vec<i64> = (0..4).map(|i| start + i * 60_000).collect();
    assert!(!accumulate(&stamps, start).is_complete(step));
}

#[test]
fn a_torn_bucket_is_not_complete() {
    let step = Timeframe::M5.duration_ms();
    let start = Timeframe::M5.bucket_start_ms(1_700_000_000_000);
    let stamps: Vec<i64> = [0, 2, 3, 4].iter().map(|i| start + i * 60_000).collect();
    assert!(!accumulate(&stamps, start).is_complete(step));
}

#[test]
fn a_lone_child_is_not_complete() {
    let step = Timeframe::H1.duration_ms();
    let start = Timeframe::H1.bucket_start_ms(1_700_000_000_000);
    assert!(!accumulate(&[start], start).is_complete(step));
    assert!(!accumulate(&[], start).is_complete(step));
}

#[test]
fn an_empty_accumulator_is_not_complete() {
    let step = Timeframe::M5.duration_ms();
    let start = Timeframe::M5.bucket_start_ms(1_700_000_000_000);
    assert!(!BucketAccum::new(start).is_complete(step));
}

/// Deterministic xorshift: randomized corpus without a dev-dependency.
fn next_rand(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *state >> 33
}

#[test]
fn the_accumulator_agrees_with_the_slice_reference_on_every_bucket() {
    let steps = [
        Timeframe::M5.duration_ms(),
        Timeframe::M15.duration_ms(),
        Timeframe::H1.duration_ms(),
        Timeframe::D1.duration_ms(),
    ];
    let anchors = [
        Timeframe::H1.bucket_start_ms(1_700_000_000_000),
        Timeframe::H1.bucket_start_ms(1_700_000_000_000) + 7 * 60_000,
        Timeframe::W1.bucket_start_ms(-1_000_000_000),
    ];
    let mut rng = 0x9E37_79B9_7F4A_7C15_u64;
    let mut buckets_checked = 0_usize;
    for &step in &steps {
        let target = if step == 300_000 {
            Timeframe::M5
        } else if step == 900_000 {
            Timeframe::M15
        } else if step == 3_600_000 {
            Timeframe::H1
        } else {
            Timeframe::D1
        };
        for &anchor in &anchors {
            for holes in [0_u64, 1, 7] {
                let mut stamps = Vec::new();
                let mut cursor = anchor;
                for _ in 0..400 {
                    // Mixed child spacings: one- and two-minute grids, so the
                    // accumulator must read the gap off the children, never
                    // assume it.
                    cursor += if next_rand(&mut rng) % 2 == 0 {
                        60_000
                    } else {
                        120_000
                    };
                    if holes != 0 && next_rand(&mut rng) % 11 < holes {
                        continue;
                    }
                    stamps.push(cursor);
                }
                // Off-grid leading edges: start some buckets mid-bucket.
                if next_rand(&mut rng) % 2 == 0 && !stamps.is_empty() {
                    stamps.remove(0);
                }
                let mut bucketed: std::collections::BTreeMap<i64, Vec<i64>> =
                    std::collections::BTreeMap::new();
                for &stamp in &stamps {
                    bucketed
                        .entry(target.bucket_start_ms(stamp))
                        .or_default()
                        .push(stamp);
                }
                for (start, bucket) in &bucketed {
                    let actual = accumulate(bucket, *start).is_complete(step);
                    let expected = reference_complete(bucket, *start, step);
                    assert_eq!(
                        actual, expected,
                        "accumulator disagrees on bucket {start} ({step}ms): {bucket:?}"
                    );
                    buckets_checked += 1;
                }
            }
        }
    }
    assert!(
        buckets_checked > 1_000,
        "the sweep must cover buckets, saw {buckets_checked}"
    );
}
