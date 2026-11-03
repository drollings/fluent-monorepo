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
