//! Bench-log tests: stat-file round-trip and histogram rendering.
//!
//! The single home for `BenchLog` plus `histograms`/`render_stats`;
//! `orator-bench` re-exports these for its replay harness.

use crate::bench::{histograms, render_stats, BenchKind, BenchLog};

#[test]
fn stat_file_round_trips() {
    let mut log = BenchLog::new();
    log.push(BenchKind::InputPcm, "stt", 0);
    log.push(BenchKind::Step, "stt", 40);
    log.push(BenchKind::StepPostSampling, "stt", 40);
    log.push(BenchKind::SendPcm, "stt", 80);
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("stat.jsonl");
    log.write_stat_file(&path).expect("writes");
    let back = BenchLog::read_stat_file(&path).expect("reads");
    assert_eq!(back, log);
}

#[test]
fn stats_name_every_stage() {
    let mut log = BenchLog::new();
    for (stage, latency) in [("stt", 80), ("tts_first_audio", 100)] {
        log.push(BenchKind::InputPcm, stage, 0);
        log.push(BenchKind::SendPcm, stage, latency);
    }
    let stats = histograms(&log);
    let rendered = render_stats(&stats);
    assert!(rendered.contains("stt"), "{rendered}");
    assert!(rendered.contains("tts_first_audio"), "{rendered}");
    assert!(rendered.contains("n=1"), "{rendered}");
}

#[test]
fn unpaired_inputs_never_render() {
    let mut log = BenchLog::new();
    log.push(BenchKind::InputPcm, "stt", 0);
    let rendered = render_stats(&histograms(&log));
    assert!(!rendered.contains("stt"), "open spans stay out: {rendered}");
}
