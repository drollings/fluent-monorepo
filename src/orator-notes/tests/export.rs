//! Export goldens: the renderer is byte-deterministic — same events,
//! same file, no clock, no model. The daemon test pins the wiring;
//! this file pins the text.

use orator_store::StoredEvent;

use crate::export::render_meeting_export;

fn event(seq: i64, kind: &str, payload: &str) -> StoredEvent {
    StoredEvent {
        seq,
        session: "s1".to_string(),
        epoch: 0,
        kind: kind.to_string(),
        payload: payload.to_string(),
        wall_ms: 0,
    }
}

#[test]
fn scripted_session_renders_exact_bytes() {
    let events = vec![
        event(1, "note", "session s1 started (meeting)"),
        event(2, "transcript", "hello there"),
        event(3, "reply", "hi, ready to take notes"),
        event(4, "note", "session s1 stopped"),
    ];
    assert_eq!(
        render_meeting_export("s1", &events),
        "# meeting s1\n\
         \n\
         turns: 2\n\
         \n\
         ## record\n\
         1. [note] session s1 started (meeting)\n\
         2. [transcript] hello there\n\
         3. [reply] hi, ready to take notes\n\
         4. [note] session s1 stopped\n\
         \n\
         ## summary\n\
         hello there\n\
         …\n\
         hi, ready to take notes\n"
    );
}

#[test]
fn empty_session_still_renders_a_record() {
    assert_eq!(
        render_meeting_export("s9", &[]),
        "# meeting s9\n\
         \n\
         turns: 0\n\
         \n\
         ## record\n\
         *no turns recorded*\n\
         \n\
         ## summary\n\
         *no turns recorded*\n"
    );
}

#[test]
fn usage_counters_never_leak_into_the_record() {
    let events = vec![
        event(1, "note", "session s1 started (meeting)"),
        event(2, "usage", "{\"turns\": 1}"),
        event(3, "transcript", "hello"),
    ];
    let rendered = render_meeting_export("s1", &events);
    assert!(rendered.contains("turns: 1"), "{rendered}");
    assert!(!rendered.contains("usage"), "{rendered}");
}

#[test]
fn render_is_deterministic_across_calls() {
    let events = vec![event(1, "transcript", "first"), event(2, "reply", "second")];
    assert_eq!(
        render_meeting_export("s1", &events),
        render_meeting_export("s1", &events)
    );
}
