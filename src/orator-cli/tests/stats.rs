//! Stat-file rendering tests: missing files fail with the envelope
//! (EXIT-1 equivalent `Err`), valid files render exactly the bytes the
//! core renderer produces, corrupt lines name the file and line.

use crate::stats::render_stat_file;

#[test]
fn missing_stat_file_fails_with_prefixed_remedy() {
    let error = render_stat_file(std::path::Path::new("/nonexistent-orator/stat.jsonl"))
        .expect_err("missing file fails");
    assert!(error.starts_with("orator: "), "{error}");
    assert!(error.contains("/nonexistent-orator/stat.jsonl"), "{error}");
    assert!(error.contains("check the path"), "{error}");
}

#[test]
fn valid_stat_file_renders_core_identical_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("stat.jsonl");
    let mut log = orator_core::bench::BenchLog::new();
    log.push(orator_core::bench::BenchKind::InputPcm, "voice", 100);
    log.push(orator_core::bench::BenchKind::SendPcm, "voice", 150);
    log.write_stat_file(&path).expect("fixture writes");
    let rendered = render_stat_file(&path).expect("valid file renders");
    let expected = orator_core::bench::render_stats(&orator_core::bench::histograms(
        &orator_core::bench::BenchLog::read_stat_file(&path).expect("core reads"),
    ));
    assert_eq!(rendered, expected, "success bytes unchanged");
    assert!(rendered.contains("voice"), "{rendered}");
}

#[test]
fn corrupt_stat_line_names_file_and_line() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("stat.jsonl");
    std::fs::write(
        &path,
        "{\"kind\":\"InputPcm\",\"stage\":\"a\",\"t_ms\":1}\nnot json\n",
    )
    .expect("fixture writes");
    let error = render_stat_file(&path).expect_err("corrupt line fails");
    assert!(error.starts_with("orator: "), "{error}");
    assert!(error.contains("line 2"), "{error}");
}

#[test]
fn empty_stat_file_renders_empty_log() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("empty.jsonl");
    std::fs::write(&path, "\n").expect("fixture writes");
    let rendered = render_stat_file(&path).expect("empty file renders");
    let expected = orator_core::bench::render_stats(&orator_core::bench::histograms(
        &orator_core::bench::BenchLog::new(),
    ));
    assert_eq!(rendered, expected);
}
