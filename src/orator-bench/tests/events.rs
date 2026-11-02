//! Event tests: stat-file round-trip, ordering, atomicity.

use crate::events::{BenchKind, BenchLog};

#[test]
fn stat_file_round_trips() {
    let mut log = BenchLog::new();
    log.push(BenchKind::InputPcm, "stt", 0);
    log.push(BenchKind::Step, "stt", 40);
    log.push(BenchKind::StepPostSampling, "stt", 40);
    log.push(BenchKind::SendPcm, "stt", 80);
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("stats.jsonl");
    log.write_stat_file(&path).expect("write");
    let back = BenchLog::read_stat_file(&path).expect("read");
    assert_eq!(back, log);
}

#[test]
fn stat_file_is_never_half_written() {
    let mut log = BenchLog::new();
    log.push(BenchKind::Step, "stt", 1);
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("stats.jsonl");
    std::fs::write(&path, "old\n").expect("old file");
    log.write_stat_file(&path).expect("atomic write");
    let text = std::fs::read_to_string(&path).expect("read");
    assert!(text.contains("stt"), "new contents landed");
    assert!(!text.contains("old"), "old contents replaced atomically");
    assert!(
        !std::fs::read_dir(dir.path())
            .expect("list")
            .any(|entry| entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")),
        "no temp files leak"
    );
}
