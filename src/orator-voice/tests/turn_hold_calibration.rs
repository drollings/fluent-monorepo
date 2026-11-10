//! CAL: endpoint-hold calibration on a labeled event corpus.
//!
//! Rule-level gate: the commit rule must never fire early on the control
//! group (hesitations, fillers, mid-utterance pauses) and must always fire
//! on complete turns and ceiling overruns. Precision and recall are both
//! required at 1.0 on this corpus before the holds ship.
//!
//! Scope note: these are scripted event sequences, so this calibrates the
//! *rule*, not the acoustics. Acoustic validation against real recordings
//! lands with the bench corpus; until then the holds below are config
//! values inside the documented band, not measured truths.

use orator::TurnConfig;

use crate::turn::machine::{TurnEvent, TurnMachine, TurnState};

struct Case {
    name: &'static str,
    events: Vec<TurnEvent>,
    must_commit: bool,
}

fn corpus() -> Vec<Case> {
    vec![
        Case {
            name: "hesitation pause",
            events: vec![
                TurnEvent::VadStart,
                TurnEvent::VadEnd,
                TurnEvent::SilenceElapsed {
                    ms: 150,
                    looks_complete: false,
                },
            ],
            must_commit: false,
        },
        Case {
            name: "filler word pause",
            events: vec![
                TurnEvent::VadStart,
                TurnEvent::VadEnd,
                TurnEvent::SilenceElapsed {
                    ms: 200,
                    looks_complete: false,
                },
            ],
            must_commit: false,
        },
        Case {
            name: "mid-utterance pause",
            events: vec![
                TurnEvent::VadStart,
                TurnEvent::VadEnd,
                TurnEvent::SilenceElapsed {
                    ms: 250,
                    looks_complete: false,
                },
            ],
            must_commit: false,
        },
        Case {
            name: "complete sentence short silence",
            events: vec![
                TurnEvent::VadStart,
                TurnEvent::VadEnd,
                TurnEvent::SilenceElapsed {
                    ms: 300,
                    looks_complete: true,
                },
            ],
            must_commit: true,
        },
        Case {
            name: "trailing conjunction resolved",
            events: vec![
                TurnEvent::VadStart,
                TurnEvent::VadEnd,
                TurnEvent::SilenceElapsed {
                    ms: 350,
                    looks_complete: true,
                },
                TurnEvent::VadStart,
                TurnEvent::VadEnd,
                TurnEvent::SilenceElapsed {
                    ms: 400,
                    looks_complete: true,
                },
            ],
            must_commit: true,
        },
        Case {
            name: "ceiling overrun without completion",
            events: vec![
                TurnEvent::VadStart,
                TurnEvent::VadEnd,
                TurnEvent::SilenceElapsed {
                    ms: 700,
                    looks_complete: false,
                },
            ],
            must_commit: true,
        },
    ]
}

#[test]
fn hold_rule_hits_precision_and_recall() {
    let mut true_commits = 0usize;
    let mut musts = 0usize;
    let mut false_commits = 0usize;
    for case in corpus() {
        let mut machine = TurnMachine::new(TurnConfig::default());
        for event in &case.events {
            machine.step(event);
        }
        let committed = machine.state() == TurnState::Endpointing;
        assert_eq!(
            committed, case.must_commit,
            "case `{}` decided wrong",
            case.name
        );
        if case.must_commit {
            musts += 1;
            true_commits += usize::from(committed);
        } else {
            false_commits += usize::from(committed);
        }
    }
    assert_eq!(false_commits, 0, "controls must never commit early");
    assert_eq!(true_commits, musts, "complete turns must always commit");
}
