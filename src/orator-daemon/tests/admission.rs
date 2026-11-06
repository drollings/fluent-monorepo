//! Admission harness: saturation refuses fast, idle capacity admits
//! everything, failing endpoints eject and recovered ones re-enter.
//!
//! The bound and the ejection threshold are scored through the shared
//! calibration harness against stub servers and scripted lanes — never
//! models, never wall-clock audio. Timing assertions are generous
//! bands, never exact.

use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

use common_core::calibration::sweep_thresholds;

use crate::admission::{AdmissionGate, EjectionTracker, EJECTION_THRESHOLD};
use crate::voice::{VoiceError, VoiceOwner};

/// One-lane scripted endpoint: turns serialize through a mutex with a
/// fixed scripted cost, the way the serial synth path does.
struct SerialLane {
    lane: Mutex<()>,
    work: Duration,
    hits: std::sync::atomic::AtomicU32,
}

impl SerialLane {
    fn turn(&self) {
        let _held = common_core::sync::lock(&self.lane);
        self.hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::thread::sleep(self.work);
    }

    fn hits(&self) -> u32 {
        self.hits.load(std::sync::atomic::Ordering::SeqCst)
    }
}

fn lane(work_ms: u64) -> SerialLane {
    SerialLane {
        lane: Mutex::new(()),
        work: Duration::from_millis(work_ms),
        hits: std::sync::atomic::AtomicU32::new(0),
    }
}

#[test]
fn idle_capacity_admits_everything() {
    // Control group that must not fire: with headroom, every request
    // is admitted — zero refusals, zero reap-while-leased.
    let gate = Arc::new(AdmissionGate::new(8));
    let lane = Arc::new(lane(100));
    let start = Barrier::new(5);
    let release = Barrier::new(5);
    let outcomes: Vec<std::sync::Mutex<Option<bool>>> =
        (0..4).map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|scope| {
        for (index, slot) in outcomes.iter().enumerate() {
            let (gate, lane, start, release) =
                (Arc::clone(&gate), Arc::clone(&lane), &start, &release);
            scope.spawn(move || {
                start.wait();
                let admitted = gate.admit().is_ok();
                *common_core::sync::lock(slot) = Some(admitted);
                release.wait();
                if admitted {
                    lane.turn();
                }
                let _ = index;
            });
        }
        start.wait();
        release.wait();
    });
    assert_eq!(lane.hits(), 4, "every admitted turn serves");
    for slot in &outcomes {
        assert_eq!(
            *common_core::sync::lock(slot),
            Some(true),
            "idle capacity never refuses"
        );
    }
}

#[test]
fn saturation_refuses_fast_with_a_named_reason() {
    let gate = Arc::new(AdmissionGate::new(2));
    let lane = Arc::new(lane(100));
    let start = Barrier::new(5);
    let release = Barrier::new(5);
    let admitted = std::sync::atomic::AtomicU32::new(0);
    let refused = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let (gate, lane, start, release) =
                (Arc::clone(&gate), Arc::clone(&lane), &start, &release);
            let admitted = &admitted;
            let refused = &refused;
            scope.spawn(move || {
                start.wait();
                let began = Instant::now();
                match gate.admit() {
                    Ok(_permit) => {
                        admitted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        release.wait();
                        lane.turn();
                    }
                    Err(refusal) => {
                        common_core::sync::lock(refused)
                            .push((refusal.reason.clone(), began.elapsed()));
                        release.wait();
                    }
                }
            });
        }
        start.wait();
        release.wait();
    });
    assert_eq!(admitted.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(lane.hits(), 2, "refused turns never reach the lane");
    let refused = common_core::sync::lock(&refused);
    assert_eq!(refused.len(), 2, "past-bound requests refuse");
    for (reason, elapsed) in refused.iter() {
        assert!(
            reason.contains("saturated"),
            "refusal names its cause: {reason}"
        );
        assert!(reason.contains('2'), "refusal names the bound: {reason}");
        assert!(
            *elapsed < Duration::from_secs(1),
            "refusal is fast, never queued: {elapsed:?}"
        );
    }
}

#[test]
fn serial_depth_predicts_breach_on_the_measured_quantum() {
    // Calibration: the single-turn quantum is measured on the lane
    // (one real turn), depths are observed through the real gate under
    // a start barrier, and breach is the serial model past the
    // budget. No wall-clock classification, no flake.
    let lane = lane(500);
    let began = Instant::now();
    lane.turn();
    let quantum = began.elapsed();
    assert!(
        (Duration::from_millis(400)..Duration::from_millis(2000)).contains(&quantum),
        "measured quantum anchors the model: {quantum:?}"
    );
    let quantum_ms = quantum.as_millis() as u64;
    let gate = Arc::new(AdmissionGate::new(usize::MAX));
    let start = Barrier::new(7);
    let release = Barrier::new(7);
    let depths = Arc::new(std::sync::Mutex::new(Vec::new()));
    std::thread::scope(|scope| {
        for _ in 0..6 {
            let (gate, start, release, depths) =
                (Arc::clone(&gate), &start, &release, Arc::clone(&depths));
            scope.spawn(move || {
                start.wait();
                let permit = gate.admit().expect("unbounded admits all");
                common_core::sync::lock(&depths).push(permit.depth());
                release.wait();
            });
        }
        start.wait();
        release.wait();
    });
    let mut depths = common_core::sync::lock(&depths).clone();
    depths.sort_unstable();
    assert_eq!(depths, vec![0, 1, 2, 3, 4, 5], "all depths observed");
    let budget_ms = 1200u64;
    struct Case {
        depth: u64,
        breached: bool,
    }
    let cases: Vec<Case> = depths
        .iter()
        .map(|depth| Case {
            depth: *depth as u64,
            breached: (*depth as u64 + 1) * quantum_ms > budget_ms,
        })
        .collect();
    assert_eq!(
        cases.iter().filter(|case| case.breached).count(),
        4,
        "depths 2..=5 breach past the budget"
    );
    let reports = sweep_thresholds(
        &cases,
        |case: &Case| case.depth as f64,
        |case: &Case| case.breached,
        &[1.0, 2.0, 3.0, 4.0],
    );
    assert_eq!(reports.len(), 4);
    // Threshold 1 over-fires on depth 1: precision below the gate.
    assert!(
        !reports[0].1.passes_gate(),
        "bound 1 false-refuses healthy depth: {:?}",
        reports[0].1
    );
    // Threshold 2 is exact on this fixture.
    assert_eq!(
        (
            reports[1].1.tp,
            reports[1].1.fp,
            reports[1].1.tn,
            reports[1].1.r#fn
        ),
        (4, 0, 2, 0)
    );
    assert!(
        reports[1].1.passes_gate(),
        "bound 2 must pass the shared gate: {:?}",
        reports[1].1
    );
    // Higher thresholds pass the gate but miss real breaches.
    assert!(reports[2].1.passes_gate() && reports[2].1.r#fn > 0);
    assert!(reports[3].1.passes_gate() && reports[3].1.r#fn > 0);
    // Decision rule: smallest passing bound with zero missed breaches.
    let winner = reports
        .iter()
        .find(|(_, report)| report.passes_gate() && report.r#fn == 0)
        .map(|(threshold, _)| *threshold);
    assert_eq!(winner, Some(2.0), "the shipped bound is 2");
}

struct EjectCase {
    consecutive_failures: u64,
    should_eject: bool,
}

#[test]
fn ejection_threshold_scores_two_over_one() {
    // Controls (one transient failure, then healthy) must never eject;
    // sustained failures must. Threshold 1 false-ejects the controls.
    let cases = vec![
        EjectCase {
            consecutive_failures: 1,
            should_eject: false,
        },
        EjectCase {
            consecutive_failures: 1,
            should_eject: false,
        },
        EjectCase {
            consecutive_failures: 1,
            should_eject: false,
        },
        EjectCase {
            consecutive_failures: 2,
            should_eject: true,
        },
        EjectCase {
            consecutive_failures: 2,
            should_eject: true,
        },
        EjectCase {
            consecutive_failures: 2,
            should_eject: true,
        },
    ];
    let reports = sweep_thresholds(
        &cases,
        |case: &EjectCase| case.consecutive_failures as f64,
        |case: &EjectCase| case.should_eject,
        &[1.0, 2.0],
    );
    assert!(
        !reports[0].1.passes_gate(),
        "eject-on-first-failure false-ejects transients: {:?}",
        reports[0].1
    );
    assert_eq!(
        (
            reports[1].1.tp,
            reports[1].1.fp,
            reports[1].1.tn,
            reports[1].1.r#fn
        ),
        (3, 0, 3, 0)
    );
    assert!(
        reports[1].1.passes_gate(),
        "eject-on-second-consecutive must pass: {:?}",
        reports[1].1
    );
    assert_eq!(EJECTION_THRESHOLD, 2, "the shipped threshold is 2");
}

#[test]
fn ejection_tracker_holds_and_releases() {
    let mut tracker = EjectionTracker::new(2);
    assert!(!tracker.is_ejected());
    tracker.record_backend();
    assert!(!tracker.is_ejected(), "one transient never ejects");
    tracker.record_success();
    tracker.record_backend();
    assert!(!tracker.is_ejected(), "success breaks the streak");
    tracker.record_backend();
    assert!(tracker.is_ejected(), "two consecutive eject");
    tracker.record_success();
    assert!(!tracker.is_ejected(), "success re-enters");
}

fn failing_audio_server() -> httpmock::MockServer {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("POST").path("/v1/chat/completions");
        then.status(500).body("sick");
    });
    server
}

fn voiced_audio_server() -> httpmock::MockServer {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("POST").path("/v1/chat/completions");
        then.status(200).body(format!(
            "data: {{\"choices\":[{{\"delta\":{{\"audio_chunk\":{{\"data\":\"{}\"}}}}}}]}}\n\
             data: [DONE]\n",
            one_f32()
        ));
    });
    server
}

fn one_f32() -> String {
    use base64::prelude::*;
    BASE64_STANDARD.encode(2.5f32.to_le_bytes())
}

#[test]
fn two_consecutive_backend_turns_eject_and_fail_fast() {
    let server = failing_audio_server();
    let owner = VoiceOwner::with_binary(
        std::path::PathBuf::from("/nonexistent-orator/audio-server"),
        Duration::from_secs(60),
    );
    for _ in 0..2 {
        let error = owner
            .say_via(&server.base_url(), "hello", None)
            .expect_err("500s fail the turn");
        assert!(
            matches!(error, VoiceError::Backend(_)) && error.to_string().contains("sick"),
            "turn failure stays Backend: {error:?}"
        );
    }
    let began = Instant::now();
    let error = common_core::runtime::block_on(owner.ensure()).expect_err("ejected fails fast");
    assert!(
        matches!(error, VoiceError::Ejected(_)),
        "ejection fails named, never boots blind: {error:?}"
    );
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "ejection fast-fails without booting: {:?}",
        began.elapsed()
    );
    owner.shutdown();
}

#[test]
fn transient_failure_then_success_never_ejects() {
    let failing = failing_audio_server();
    let voiced = voiced_audio_server();
    let owner = VoiceOwner::with_binary(
        std::path::PathBuf::from("/nonexistent-orator/audio-server"),
        Duration::from_secs(60),
    );
    owner
        .say_via(&failing.base_url(), "hello", None)
        .expect_err("first turn fails");
    owner
        .say_via(&voiced.base_url(), "hello", None)
        .expect("success breaks the streak");
    owner
        .say_via(&failing.base_url(), "hello", None)
        .expect_err("isolated failure is not ejection");
    let error = common_core::runtime::block_on(owner.ensure()).expect_err("no server boots here");
    assert!(
        matches!(error, VoiceError::Boot(_)),
        "a broken streak proceeds to boot, never Ejected: {error:?}"
    );
    owner.shutdown();
}

#[test]
fn recovered_endpoint_re_enters_through_the_probe() {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("POST").path("/v1/chat/completions");
        then.status(500).body("sick");
    });
    server.mock(|when, then| {
        when.method("OPTIONS").path("/v1/chat/completions");
        then.status(200).body("{}");
    });
    let owner = VoiceOwner::with_binary(
        std::path::PathBuf::from("/nonexistent-orator/audio-server"),
        Duration::from_secs(60),
    );
    for _ in 0..2 {
        let _ = owner.say_via(&server.base_url(), "hello", None);
    }
    // The probe answers healthy while turns still fail: the next
    // ensure clears the ejection and reaches boot — Boot, not Ejected,
    // proves the endpoint re-entered.
    let error = common_core::runtime::block_on(owner.ensure()).expect_err("no binary boots here");
    assert!(
        matches!(error, VoiceError::Boot(_)),
        "healthy probe re-enters toward boot: {error:?}"
    );
    owner.shutdown();
}

#[test]
fn text_only_voiceless_turns_never_eject() {
    // False-eject control: a responsive-but-voiceless server is alive
    // (its probe answers) — NoAudio must not count toward ejection.
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.method("POST").path("/v1/chat/completions");
        then.status(200).body(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\
             data: [DONE]\n",
        );
    });
    let owner = VoiceOwner::with_binary(
        std::path::PathBuf::from("/nonexistent-orator/audio-server"),
        Duration::from_secs(60),
    );
    for _ in 0..3 {
        let error = owner
            .say_via(&server.base_url(), "hello", None)
            .expect_err("text-only voices nothing");
        assert!(
            error.to_string().contains("no audio"),
            "voiceless stays voiceless: {error:?}"
        );
    }
    let error = common_core::runtime::block_on(owner.ensure()).expect_err("no server boots here");
    assert!(
        matches!(error, VoiceError::Boot(_)),
        "voiceless turns never eject: {error:?}"
    );
    owner.shutdown();
}

/// M8-S1 characterization: the refusal line verbatim (cause +
/// inflight count + bound + remedy) and guard-drop release — dropping
/// the guard frees the permit, so admission is strictly scoped.
#[test]
fn refusal_line_is_verbatim_and_guards_release_on_drop() {
    let gate = AdmissionGate::new(1);
    assert_eq!(gate.max_inflight(), 1);
    assert_eq!(gate.inflight(), 0);
    let guard = gate.admit().expect("idle capacity admits");
    assert_eq!(guard.depth(), 0, "nothing else in flight");
    assert_eq!(gate.inflight(), 1);
    let refusal = gate.admit().expect_err("past-bound refuses");
    assert_eq!(
        refusal.reason,
        "admission saturated: 1 turns in flight (bound 1) — retry when a turn completes"
    );
    drop(guard);
    assert_eq!(gate.inflight(), 0, "drop releases the permit");
    let guard = gate.admit().expect("released capacity re-admits");
    assert_eq!(gate.inflight(), 1);
    drop(guard);
}

#[test]
fn sequential_turns_pass_the_default_bound() {
    let server = voiced_audio_server();
    let owner = VoiceOwner::disabled();
    for _ in 0..3 {
        owner
            .say_via(&server.base_url(), "hello", None)
            .expect("sequential turns never contend");
    }
    owner.shutdown();
}

#[test]
fn saturated_admission_refuses_at_the_owner_boundary() {
    // Pinned refusal shape: past the bound the turn fails with the
    // named reason instead of queueing — never `Ok`, never silent.
    // The bound is read off the gate, not restated, so a future
    // recalibration updates the holds without rewriting the shape.
    let server = voiced_audio_server();
    let owner = VoiceOwner::disabled();
    let bound = owner.gate.max_inflight();
    assert!(bound >= 2, "the shipped bound admits a pair");
    let _held: Vec<_> = (0..bound)
        .map(|_| owner.gate.admit().expect("hold"))
        .collect();
    let error = owner
        .say_via(&server.base_url(), "hello", None)
        .expect_err("past-bound turns refuse");
    assert!(
        matches!(error, VoiceError::Saturated(_)),
        "refusal carries its code: {error:?}"
    );
    let text = error.to_string();
    assert!(
        text.contains("saturated") && text.contains(&bound.to_string()),
        "refusal names cause and bound: {text}"
    );
    owner.shutdown();
}
