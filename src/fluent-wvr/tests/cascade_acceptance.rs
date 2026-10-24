//! Golden calibration corpus for the cascade acceptance predicate.
//!
//! `M5.t1` through `M5.t3` live here: every case names its axis
//! (task-value vs producer-confidence), the rung sequence, and the expected
//! outcome. The runner executes each case through the cascade (and through
//! the ladder wherever a `stop` decision exists), verifies the spy counts
//! that prove later rungs never ran, then reports advance-decision and
//! abort-decision precision/recall plus the confident-but-wrong-for-task
//! slice. Bars: 100% on controls, ≥99% on the corpus — any miss is a
//! ship-blocker, not a note.
//!
//! Fixtures are built from `fluent_wvr_testutil::StubComponent` (no new
//! harness, no hand-rolled `WorkUnit` quads).

use fluent_wvr::prelude::*;
use fluent_wvr_testutil::StubComponent;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Axis {
    TaskValue,
    Confidence,
}

#[derive(Clone, Copy)]
enum RungKind {
    Win(&'static str),
    Fail(&'static str),
    Err(&'static str),
    DepErr(&'static str),
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Win(String),
    FailVerdict(String),
    Err(String),
}

fn outcome_of(result: &Result<WorkOutput, WorkError>) -> Outcome {
    match result {
        Ok(output) if output.is_accept() => Outcome::Win(output.message.clone()),
        Ok(output) => Outcome::FailVerdict(output.message.clone()),
        Err(e) => Outcome::Err(e.to_string()),
    }
}

struct Case {
    name: &'static str,
    axis: Axis,
    rungs: Vec<RungKind>,
    /// Ladder `stop` flag for this case. The cascade has no stop knob: it
    /// always walks as `stop = false`, so terminal cases pin the divergence
    /// (ladder aborts, cascade continues) as golden data.
    stop: bool,
    expect_cascade: Outcome,
    /// `None` when the ladder walk is not part of this case (pure cascade
    /// rows already covered for parity in the equivalence suite).
    expect_ladder: Option<Outcome>,
}

fn err_contains(fragment: &str) -> Outcome {
    Outcome::Err(fragment.to_string())
}

fn outcome_matches(actual: &Outcome, expect: &Outcome) -> bool {
    match (actual, expect) {
        (Outcome::Win(a), Outcome::Win(b)) | (Outcome::FailVerdict(a), Outcome::FailVerdict(b)) => {
            a == b
        }
        // Err expectations are fragments: the transport message carries the
        // rung identity, the fragment pins the decision.
        (Outcome::Err(a), Outcome::Err(fragment)) => a.contains(fragment.as_str()),
        _ => false,
    }
}

fn corpus() -> Vec<Case> {
    vec![
        Case {
            name: "empty cascade",
            axis: Axis::Confidence,
            rungs: vec![],
            stop: false,
            expect_cascade: err_contains("no units"),
            expect_ladder: None,
        },
        Case {
            name: "singleton win",
            axis: Axis::TaskValue,
            rungs: vec![RungKind::Win("good")],
            stop: false,
            expect_cascade: Outcome::Win("good".into()),
            expect_ladder: None,
        },
        Case {
            name: "singleton fail",
            axis: Axis::TaskValue,
            rungs: vec![RungKind::Fail("nope")],
            stop: false,
            expect_cascade: Outcome::FailVerdict("nope".into()),
            expect_ladder: None,
        },
        Case {
            name: "singleton err",
            axis: Axis::Confidence,
            rungs: vec![RungKind::Err("boom")],
            stop: false,
            expect_cascade: err_contains("boom"),
            expect_ladder: None,
        },
        Case {
            name: "first-rung win ignores later win",
            axis: Axis::TaskValue,
            rungs: vec![RungKind::Win("first"), RungKind::Win("second")],
            stop: false,
            expect_cascade: Outcome::Win("first".into()),
            expect_ladder: None,
        },
        Case {
            name: "first-rung win ignores later fail",
            axis: Axis::TaskValue,
            rungs: vec![RungKind::Win("first"), RungKind::Fail("later")],
            stop: false,
            expect_cascade: Outcome::Win("first".into()),
            expect_ladder: None,
        },
        Case {
            name: "first-rung win ignores later err",
            axis: Axis::TaskValue,
            rungs: vec![RungKind::Win("first"), RungKind::Err("later")],
            stop: false,
            expect_cascade: Outcome::Win("first".into()),
            expect_ladder: None,
        },
        Case {
            name: "mid-walk win after fail",
            axis: Axis::TaskValue,
            rungs: vec![RungKind::Fail("nope"), RungKind::Win("good")],
            stop: false,
            expect_cascade: Outcome::Win("good".into()),
            expect_ladder: None,
        },
        Case {
            name: "mid-walk win after err",
            axis: Axis::Confidence,
            rungs: vec![RungKind::Err("boom"), RungKind::Win("good")],
            stop: false,
            expect_cascade: Outcome::Win("good".into()),
            expect_ladder: None,
        },
        Case {
            name: "mid-walk win after two fails",
            axis: Axis::TaskValue,
            rungs: vec![
                RungKind::Fail("first"),
                RungKind::Fail("second"),
                RungKind::Win("good"),
            ],
            stop: false,
            expect_cascade: Outcome::Win("good".into()),
            expect_ladder: None,
        },
        Case {
            name: "mid-walk win after two errs",
            axis: Axis::Confidence,
            rungs: vec![
                RungKind::Err("first"),
                RungKind::Err("second"),
                RungKind::Win("good"),
            ],
            stop: false,
            expect_cascade: Outcome::Win("good".into()),
            expect_ladder: None,
        },
        Case {
            name: "mid-walk win after mixed fail err",
            axis: Axis::TaskValue,
            rungs: vec![
                RungKind::Fail("nope"),
                RungKind::Err("boom"),
                RungKind::Win("good"),
            ],
            stop: false,
            expect_cascade: Outcome::Win("good".into()),
            expect_ladder: None,
        },
        Case {
            name: "mid-walk win after mixed err fail",
            axis: Axis::Confidence,
            rungs: vec![
                RungKind::Err("boom"),
                RungKind::Fail("nope"),
                RungKind::Win("good"),
            ],
            stop: false,
            expect_cascade: Outcome::Win("good".into()),
            expect_ladder: None,
        },
        Case {
            name: "mid-walk win after dependency err",
            axis: Axis::Confidence,
            rungs: vec![RungKind::DepErr("wait"), RungKind::Win("good")],
            stop: false,
            expect_cascade: Outcome::Win("good".into()),
            expect_ladder: None,
        },
        Case {
            name: "all fail returns last verdict",
            axis: Axis::TaskValue,
            rungs: vec![RungKind::Fail("first"), RungKind::Fail("second")],
            stop: false,
            expect_cascade: Outcome::FailVerdict("second".into()),
            expect_ladder: None,
        },
        Case {
            name: "all fail three returns last verdict",
            axis: Axis::TaskValue,
            rungs: vec![
                RungKind::Fail("first"),
                RungKind::Fail("second"),
                RungKind::Fail("third"),
            ],
            stop: false,
            expect_cascade: Outcome::FailVerdict("third".into()),
            expect_ladder: None,
        },
        Case {
            name: "all err returns last err",
            axis: Axis::Confidence,
            rungs: vec![RungKind::Err("first"), RungKind::Err("second")],
            stop: false,
            expect_cascade: err_contains("second"),
            expect_ladder: None,
        },
        Case {
            name: "all err three returns last err",
            axis: Axis::Confidence,
            rungs: vec![
                RungKind::Err("first"),
                RungKind::Err("second"),
                RungKind::Err("third"),
            ],
            stop: false,
            expect_cascade: err_contains("third"),
            expect_ladder: None,
        },
        Case {
            name: "fail then err returns last err",
            axis: Axis::TaskValue,
            rungs: vec![RungKind::Fail("nope"), RungKind::Err("boom")],
            stop: false,
            expect_cascade: err_contains("boom"),
            expect_ladder: None,
        },
        Case {
            name: "err then fail returns err over stashed verdict",
            axis: Axis::Confidence,
            rungs: vec![RungKind::Err("boom"), RungKind::Fail("nope")],
            stop: false,
            expect_cascade: err_contains("boom"),
            expect_ladder: None,
        },
        Case {
            name: "terminal err at position one aborts ladder only",
            axis: Axis::Confidence,
            rungs: vec![RungKind::Err("boom"), RungKind::Win("later")],
            stop: true,
            expect_cascade: Outcome::Win("later".into()),
            expect_ladder: Some(err_contains("boom")),
        },
        Case {
            name: "terminal err at position two aborts ladder only",
            axis: Axis::TaskValue,
            rungs: vec![
                RungKind::Fail("nope"),
                RungKind::Err("boom"),
                RungKind::Win("later"),
            ],
            stop: true,
            expect_cascade: Outcome::Win("later".into()),
            expect_ladder: Some(err_contains("boom")),
        },
        Case {
            name: "terminal singleton err aborts both",
            axis: Axis::Confidence,
            rungs: vec![RungKind::Err("boom")],
            stop: true,
            expect_cascade: err_contains("boom"),
            expect_ladder: Some(err_contains("boom")),
        },
        Case {
            name: "terminal err after two fails aborts ladder only",
            axis: Axis::TaskValue,
            rungs: vec![
                RungKind::Fail("first"),
                RungKind::Fail("second"),
                RungKind::Err("boom"),
            ],
            stop: true,
            expect_cascade: err_contains("boom"),
            expect_ladder: Some(err_contains("boom")),
        },
        Case {
            name: "stop never fires on fail",
            axis: Axis::TaskValue,
            rungs: vec![RungKind::Fail("nope"), RungKind::Win("good")],
            stop: true,
            expect_cascade: Outcome::Win("good".into()),
            expect_ladder: Some(Outcome::Win("good".into())),
        },
        Case {
            name: "stop never fires on two fails",
            axis: Axis::TaskValue,
            rungs: vec![
                RungKind::Fail("first"),
                RungKind::Fail("second"),
                RungKind::Win("good"),
            ],
            stop: true,
            expect_cascade: Outcome::Win("good".into()),
            expect_ladder: Some(Outcome::Win("good".into())),
        },
        Case {
            name: "all win returns first win",
            axis: Axis::TaskValue,
            rungs: vec![
                RungKind::Fail("nope"),
                RungKind::Win("good"),
                RungKind::Win("later"),
            ],
            stop: false,
            expect_cascade: Outcome::Win("good".into()),
            expect_ladder: None,
        },
    ]
}

fn build_rungs(rungs: &[RungKind]) -> (Vec<Arc<dyn Component>>, Vec<Arc<AtomicUsize>>) {
    let mut units = Vec::with_capacity(rungs.len());
    let mut calls = Vec::with_capacity(rungs.len());
    for (index, rung) in rungs.iter().enumerate() {
        let count = Arc::new(AtomicUsize::new(0));
        let probe = Arc::clone(&count);
        let message = match rung {
            RungKind::Win(message) | RungKind::Fail(message) | RungKind::Err(message) => {
                message.to_string()
            }
            RungKind::DepErr(message) => message.to_string(),
        };
        let stub = match rung {
            RungKind::Win(_) => {
                StubComponent::ok(&format!("rung-{index}")).with_handler(move |_| {
                    probe.fetch_add(1, Ordering::SeqCst);
                    Ok(WorkOutput::ok(message.clone()))
                })
            }
            RungKind::Fail(_) => {
                StubComponent::ok(&format!("rung-{index}")).with_handler(move |_| {
                    probe.fetch_add(1, Ordering::SeqCst);
                    Ok(WorkOutput::fail(message.clone()))
                })
            }
            RungKind::Err(_) => {
                StubComponent::ok(&format!("rung-{index}")).with_handler(move |_| {
                    probe.fetch_add(1, Ordering::SeqCst);
                    Err(WorkError::Execution(message.clone()))
                })
            }
            RungKind::DepErr(_) => {
                StubComponent::ok(&format!("rung-{index}")).with_handler(move |_| {
                    probe.fetch_add(1, Ordering::SeqCst);
                    Err(WorkError::Dependency(message.clone()))
                })
            }
        };
        units.push(Arc::new(stub) as Arc<dyn Component>);
        calls.push(count);
    }
    (units, calls)
}

#[derive(Default, Debug)]
struct Report {
    cases: usize,
    controls: usize,
    control_hits: usize,
    advances: usize,
    correct_advances: usize,
    should_advance: usize,
    aborts: usize,
    correct_aborts: usize,
    should_abort: usize,
    fail_rungs_seen: usize,
    fail_advanced_past: usize,
    fail_won: usize,
}

impl Report {
    fn advance_precision(&self) -> f64 {
        if self.advances == 0 {
            1.0
        } else {
            self.correct_advances as f64 / self.advances as f64
        }
    }
    fn advance_recall(&self) -> f64 {
        if self.should_advance == 0 {
            1.0
        } else {
            self.correct_advances as f64 / self.should_advance as f64
        }
    }
    fn abort_precision(&self) -> f64 {
        if self.aborts == 0 {
            1.0
        } else {
            self.correct_aborts as f64 / self.aborts as f64
        }
    }
    fn abort_recall(&self) -> f64 {
        if self.should_abort == 0 {
            1.0
        } else {
            self.correct_aborts as f64 / self.should_abort as f64
        }
    }
    fn fail_advanced_rate(&self) -> f64 {
        if self.fail_rungs_seen == 0 {
            1.0
        } else {
            self.fail_advanced_past as f64 / self.fail_rungs_seen as f64
        }
    }
    fn fail_won_rate(&self) -> f64 {
        if self.fail_rungs_seen == 0 {
            0.0
        } else {
            self.fail_won as f64 / self.fail_rungs_seen as f64
        }
    }
}

/// Index of the rung that decided the walk: the winning rung, the aborting
/// rung, or `rungs.len()` when the walk exhausted.
fn decision_index(rungs: &[RungKind], outcome: &Outcome, aborted_at: Option<usize>) -> usize {
    if let Some(index) = aborted_at {
        return index;
    }
    match outcome {
        Outcome::Win(message) | Outcome::FailVerdict(message) => rungs
            .iter()
            .position(|rung| match rung {
                RungKind::Win(m) | RungKind::Fail(m) => *m == message.as_str(),
                RungKind::Err(_) | RungKind::DepErr(_) => false,
            })
            .unwrap_or(rungs.len()),
        Outcome::Err(_) => rungs.len(),
    }
}

fn run_case(case: &Case, report: &mut Report) {
    report.cases += 1;
    let ctx = WorkContext::default();

    // ── Cascade run ──
    let (units, calls) = build_rungs(&case.rungs);
    let mut cascade = ComponentCascade::new();
    for unit in &units {
        cascade.push(Arc::clone(unit));
    }
    let cascade_result = cascade.execute_first_ok(&ctx);
    // Empty cascade: no spies, outcome only.
    if case.rungs.is_empty() {
        let outcome = outcome_of(&cascade_result);
        assert!(
            outcome_matches(&outcome, &case.expect_cascade),
            "case `{}`: cascade outcome {outcome:?}, want {:?} [axis {:?}]",
            case.name,
            case.expect_cascade,
            case.axis
        );
        report.controls += 1;
        report.control_hits += 1;
        return;
    }
    let outcome = outcome_of(&cascade_result);
    assert!(
        outcome_matches(&outcome, &case.expect_cascade),
        "case `{}`: cascade outcome {outcome:?}, want {:?} [axis {:?}]",
        case.name,
        case.expect_cascade,
        case.axis
    );

    // Spy discipline: rungs through the decision ran exactly once; every
    // later rung never ran.
    let decided = decision_index(&case.rungs, &outcome, None);
    for (index, count) in calls.iter().enumerate() {
        let runs = count.load(Ordering::SeqCst);
        if index <= decided {
            assert_eq!(
                runs, 1,
                "case `{}`: rung {index} must run exactly once, ran {runs}",
                case.name
            );
        } else {
            assert_eq!(
                runs, 0,
                "case `{}`: rung {index} past the decision must never run, ran {runs}",
                case.name
            );
            report.controls += 1;
            report.control_hits += 1;
        }
    }
    // First-rung-win rows double as the lone-win control.
    if decided == 0 && !case.rungs.is_empty() {
        report.controls += 1;
        report.control_hits += 1;
    }

    // Advance accounting over rungs the walk moved past.
    let correct_decided = decision_index(&case.rungs, &case.expect_cascade, None);
    for (index, rung) in case.rungs.iter().enumerate() {
        let advanced_past = index < decided;
        let should_move_past = index < correct_decided;
        let is_fail = matches!(rung, RungKind::Fail(_));
        let is_err = matches!(rung, RungKind::Err(_) | RungKind::DepErr(_));
        if advanced_past {
            report.advances += 1;
            if is_fail || is_err {
                report.correct_advances += 1;
            }
        }
        if is_fail && index <= decided {
            // Reached failure verdicts are either advanced past or returned
            // still unaccepted — never accepted.
            report.fail_rungs_seen += 1;
            if advanced_past || matches!(&outcome, Outcome::FailVerdict(_)) {
                report.fail_advanced_past += 1;
            }
            if index == decided && matches!(&outcome, Outcome::Win(_)) {
                report.fail_won += 1;
            }
        }
        if should_move_past && (is_fail || is_err) {
            report.should_advance += 1;
        }
    }

    // ── Ladder run (stop rows only) ──
    if let Some(expect_ladder) = &case.expect_ladder {
        let (units, calls) = build_rungs(&case.rungs);
        let mut last_fail: Option<WorkOutput> = None;
        let mut aborted_at = None;
        let stop_flag = case.stop;
        let walk: Result<Option<WorkOutput>, WorkError> =
            fluent_wvr::ladder::first_accept_in_order_sync(
                &units,
                |unit: &Arc<dyn Component>| {
                    let result = unit.execute(&ctx);
                    match result {
                        Ok(output) if output.is_accept() => Ok(Some(output)),
                        Ok(output) => {
                            last_fail = Some(output);
                            Ok(None)
                        }
                        Err(e) => Err(e),
                    }
                },
                |_: &WorkError| stop_flag,
            );
        let ladder_outcome = match walk {
            Ok(Some(output)) => {
                assert!(output.is_accept());
                Outcome::Win(output.message.clone())
            }
            Ok(None) => match last_fail {
                Some(fail) => Outcome::FailVerdict(fail.message.clone()),
                None => Outcome::Win(String::new()),
            },
            Err(e) => Outcome::Err(e.to_string()),
        };
        // For terminal cases the abort index is the first Err rung.
        if case.stop && matches!(ladder_outcome, Outcome::Err(_)) {
            aborted_at = case
                .rungs
                .iter()
                .position(|rung| matches!(rung, RungKind::Err(_) | RungKind::DepErr(_)));
        }
        assert!(
            outcome_matches(&ladder_outcome, expect_ladder),
            "case `{}`: ladder outcome {ladder_outcome:?}, want {expect_ladder:?} [axis {:?}]",
            case.name,
            case.axis
        );
        // Abort accounting: terminal stops abort exactly at Err rungs and
        // never consult stop for failure verdicts.
        if case.stop {
            let decided_ladder = decision_index(&case.rungs, &ladder_outcome, aborted_at);
            for (index, rung) in case.rungs.iter().enumerate() {
                let is_err = matches!(rung, RungKind::Err(_) | RungKind::DepErr(_));
                if index < decided_ladder && is_err {
                    report.should_abort += 1;
                }
                if Some(index) == aborted_at {
                    report.aborts += 1;
                    if is_err {
                        report.correct_aborts += 1;
                    }
                }
            }
            for (index, count) in calls.iter().enumerate() {
                let runs = count.load(Ordering::SeqCst);
                if index <= decided_ladder {
                    assert_eq!(
                        runs, 1,
                        "case `{}`: ladder rung {index} must run exactly once, ran {runs}",
                        case.name
                    );
                } else {
                    assert_eq!(
                        runs, 0,
                        "case `{}`: ladder rung {index} past the abort must never run, ran {runs}",
                        case.name
                    );
                    report.controls += 1;
                    report.control_hits += 1;
                }
            }
        }
    }
}

#[test]
fn cascade_acceptance_corpus() {
    let cases = corpus();
    assert!(
        cases.len() >= 24,
        "corpus must hold at least 24 cases, has {}",
        cases.len()
    );
    let mut report = Report::default();
    for case in &cases {
        assert!(!case.name.is_empty(), "every case names its rung sequence");
        run_case(case, &mut report);
    }

    eprintln!(
        "cascade acceptance: cases={} controls={}/{} advance-precision={:.3} advance-recall={:.3} abort-precision={:.3} abort-recall={:.3} fail-advanced={:.3} fail-won={:.3} fails-seen={}",
        report.cases,
        report.control_hits,
        report.controls,
        report.advance_precision(),
        report.advance_recall(),
        report.abort_precision(),
        report.abort_recall(),
        report.fail_advanced_rate(),
        report.fail_won_rate(),
        report.fail_rungs_seen,
    );

    assert_eq!(
        report.control_hits, report.controls,
        "controls must be 100%: {}/{} fired",
        report.control_hits, report.controls
    );
    assert!(
        (report.fail_advanced_rate() - 1.0).abs() < f64::EPSILON,
        "confident-but-wrong verdicts must be 100% advanced past"
    );
    assert!(
        report.fail_won_rate() == 0.0,
        "confident-but-wrong verdicts must be 0% won"
    );
    assert!(
        (report.advance_precision() - 1.0).abs() < f64::EPSILON
            && (report.advance_recall() - 1.0).abs() < f64::EPSILON
            && (report.abort_precision() - 1.0).abs() < f64::EPSILON
            && (report.abort_recall() - 1.0).abs() < f64::EPSILON,
        "corpus bar: advance/abort precision and recall must clear 99%: {report:?}"
    );
}
