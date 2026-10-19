//! Producer-confidence reading pins: a detached gate is declared (allowed,
//! never silently healthy), an attached-but-unread gate refuses entries, an
//! observed trip refuses entries with its reason, and exits always route.

use super::*;

use crate::backfill::{BackfillHandle, BackfillReport};
use crate::command::{StateHandle, TradingState};
use crate::reconcile::{ReconcileHandle, ReconcileReport};
use crate::stream::{StreamBreaker, StreamConfig};
use crate::tastytrade::TransportError;

fn detached() -> GateReading {
    GateReading::assemble(TradingState::Enabled, None, None, None)
}

fn clean_reconcile() -> ReconcileHandle {
    let handle = ReconcileHandle::new();
    handle.observe(&ReconcileReport {
        matched: true,
        mismatches: Vec::new(),
        alerts: Vec::new(),
    });
    handle
}

fn halted_reconcile() -> ReconcileHandle {
    let handle = ReconcileHandle::new();
    handle.observe(&ReconcileReport {
        matched: false,
        mismatches: Vec::new(),
        alerts: vec!["book disagrees with venue".to_owned()],
    });
    handle
}

fn clean_backfill() -> BackfillHandle {
    let handle = BackfillHandle::new();
    handle.observe(&BackfillReport {
        complete: true,
        filled: 0,
        alerts: Vec::new(),
    });
    handle
}

fn suspended_backfill() -> BackfillHandle {
    let handle = BackfillHandle::new();
    handle.suspend("unspliced hole".to_owned());
    handle
}

fn calm_breaker() -> StreamBreaker {
    StreamBreaker::new(StreamConfig::new().build())
}

fn tripped_breaker() -> StreamBreaker {
    let breaker = calm_breaker();
    breaker.record_error(&TransportError::Terminal("line cut".to_owned()));
    assert!(breaker.tripped());
    breaker
}

#[test]
fn detached_gates_are_declared_not_silently_healthy() {
    let reading = detached();
    assert!(reading.allows_entries());
    assert!(reading.entry_refusal_reason().is_none());
    for gate in [
        &reading.trading,
        &reading.reconcile,
        &reading.backfill,
        &reading.stream,
    ] {
        assert!(gate.allows_entry);
        assert!(gate.allows_exit);
        assert!(gate.reason.is_none());
    }
}

#[test]
fn disabled_trading_halts_entries_and_routes_exits() {
    let reading = GateReading::assemble(TradingState::Disabled, None, None, None);
    assert!(!reading.allows_entries());
    assert!(reading.trading.allows_exit);
    let reason = reading.entry_refusal_reason().expect("a halt names itself");
    assert!(reason.contains("halted"), "{reason:?}");
}

#[test]
fn attached_but_unread_gates_refuse_entries() {
    let reading = GateReading::assemble(
        TradingState::Enabled,
        Some(&ReconcileHandle::new()),
        Some(&BackfillHandle::new()),
        None,
    );
    assert!(!reading.allows_entries());
    // Exits still route past an unread producer.
    assert!(reading.reconcile.allows_exit);
    assert!(reading.backfill.allows_exit);
    let reason = reading.entry_refusal_reason().expect("unread names itself");
    assert!(
        reason.contains("no report") || reason.contains("no resync"),
        "{reason:?}"
    );
}

#[test]
fn observed_clean_gates_allow() {
    let reconcile = clean_reconcile();
    let backfill = clean_backfill();
    let reading = GateReading::assemble(
        TradingState::Enabled,
        Some(&reconcile),
        Some(&backfill),
        None,
    );
    assert!(reading.allows_entries());
    assert!(reading.entry_refusal_reason().is_none());
}

#[test]
fn observed_trips_halt_entries_with_their_reason() {
    let reconcile = halted_reconcile();
    let backfill = suspended_backfill();
    let reading = GateReading::assemble(
        TradingState::Enabled,
        Some(&reconcile),
        Some(&backfill),
        None,
    );
    assert!(!reading.allows_entries());
    assert!(reading.reconcile.allows_exit);
    assert!(reading.backfill.allows_exit);
    // Trading state is checked first, then reconcile, then backfill: the
    // first refusal names the order.
    let reason = reading.entry_refusal_reason().expect("a trip names itself");
    assert!(reason.contains("disagrees"), "{reason:?}");
}

#[test]
fn tripped_stream_breaker_disarms_entries_only() {
    let breaker = tripped_breaker();
    let reading = GateReading::assemble(TradingState::Enabled, None, None, Some(&breaker));
    assert!(!reading.allows_entries());
    assert!(reading.stream.allows_exit);
    let reason = reading.entry_refusal_reason().expect("a trip names itself");
    assert!(reason.contains("breaker"), "{reason:?}");

    let calm = GateReading::assemble(TradingState::Enabled, None, None, Some(&calm_breaker()));
    assert!(calm.allows_entries());
}

#[test]
fn state_handle_flows_into_the_reading() {
    let state = StateHandle::new(TradingState::Enabled);
    let reading = GateReading::assemble(state.get(), None, None, None);
    assert!(reading.allows_entries());
    state.set(TradingState::ExitOnly);
    let reading = GateReading::assemble(state.get(), None, None, None);
    assert!(!reading.allows_entries());
    assert!(reading.trading.allows_exit);
}
