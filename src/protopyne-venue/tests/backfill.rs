//! Backfill primitives: the missing-range math, the idempotent splice, and
//! the ordered ladder walk — without any sensor or session above them.

use super::*;
use crate::{BackfillSource, VenueError};
use protopyne_types::{Bar, BarGap, GapPolicy, SeriesContext, Timeframe};
use std::sync::Arc;

fn bar_at(minute: u32) -> Bar {
    Bar::new()
        .ts(chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2026, 6, 1, 12, minute, 0).unwrap())
        .o("600.00".parse().expect("decimal"))
        .h("601.00".parse().expect("decimal"))
        .l("599.00".parse().expect("decimal"))
        .c("600.50".parse().expect("decimal"))
        .volume(1_000)
        .maybe_adj(None)
        .build()
}

fn dense_ctx_through(minute: u32) -> SeriesContext {
    let mut ctx = SeriesContext::new("SPY", Timeframe::M1, GapPolicy::Dense);
    for minute in 0..=minute {
        ctx.push(bar_at(minute)).expect("contiguous pushes hold");
    }
    ctx
}

fn gap_over(low_minute: u32, high_minute: u32, missing: u64) -> BarGap {
    BarGap::new()
        .at_index(3)
        .expected_ts(bar_at(low_minute + 1).ts)
        .actual_ts(bar_at(high_minute).ts)
        .missing_bars(missing)
        .build()
}

struct Scripted {
    bars: Vec<Bar>,
    error: Option<VenueError>,
}

impl BackfillSource for Scripted {
    fn fetch(&self, _start_ms: i64, _end_ms: i64) -> Result<Vec<Bar>, VenueError> {
        match &self.error {
            Some(error) => Err(error.clone()),
            None => Ok(self.bars.clone()),
        }
    }
}

/// The missing range names the hole in bar terms: start at the first
/// missing bar's stamp, count exactly the hole.
#[test]
fn missing_range_names_the_hole() {
    let bounds = missing_range(&gap_over(2, 6, 3), Timeframe::M1, 1);
    assert_eq!(
        bounds.original_start_ms,
        Some(bar_at(3).ts.timestamp_millis())
    );
    assert_eq!(bounds.original_limit, Some(3));
}

/// Warmup headroom widens the fetch start without moving the hole: the
/// widened prefix is already-present history the splice skips.
#[test]
fn warmup_headroom_widens_the_fetch_start() {
    let plain = missing_range(&gap_over(5, 9, 3), Timeframe::M1, 1);
    let warmed = missing_range(&gap_over(5, 9, 3), Timeframe::M1, 4);
    assert!(
        warmed.fetch_start_ms < plain.fetch_start_ms,
        "headroom backs the fetch off, got {:?} vs {:?}",
        warmed.fetch_start_ms,
        plain.fetch_start_ms
    );
    assert_eq!(
        warmed.original_limit,
        Some(3),
        "the hole count never widens"
    );
}

/// The splice lands hole bars in order, skips already-present headroom,
/// and refuses a bar that jumps the hole instead of filling it.
#[test]
fn splice_lands_skips_and_refuses() {
    let mut ctx = dense_ctx_through(2);
    let landed = splice(&mut ctx, vec![bar_at(0), bar_at(1), bar_at(3), bar_at(4)])
        .expect("in-order hole bars splice");
    assert_eq!(landed, 2, "present headroom skips, only the hole counts");
    assert_eq!(ctx.len(), 5);
    splice(&mut ctx, vec![bar_at(9)]).expect_err("jumping the hole refuses");
    assert_eq!(ctx.len(), 5, "a refused splice mutates nothing");
}

/// The ladder takes the first rung holding bars, skips empty rungs, walks
/// past retryable errors, and stops at terminal ones.
#[test]
fn ladder_prefers_skip_and_stop_in_order() {
    let empty: Arc<dyn BackfillSource> = Arc::new(Scripted {
        bars: Vec::new(),
        error: None,
    });
    let full: Arc<dyn BackfillSource> = Arc::new(Scripted {
        bars: vec![bar_at(3)],
        error: None,
    });
    let found = fetch_through(&[empty.clone(), full.clone()], 0, 1).expect("second rung serves");
    assert_eq!(found.len(), 1);

    let retryable: Arc<dyn BackfillSource> = Arc::new(Scripted {
        bars: Vec::new(),
        error: Some(VenueError::Transport("dark".to_owned())),
    });
    let found = fetch_through(&[retryable, full.clone()], 0, 1).expect("retryable walks on");
    assert_eq!(found.len(), 1);

    let terminal: Arc<dyn BackfillSource> = Arc::new(Scripted {
        bars: Vec::new(),
        error: Some(VenueError::Invalid("bad range".to_owned())),
    });
    fetch_through(&[terminal, full], 0, 1).expect_err("terminal stops the walk");

    let nothing: Arc<dyn BackfillSource> = Arc::new(Scripted {
        bars: Vec::new(),
        error: None,
    });
    fetch_through(&[nothing], 0, 1).expect_err("no bars anywhere stays a failure");
}

/// Gate suspends on a hole, refuses entries, clears on a complete report,
/// and drains without disarming (M9.2a transition pin, mirroring the
/// reconcile gate's pin).
#[test]
fn gate_suspends_entries_until_the_series_is_contiguous() {
    use crate::backfill::{BackfillGate, BackfillReport};
    let mut gate = BackfillGate::default();
    assert!(!gate.entries_suspended());
    assert!(gate.check_entries().is_ok());
    assert!(gate.backfilled());

    gate.observe(&BackfillReport {
        complete: false,
        filled: 0,
        alerts: vec!["hole [1, 2, 3]".to_owned()],
    });
    assert!(gate.entries_suspended());
    assert!(gate.check_entries().is_err());
    assert!(!gate.backfilled());
    assert_eq!(gate.alerts().len(), 1);

    gate.observe(&BackfillReport {
        complete: true,
        filled: 3,
        alerts: Vec::new(),
    });
    assert!(!gate.entries_suspended());
    assert!(gate.check_entries().is_ok());
    assert!(gate.backfilled());
}

/// Delivery drains without touching the suspension (M9.2a).
#[test]
fn gate_delivery_drains_without_clearing_the_suspension() {
    use crate::backfill::BackfillGate;
    let mut gate = BackfillGate::default();
    gate.suspend("reconnect".to_owned());
    assert!(gate.is_observed());
    assert!(gate.entries_suspended());
    gate.suspend("still catching up".to_owned());
    assert_eq!(gate.alerts().len(), 2);
    let delivered = gate.drain_alerts();
    assert_eq!(delivered.len(), 2);
    assert!(gate.alerts().is_empty(), "delivery drains the bank");
    assert!(
        gate.entries_suspended(),
        "draining never clears the suspension"
    );
    assert!(gate.drain_alerts().is_empty());
}
