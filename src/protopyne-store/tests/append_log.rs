//! The durable append-log contract: round-trip, batch atomicity, the
//! deterministic torn-tail rule, mid-file corruption refusal, and order
//! continuity across reopen.
//!
//! The rule is fixed, never heuristic: a single incomplete trailing record
//! with no bytes after it is dropped with a logged repair; a parse failure
//! with more data after it refuses as corruption. Every test below pins one
//! clause of that rule.

use super::*;
use crate::append_log::test_fixtures::Kv;

fn kv(k: &str, v: &str) -> Kv {
    Kv {
        key: k.to_owned(),
        value: v.to_owned(),
    }
}

fn open_rw(dir: &std::path::Path, name: &str) -> AppendLog<Kv> {
    AppendLog::open(&dir.join(name)).expect("log opens")
}

#[test]
fn round_trip_preserves_every_record_in_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut log = open_rw(dir.path(), "round.csv");
    let rows = vec![kv("a", "1"), kv("b", "2"), kv("c", "3")];
    log.append_batch(&rows).expect("batch appends");
    log.sync().expect("sync");
    assert_eq!(log.read_all().expect("replay"), rows);
    let reopened = open_rw(dir.path(), "round.csv");
    assert_eq!(reopened.read_all().expect("replay after reopen"), rows);
}

#[test]
fn empty_batches_append_nothing_and_reopen_clean() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut log = open_rw(dir.path(), "empty.csv");
    let before = log.last_good_offset();
    log.append_batch(&[]).expect("empty batch appends");
    assert_eq!(log.last_good_offset(), before, "an empty batch writes no bytes");
    let reopened = open_rw(dir.path(), "empty.csv");
    assert!(reopened.read_all().expect("replay").is_empty());
}

#[test]
fn a_failed_batch_leaves_the_pre_batch_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("atomic.csv");
    let mut log = AppendLog::<Kv>::open(&path).expect("log opens");
    let committed = vec![kv("a", "1"), kv("b", "2")];
    log.append_batch(&committed).expect("first batch lands");
    let good_offset = log.last_good_offset();
    // Fail the write partway through the second batch.
    log.fail_after_bytes(good_offset + 4);
    let batch = vec![kv("c", "3"), kv("d", "4"), kv("e", "5")];
    log.append_batch(&batch).expect_err("injected failure refuses");
    assert_eq!(
        log.last_good_offset(),
        good_offset,
        "the failed batch truncates back to the pre-batch offset"
    );
    assert_eq!(log.read_all().expect("replay"), committed);
    drop(log);
    let reopened = AppendLog::<Kv>::open(&path).expect("reopen");
    assert_eq!(
        reopened.read_all().expect("replay after reopen"),
        committed,
        "a reopen shows the pre-batch state, never a partial batch"
    );
}

#[test]
fn torn_tail_at_every_byte_offset_replays_the_prefix_and_repairs_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("torn.csv");
    let rows: Vec<Kv> = (0..8).map(|i| kv(&format!("k{i}"), &format!("v{i}"))).collect();
    let mut ends = Vec::new();
    {
        let mut log = AppendLog::<Kv>::open(&path).expect("log opens");
        for row in &rows {
            log.append_batch(std::slice::from_ref(row)).expect("row lands");
            ends.push(log.last_good_offset());
        }
    }
    let full = std::fs::read(&path).expect("log reads");
    let header_len = {
        let probe = AppendLog::<Kv>::open(&dir.path().join("probe.csv")).expect("probe opens");
        probe.last_good_offset()
    };
    // Every byte offset: the reopen replays exactly the records fully
    // before the cut, truncates the torn tail (or rewrites the header when
    // the cut lands inside it), and a second reopen changes nothing — the
    // repair happens exactly once.
    for cut in 0..full.len() {
        let trial = dir.path().join("trial.csv");
        std::fs::write(&trial, &full[..cut]).expect("trial writes");
        let trial_log = AppendLog::<Kv>::open(&trial).expect("torn tail always opens");
        let complete = ends.iter().filter(|end| **end <= cut as u64).count();
        assert_eq!(
            trial_log.read_all().expect("replay").len(),
            complete,
            "cut at byte {cut} replays the {complete}-record prefix"
        );
        let after_first = std::fs::read(&trial).expect("trial re-reads");
        let expected_len = ends[..complete].last().copied().unwrap_or(header_len);
        assert_eq!(
            after_first.len() as u64,
            expected_len,
            "cut at byte {cut} leaves exactly the committed prefix"
        );
        let second = AppendLog::<Kv>::open(&trial).expect("second open");
        let after_second = std::fs::read(&trial).expect("trial re-reads again");
        assert_eq!(after_first, after_second, "the repair happens exactly once");
        assert_eq!(second.read_all().expect("replay"), trial_log.read_all().expect("replay"));
    }
}

#[test]
fn mid_file_corruption_refuses_loudly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("corrupt.csv");
    {
        let mut log = AppendLog::<Kv>::open(&path).expect("log opens");
        let rows: Vec<Kv> = (0..4).map(|i| kv(&format!("k{i}"), &format!("v{i}"))).collect();
        log.append_batch(&rows).expect("rows land");
    }
    let mut bytes = std::fs::read(&path).expect("log reads");
    // Delete a comma in the middle of the file, keeping every line
    // terminated: two fields merge into one, so the row no longer decodes.
    // The damage has bytes after it, so it is corruption, never a torn tail.
    let middle = bytes.len() / 2;
    let comma = bytes[middle..]
        .iter()
        .position(|b| *b == b',')
        .map(|offset| middle + offset)
        .expect("a comma past the middle");
    bytes.remove(comma);
    std::fs::write(&path, &bytes).expect("corruption lands");
    match AppendLog::<Kv>::open(&path) {
        Err(StoreError::InvalidBar(reason)) => {
            assert!(reason.contains("corrupt") || reason.contains("line"), "the refusal names the damage: {reason}");
        }
        other => panic!("mid-file corruption must refuse loudly, got {other:?}"),
    }
}

#[test]
fn a_wrong_domain_or_version_refuses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("foreign.csv");
    std::fs::write(&path, "other_domain,1\nk,v\nk1,v1\n").expect("foreign log lands");
    match AppendLog::<Kv>::open(&path) {
        Err(StoreError::InvalidBar(reason)) => {
            assert!(reason.contains("domain") || reason.contains("version"), "the refusal names the mismatch: {reason}");
        }
        other => panic!("a foreign log must refuse, got {other:?}"),
    }
}

#[test]
fn order_continues_across_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("seq.csv");
    {
        let mut log = AppendLog::<Kv>::open(&path).expect("log opens");
        log.append_batch(&[kv("a", "1")]).expect("first lands");
    }
    {
        let mut log = AppendLog::<Kv>::open(&path).expect("reopen");
        log.append_batch(&[kv("b", "2")]).expect("second lands");
    }
    let log = AppendLog::<Kv>::open(&path).expect("final reopen");
    assert_eq!(log.read_all().expect("replay"), vec![kv("a", "1"), kv("b", "2")]);
}

#[test]
fn truncate_to_forgets_the_truncated_suffix() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("trunc.csv");
    let mut log = AppendLog::<Kv>::open(&path).expect("log opens");
    let rows: Vec<Kv> = (0..4).map(|i| kv(&format!("k{i}"), &format!("v{i}"))).collect();
    for row in &rows {
        log.append_batch(std::slice::from_ref(row)).expect("row lands");
    }
    log.truncate_to(0).expect("truncate");
    // Truncating to zero drops the header too: the next open rewrites it
    // and the log reads empty rather than corrupt.
    drop(log);
    let reopened = AppendLog::<Kv>::open(&path).expect("reopen after truncate");
    assert!(reopened.read_all().expect("replay").is_empty());
}
