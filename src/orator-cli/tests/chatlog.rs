//! `chatlog` goldens: timestamp shape, both renderers, latency
//! fields, verbosity filtering — all hermetic (fixed timestamps, no
//! I/O, no models).

use std::time::{Duration, UNIX_EPOCH};

use crate::chatlog::{
    format_timestamp, parse_log_format, render_json, render_text, ChatEvent, LogFormat,
};

fn at(millis: u64) -> std::time::SystemTime {
    UNIX_EPOCH + Duration::from_millis(millis)
}

#[test]
fn timestamp_renders_millis_with_a_fixed_clock() {
    // 14:22:01.042 UTC.
    let stamp = 14 * 3_600_000 + 22 * 60_000 + 1_000 + 42;
    assert_eq!(format_timestamp(at(stamp)), "[14:22:01.042]");
    assert_eq!(format_timestamp(at(0)), "[00:00:00.000]");
}

#[test]
fn unknown_log_format_names_both_spellings() {
    let error = parse_log_format("yaml").expect_err("yaml refuses");
    assert!(error.contains("text"), "{error}");
    assert!(error.contains("json"), "{error}");
    assert_eq!(parse_log_format("text"), Ok(LogFormat::Text));
    assert_eq!(parse_log_format("json"), Ok(LogFormat::Json));
}

#[test]
fn text_golden_names_latency_and_server() {
    let event = ChatEvent::Asr {
        at: at(14 * 3_600_000 + 22 * 60_000 + 4_890),
        text: "List top 3 system metrics in terminal.".to_string(),
        latency_ms: 245,
        model: "handy-computer/moonshine-streaming-medium".to_string(),
        backend: "whisper".to_string(),
    };
    assert_eq!(
        render_text(&event, false),
        "[14:22:04.890] 📝 ASR | \"List top 3 system metrics in terminal.\" [245ms] (handy-computer/moonshine-streaming-medium via whisper)"
    );
}

#[test]
fn text_system_row_has_no_latency_tail() {
    let event = ChatEvent::System {
        at: at(14 * 3_600_000 + 22 * 60_000 + 1_042),
        message: "Mode: Push-to-Record | Pipeline: Moonshine-Medium -> local LLM -> Kokoro"
            .to_string(),
    };
    let line = render_text(&event, false);
    assert!(
        line.starts_with("[14:22:01.042] ⚙ SYSTEM | Mode:"),
        "{line}"
    );
    assert!(!line.contains("ms"), "{line}");
}

#[test]
fn colored_text_wraps_the_header_only() {
    let event = ChatEvent::Error {
        at: at(0),
        message: "orator: no LLM server answers".to_string(),
    };
    let line = render_text(&event, true);
    assert!(
        line.starts_with("\u{1b}[36m[00:00:00.000] ❌ ERROR\u{1b}[0m | "),
        "{line:?}"
    );
    assert!(line.ends_with("orator: no LLM server answers"), "{line}");
}

#[test]
fn json_golden_carries_latency_model_backend() {
    let event = ChatEvent::Tts {
        at: at(14 * 3_600_000 + 22 * 60_000 + 5_210),
        message: "Synthesizing 42 words... [Streaming]".to_string(),
        latency_ms: 310,
        model: "agentory/kokoro-82m".to_string(),
        backend: "kokoro".to_string(),
    };
    let parsed: serde_json::Value = serde_json::from_str(&render_json(&event)).expect("valid JSON");
    assert_eq!(parsed["kind"], "TTS");
    assert_eq!(parsed["ts"], "[14:22:05.210]");
    assert_eq!(parsed["latency_ms"], 310);
    assert_eq!(parsed["model"], "agentory/kokoro-82m");
    assert_eq!(parsed["backend"], "kokoro");
    assert!(
        parsed["message"]
            .as_str()
            .is_some_and(|message| message.contains("42 words")),
        "{parsed}"
    );
}

#[test]
fn json_system_row_omits_absent_fields() {
    let event = ChatEvent::System {
        at: at(1_042),
        message: "ready".to_string(),
    };
    let parsed: serde_json::Value = serde_json::from_str(&render_json(&event)).expect("valid JSON");
    assert_eq!(parsed["kind"], "SYSTEM");
    assert!(parsed.get("latency_ms").is_none(), "{parsed}");
    assert!(parsed.get("model").is_none(), "{parsed}");
    assert!(parsed.get("backend").is_none(), "{parsed}");
}

#[test]
fn agent_detail_filters_at_default_verbosity() {
    let detail = ChatEvent::Agent {
        at: at(0),
        message: "Calling tool: get_system_metrics()".to_string(),
        latency_ms: 90,
        model: "liquidai/lfm3-audio-2b".to_string(),
        detail: true,
    };
    let reply = ChatEvent::Agent {
        at: at(0),
        message: "hi".to_string(),
        latency_ms: 90,
        model: "liquidai/lfm3-audio-2b".to_string(),
        detail: false,
    };
    let system = ChatEvent::System {
        at: at(0),
        message: "ready".to_string(),
    };
    assert!(!detail.shows_at(0), "tool detail hides at default");
    assert!(detail.shows_at(1), "verbosity 1 reveals tool detail");
    assert!(reply.shows_at(0), "replies always show with latency+model");
    assert!(system.shows_at(0), "system always shows");
    assert!(hidden(&detail, 0), "emit skips hidden rows");
    assert!(!hidden(&reply, 0), "emit keeps replies");
}

fn hidden(event: &ChatEvent, verbosity: u8) -> bool {
    // `emit` writes to stdout/stderr; this helper only proves the
    // filter decision the emitter consults.
    !event.shows_at(verbosity)
}

#[test]
fn error_rows_always_show_with_latency_free_text() {
    let event = ChatEvent::Error {
        at: at(0),
        message:
            "orator: local TTS exceeded its 10 s budget; turn stays interactive instead of stalling"
                .to_string(),
    };
    assert!(event.shows_at(0), "budget fire is never filtered");
    let line = render_text(&event, false);
    assert!(line.contains("exceeded its 10 s budget"), "{line}");
    let parsed: serde_json::Value = serde_json::from_str(&render_json(&event)).expect("valid JSON");
    assert_eq!(parsed["kind"], "ERROR");
}

#[test]
fn agent_reply_golden_names_latency_and_model() {
    let event = ChatEvent::Agent {
        at: at(14 * 3_600_000 + 22 * 60_000 + 5_120),
        message: "The top metrics are load, memory, and disk.".to_string(),
        latency_ms: 812,
        model: "liquidai/lfm3-audio-2b".to_string(),
        detail: false,
    };
    assert_eq!(
        render_text(&event, false),
        "[14:22:05.120] 🤖 AGENT | The top metrics are load, memory, and disk. [812ms] (liquidai/lfm3-audio-2b)"
    );
    let parsed: serde_json::Value = serde_json::from_str(&render_json(&event)).expect("valid JSON");
    assert_eq!(parsed["kind"], "AGENT");
    assert_eq!(parsed["latency_ms"], 812);
    assert_eq!(parsed["model"], "liquidai/lfm3-audio-2b");
}
