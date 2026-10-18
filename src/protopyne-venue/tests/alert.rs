//! Out-of-band alerting: breaker trips and gate halts leave the UI.
//!
//! Protocol: the terminal never carries alerts — a dead UI must not mean a
//! silent book. `poll_alerts` collects what tripped or halted into typed
//! alerts (each naming its side of the threshold split) and delivers them
//! through a sink the operator reads elsewhere. Collection drains the gate
//! banks, so every poll delivers each banked line exactly once.

use super::*;
use crate::command::{StateHandle, TradingState};
use crate::reconcile::{ReconcileHandle, ReconcileMismatch, ReconcileReport};
use crate::stream::{StreamBreaker, StreamConfig};
use crate::tastytrade::TransportError;
use chrono::{TimeZone, Utc};

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 5).unwrap()
}

fn watch<'a>(
    breaker: Option<&'a StreamBreaker>,
    reconcile: Option<&'a ReconcileHandle>,
    state: Option<&'a StateHandle>,
) -> GateWatch<'a> {
    GateWatch {
        breaker,
        reconcile,
        backfill: None,
        state,
    }
}

fn mismatch_report(line: &str) -> ReconcileReport {
    ReconcileReport {
        matched: false,
        mismatches: vec![ReconcileMismatch {
            underlying: "SPY".to_owned(),
            journal_qty: 2,
            venue_qty: 0,
            reason: "scripted drift".to_owned(),
        }],
        alerts: vec![line.to_owned()],
    }
}

/// A tripped breaker delivers one producer-confidence alert naming its
/// reason; later polls stay silent (the trip is sticky, the alert is not
/// repeated).
#[test]
fn breaker_trip_delivers_one_confidence_alert_then_silence() {
    let breaker = StreamBreaker::new(StreamConfig::new().build());
    breaker.record_error(&TransportError::Terminal(
        "unauthorized: scripted".to_owned(),
    ));
    assert!(breaker.tripped());

    let sink = MemorySink::new();
    let mut cursor = AlertCursor::new();
    let sent = poll_alerts(
        &mut cursor,
        &watch(Some(&breaker), None, None),
        &sink,
        now(),
    )
    .expect("delivery");
    assert_eq!(sent, 1);
    let alerts = sink.alerts();
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].source, AlertSource::BreakerTrip);
    assert_eq!(alerts[0].class, AlertClass::ProducerConfidence);
    assert_eq!(
        alerts[0].class,
        alerts[0].source.class(),
        "the source names its own side"
    );
    assert!(
        alerts[0].detail.contains("unauthorized"),
        "got: {}",
        alerts[0].detail
    );

    let sent = poll_alerts(
        &mut cursor,
        &watch(Some(&breaker), None, None),
        &sink,
        now(),
    )
    .expect("delivery");
    assert_eq!(sent, 0, "a reported trip never re-alerts");
    assert_eq!(sink.alerts().len(), 1);
}

/// A reconcile mismatch drains into correctness alerts while the halt
/// itself stays set: delivery never clears the gate.
#[test]
fn reconcile_mismatch_drains_into_correctness_alerts_without_clearing_the_halt() {
    let gate = ReconcileHandle::new();
    gate.observe(&mismatch_report("RECONCILE HALT: scripted"));
    assert!(gate.entries_halted());

    let sink = MemorySink::new();
    let mut cursor = AlertCursor::new();
    let sent =
        poll_alerts(&mut cursor, &watch(None, Some(&gate), None), &sink, now()).expect("delivery");
    assert_eq!(sent, 1);
    let alerts = sink.alerts();
    assert_eq!(alerts[0].source, AlertSource::ReconcileMismatch);
    assert_eq!(alerts[0].class, AlertClass::Correctness);
    assert!(gate.entries_halted(), "delivery never clears the halt");

    let sent =
        poll_alerts(&mut cursor, &watch(None, Some(&gate), None), &sink, now()).expect("delivery");
    assert_eq!(sent, 0, "the bank drained exactly once");
}

/// The kill switch alerts on the transition into halt, not on the level:
/// disarm alerts, polls while halted stay silent, resume is silent, and a
/// second disarm alerts again.
#[test]
fn kill_switch_transition_alerts_once_across_cycles() {
    let state = StateHandle::new(TradingState::Enabled);
    let sink = MemorySink::new();
    let mut cursor = AlertCursor::new();

    state.set(TradingState::Disabled);
    let sent =
        poll_alerts(&mut cursor, &watch(None, None, Some(&state)), &sink, now()).expect("delivery");
    assert_eq!(sent, 1);
    assert_eq!(sink.alerts()[0].source, AlertSource::KillSwitch);
    assert_eq!(sink.alerts()[0].class, AlertClass::Correctness);

    let sent =
        poll_alerts(&mut cursor, &watch(None, None, Some(&state)), &sink, now()).expect("delivery");
    assert_eq!(sent, 0, "a held halt never re-alerts");

    state.set(TradingState::Enabled);
    let sent =
        poll_alerts(&mut cursor, &watch(None, None, Some(&state)), &sink, now()).expect("delivery");
    assert_eq!(sent, 0, "resume is not an alert");

    state.set(TradingState::Disabled);
    let sent =
        poll_alerts(&mut cursor, &watch(None, None, Some(&state)), &sink, now()).expect("delivery");
    assert_eq!(sent, 1, "a second halt alerts again");
}

/// The file sink appends one JSON object per line, parseable without the
/// platform: the alert survives the UI by construction.
#[test]
fn file_sink_appends_parseable_json_lines() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("alerts.jsonl");
    let sink = FileSink::new(path.clone());

    let breaker = StreamBreaker::new(StreamConfig::new().build());
    breaker.record_error(&TransportError::Terminal("gone: scripted".to_owned()));
    let mut cursor = AlertCursor::new();
    poll_alerts(
        &mut cursor,
        &watch(Some(&breaker), None, None),
        &sink,
        now(),
    )
    .expect("delivery");

    let text = std::fs::read_to_string(&path).expect("alert file");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "one alert, one line, got: {text:?}");
    let parsed: serde_json::Value = serde_json::from_str(lines[0]).expect("JSON line");
    assert_eq!(parsed["source"], "BreakerTrip", "got: {parsed}");
    assert_eq!(parsed["class"], "ProducerConfidence", "got: {parsed}");
    assert!(
        parsed["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("gone"),
        "got: {parsed}"
    );
}

/// Fan-out attempts every sink even when one is dark, and reports the
/// failure by name instead of swallowing it.
#[test]
fn fanout_attempts_every_sink_and_reports_the_failure() {
    struct Dark;
    impl AlertSink for Dark {
        fn send(&self, _alert: &Alert) -> Result<(), crate::VenueError> {
            Err(crate::VenueError::Alert("dark sink: scripted".to_owned()))
        }
    }

    let good = MemorySink::new();
    let good_member: std::sync::Arc<dyn AlertSink> = std::sync::Arc::new(good.clone());
    let fanout = FanoutSink::new(vec![good_member, std::sync::Arc::new(Dark)]);
    let alert = Alert {
        source: AlertSource::ReconcileMismatch,
        class: AlertClass::Correctness,
        detail: "RECONCILE HALT: scripted".to_owned(),
        at: now(),
    };
    let err = fanout
        .send(&alert)
        .expect_err("a dark member fails the fan-out");
    assert!(err.to_string().contains("dark sink"), "got: {err}");
    assert_eq!(good.alerts().len(), 1, "the live member still delivered");
}
