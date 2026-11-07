//! Barge-in calibration: scripted evidence corpus with control groups.
//!
//! Controls that must NOT cut: backchannels (short + lexicon, per
//! language), TV speech with a verification mismatch, empty transcripts,
//! below-threshold sustains. Cases that MUST cut: sustained speech with
//! STT words. Metrics: false-cut rate 0, must-cut recall 1. Cut latency
//! itself is a bench-gate quantity — here the decision is what ships.

use crate::driver::{BargeInEvidence, BargeInOutcome, TurnDriver};
use orator_core::Epoch;

struct Case {
    name: &'static str,
    lang: &'static str,
    evidence: BargeInEvidence,
    must_cut: bool,
}

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|word| (*word).to_string()).collect()
}

fn corpus() -> Vec<Case> {
    vec![
        // Control: English backchannels.
        Case {
            name: "en backchannel yeah",
            lang: "en",
            evidence: BargeInEvidence {
                sustained_ms: 200,
                audio_ms: 320,
                words: words(&["yeah"]),
                speaker_match: None,
            },
            must_cut: false,
        },
        Case {
            name: "en backchannel mm-hm right",
            lang: "en",
            evidence: BargeInEvidence {
                sustained_ms: 250,
                audio_ms: 540,
                words: words(&["mm-hm", "right"]),
                speaker_match: None,
            },
            must_cut: false,
        },
        // Control: German backchannels.
        Case {
            name: "de backchannel ja",
            lang: "de",
            evidence: BargeInEvidence {
                sustained_ms: 200,
                audio_ms: 280,
                words: words(&["ja"]),
                speaker_match: None,
            },
            must_cut: false,
        },
        Case {
            name: "de backchannel genau",
            lang: "de",
            evidence: BargeInEvidence {
                sustained_ms: 220,
                audio_ms: 410,
                words: words(&["genau"]),
                speaker_match: None,
            },
            must_cut: false,
        },
        // Control: TV without verification match.
        Case {
            name: "tv mismatch long speech",
            lang: "en",
            evidence: BargeInEvidence {
                sustained_ms: 900,
                audio_ms: 4200,
                words: words(&["breaking", "news", "tonight", "at", "eleven"]),
                speaker_match: Some(false),
            },
            must_cut: false,
        },
        // Control: empty transcript.
        Case {
            name: "empty transcript cough",
            lang: "en",
            evidence: BargeInEvidence {
                sustained_ms: 300,
                audio_ms: 700,
                words: Vec::new(),
                speaker_match: None,
            },
            must_cut: false,
        },
        // Control: below-threshold sustain with words.
        Case {
            name: "short overlap blip",
            lang: "en",
            evidence: BargeInEvidence {
                sustained_ms: 100,
                audio_ms: 140,
                words: words(&["what"]),
                speaker_match: None,
            },
            must_cut: false,
        },
        // Control: lexicon words but long audio are NOT backchannels…
        // …yet without other evidence of intent they still must cut only
        // with real words: "yeah yeah yeah yeah" over 2 s is insistence.
        // (Corpus pins the current rule: length alone never protects.)
        Case {
            name: "long lexicon insistence",
            lang: "en",
            evidence: BargeInEvidence {
                sustained_ms: 400,
                audio_ms: 2000,
                words: words(&["yeah", "yeah", "yeah"]),
                speaker_match: None,
            },
            must_cut: true,
        },
        // Must cut: sustained speech with words, verified or unenrolled.
        Case {
            name: "en interruption",
            lang: "en",
            evidence: BargeInEvidence {
                sustained_ms: 200,
                audio_ms: 1100,
                words: words(&["stop", "what", "is", "that"]),
                speaker_match: None,
            },
            must_cut: true,
        },
        Case {
            name: "de interruption verified",
            lang: "de",
            evidence: BargeInEvidence {
                sustained_ms: 250,
                audio_ms: 1300,
                words: words(&["stopp", "was", "ist", "das"]),
                speaker_match: Some(true),
            },
            must_cut: true,
        },
        Case {
            name: "overlap while thinking",
            lang: "en",
            evidence: BargeInEvidence {
                sustained_ms: 400,
                audio_ms: 1500,
                words: words(&["actually", "never", "mind"]),
                speaker_match: None,
            },
            must_cut: true,
        },
    ]
}

#[test]
fn barge_in_hits_zero_false_cuts_and_full_recall() {
    let mut false_cuts = 0usize;
    let mut controls = 0usize;
    let mut true_cuts = 0usize;
    let mut musts = 0usize;
    for case in corpus() {
        // Fresh driver per case: calibration measures the rule, not order.
        let mut driver = TurnDriver::with_lang(Epoch(1), case.lang);
        assert_eq!(
            driver.detect(case.evidence.sustained_ms),
            if case.evidence.sustained_ms >= 150 {
                BargeInOutcome::Ducking
            } else {
                BargeInOutcome::Ignored
            },
            "case `{}` detects wrong",
            case.name
        );
        let cut = driver.confirm(&case.evidence) == BargeInOutcome::Cut;
        assert_eq!(cut, case.must_cut, "case `{}` decides wrong", case.name);
        if case.must_cut {
            musts += 1;
            true_cuts += usize::from(cut);
        } else {
            controls += 1;
            false_cuts += usize::from(cut);
        }
    }
    assert_eq!(false_cuts, 0, "controls must never cut");
    assert_eq!(true_cuts, musts, "interruptions must always cut");
    assert!(controls >= 7, "control group must stay covered");
}

#[test]
fn recall_splits_evenly_across_languages() {
    // Must-cut recall per language: the lexicon must not protect one
    // language's interruptions while cutting the other's. Both `en`
    // and `de` must-cuts recall at 1.0 on this corpus.
    for lang in ["en", "de"] {
        let mut musts = 0usize;
        let mut true_cuts = 0usize;
        for case in corpus() {
            if case.lang != lang || !case.must_cut {
                continue;
            }
            let mut driver = TurnDriver::with_lang(Epoch(1), case.lang);
            let cut = driver.confirm(&case.evidence) == BargeInOutcome::Cut;
            musts += 1;
            true_cuts += usize::from(cut);
        }
        assert!(musts > 0, "language {lang} needs must-cut cases");
        assert_eq!(
            true_cuts, musts,
            "language {lang} must recall every interruption"
        );
    }
}

/// Sustained-speech detect grid: the temporal bound comes from the
/// shipped `TurnConfig` leaf (never a hardcoded duplicate), and the
/// detect arm trips exactly at/above it, never below — zero included
/// (a cold PTT mic accumulates no evidence, so detect stays shut).
/// Deterministic: durations, no timers, no audio.
#[test]
fn detect_trips_exactly_at_the_configured_bound() {
    use crate::driver::BargeInOutcome;
    let bound = orator_core::TurnConfig::default().barge_in_ms;
    assert_eq!(bound, 150, "shipped bound the grid is built around");
    let driver = TurnDriver::with_config(Epoch(1), "en", &orator_core::TurnConfig::default());
    for below in [0, 1, 50, bound - 1] {
        assert_eq!(
            driver.detect(below),
            BargeInOutcome::Ignored,
            "{below} ms must not trip a {bound} ms bound"
        );
    }
    for at_or_above in [bound, bound + 1, 500, u16::MAX] {
        assert_eq!(
            driver.detect(at_or_above),
            BargeInOutcome::Ducking,
            "{at_or_above} ms must trip a {bound} ms bound"
        );
    }
    // A raised leaf moves the arm with it: the driver reads config,
    // never a copy of the number.
    let raised = TurnDriver::with_config(
        Epoch(2),
        "en",
        &orator_core::TurnConfig {
            barge_in_ms: 300,
            ..orator_core::TurnConfig::default()
        },
    );
    assert_eq!(raised.detect(200), BargeInOutcome::Ignored);
    assert_eq!(raised.detect(300), BargeInOutcome::Ducking);
    // Vacuity guard: the grid exercises both arms, so a rule that
    // always ignores (or always ducks) fails here, not silently.
    let grid: Vec<u16> = [0, 1, 50, bound - 1, bound, bound + 1, 500]
        .into_iter()
        .collect();
    assert!(
        grid.iter()
            .any(|ms| driver.detect(*ms) == BargeInOutcome::Ignored),
        "grid must cover the ignore arm"
    );
    assert!(
        grid.iter()
            .any(|ms| driver.detect(*ms) == BargeInOutcome::Ducking),
        "grid must cover the duck arm"
    );
}

/// Reference continuity rule (M3b — the contract M5 promotes to
/// production): per-20 ms frames vote hot or cold; only an
/// *unbroken* hot run counts, and the run must reach the bound. Any
/// cold frame resets the run (pause-split speech never counts as
/// continuous); PTT-cold frames reset too (a latched-off mic is not
/// the user). The rule carries no timers and no I/O — a pure
/// function of the frame votes.
struct ContinuityWindow {
    run_ms: u64,
}

impl ContinuityWindow {
    fn new() -> Self {
        Self { run_ms: 0 }
    }

    /// Feed one frame vote; returns whether the bound is reached.
    /// `bound_ms` is the `turn.barge_in_ms` leaf, read by the caller.
    fn push(&mut self, hot: bool, ptt_cold: bool, frame_ms: u64, bound_ms: u16) -> bool {
        if !hot || ptt_cold {
            self.run_ms = 0;
            return false;
        }
        self.run_ms += frame_ms;
        self.run_ms >= u64::from(bound_ms)
    }
}

/// Sliding-window grid: below/at/above the bound, pause-split runs
/// that must not count, PTT edges, and the weakened bound-0 rule
/// that must fail the grid (anti-weakening). Frame quantum is the
/// 20 ms analysis frame.
#[test]
fn continuity_window_counts_only_unbroken_hot_runs() {
    use orator_core::TurnConfig;
    let bound = TurnConfig::default().barge_in_ms;
    assert_eq!(bound, 150, "shipped bound the grid is built around");
    // Grid runner: returns the list of mismatches, so the weakened
    // rule's failure is itself pinned (not just asserted away).
    fn check(bound_ms: u16) -> Vec<&'static str> {
        let mut failures = Vec::new();
        let mut window = ContinuityWindow::new();
        // Below the bound: 7 hot frames (140 ms) never fire.
        let mut fired = false;
        for _ in 0..7 {
            fired |= window.push(true, false, 20, bound_ms);
        }
        if fired {
            failures.push("below-bound run fired");
        }
        // At the bound: the 8th hot frame (160 ms) fires.
        if !window.push(true, false, 20, bound_ms) {
            failures.push("at-bound frame held");
        }
        // Above: continued heat keeps firing.
        if !window.push(true, false, 20, bound_ms) {
            failures.push("above-bound heat held");
        }
        // Pause-split: 5 hot, 2 cold, 5 hot — never 150 continuous.
        let mut split = ContinuityWindow::new();
        let mut split_fired = false;
        for _ in 0..5 {
            split_fired |= split.push(true, false, 20, bound_ms);
        }
        for _ in 0..2 {
            split_fired |= split.push(false, false, 20, bound_ms);
        }
        for _ in 0..5 {
            split_fired |= split.push(true, false, 20, bound_ms);
        }
        if split_fired {
            failures.push("pause-split run fired");
        }
        // Brief blip: 2 hot frames.
        let mut blip = ContinuityWindow::new();
        let mut blip_fired = false;
        for _ in 0..2 {
            blip_fired |= blip.push(true, false, 20, bound_ms);
        }
        if blip_fired {
            failures.push("brief blip fired");
        }
        // PTT cold: 10 hot votes with the latch off.
        let mut ptt = ContinuityWindow::new();
        let mut ptt_fired = false;
        for _ in 0..10 {
            ptt_fired |= ptt.push(true, true, 20, bound_ms);
        }
        if ptt_fired {
            failures.push("ptt-cold run fired");
        }
        // Cold start: first frame cold never fires.
        let mut cold = ContinuityWindow::new();
        if cold.push(false, false, 20, bound_ms) {
            failures.push("cold frame fired");
        }
        failures
    }
    assert!(
        check(bound).is_empty(),
        "shipped bound passes every row: {:?}",
        check(bound)
    );
    assert!(
        !check(0).is_empty(),
        "weakened bound-0 rule must fail the grid (blips would cut)"
    );
    // Vacuity: the grid covers firing and holding rows.
    assert!(
        !check(u16::MAX).is_empty(),
        "unreachable bound must fail the grid (nothing would ever cut)"
    );
}
