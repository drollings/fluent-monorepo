//! Event-log tests: append cursors, paged replay, ephemeral convention.

use crate::events::{EventLog, REPLAY_PAGE};

#[test]
fn append_assigns_monotonic_cursors() {
    let log = EventLog::in_memory().expect("in-memory log");
    let first = log.append("s1", 1, "SessionStarted", "{}", 1000).expect("append");
    let second = log.append("s1", 1, "ReplyFinal", "{\"text\":\"hi\"}", 1001).expect("append");
    assert!(second.seq > first.seq);
    assert!(first.seq >= 1);
    assert_eq!(log.latest().expect("latest"), Some(second.seq));
}

#[test]
fn replay_pages_without_gaps_or_dupes() {
    let log = EventLog::in_memory().expect("in-memory log");
    let count = 120usize;
    for index in 0..count {
        log.append("s1", 1, "ReplyFinal", &format!("{{\"n\":{index}}}"), 1000 + index as u64)
            .expect("append");
    }
    // Reconnect from mid-stream: page through and demand contiguity.
    let since = 40i64;
    let mut seen: Vec<i64> = Vec::new();
    let mut cursor = since;
    loop {
        let page = log.replay(cursor, 25).expect("replay");
        if page.is_empty() {
            break;
        }
        for event in &page {
            assert!(event.seq > cursor || !seen.contains(&event.seq));
            cursor = cursor.max(event.seq);
        }
        seen.extend(page.iter().map(|event| event.seq));
        if page.len() < 25 {
            break;
        }
    }
    assert_eq!(seen.len(), count - since as usize);
    let mut sorted = seen.clone();
    sorted.sort_unstable();
    assert_eq!(seen, sorted, "replay must arrive oldest-first");
    sorted.dedup();
    assert_eq!(sorted.len(), seen.len(), "zero dupes");
    for pair in sorted.windows(2) {
        assert_eq!(pair[1], pair[0] + 1, "zero gaps");
    }
}

#[test]
fn replay_respects_the_page_cap() {
    assert_eq!(REPLAY_PAGE, 5_000);
    let log = EventLog::in_memory().expect("in-memory log");
    for index in 0..10 {
        log.append("s1", 1, "ReplyFinal", &format!("{index}"), index).expect("append");
    }
    let page = log.replay(0, usize::MAX).expect("replay");
    assert!(page.len() <= REPLAY_PAGE);
}

#[test]
fn ephemerals_never_persist_nor_move_the_cursor() {
    let log = EventLog::in_memory().expect("in-memory log");
    let durable = log.append("s1", 1, "ReplyFinal", "hi", 1000).expect("append");
    let live = log.publish_ephemeral("s1", 1, "SttPartial", "typing", 1001);
    assert!(live.seq < 0);
    assert!(!live.durable());
    assert!(durable.durable());
    // Reconnect from zero sees only the durable row.
    let replayed = log.replay(0, 100).expect("replay");
    assert_eq!(replayed.len(), 1);
    assert_eq!(replayed[0].seq, durable.seq);
    assert_eq!(log.latest().expect("latest"), Some(durable.seq));
    // Live subscribers see both.
    let mut rx = log.subscribe();
    log.publish_ephemeral("s1", 1, "VadStart", "", 1002);
    let got = rx.try_recv().expect("live fan-out");
    assert!(got.seq < 0);
}

#[test]
fn dictation_history_is_a_filtered_view_not_a_second_store() {
    let log = EventLog::in_memory().expect("in-memory log");
    log.append("s1", 0, "note", "session started", 1000).expect("append");
    log.append("s1", 0, "dictation", "One.", 1001).expect("append");
    log.append("s1", 0, "reply", "answer", 1002).expect("append");
    log.append("s1", 0, "dictation", "Two.", 1003).expect("append");
    let history = log.replay_kind("dictation", 0, 100).expect("replay kind");
    assert_eq!(
        history.iter().map(|event| event.payload.as_str()).collect::<Vec<_>>(),
        vec!["One.", "Two."],
        "history view holds clean dictated text in order"
    );
    assert_eq!(log.count_kind("dictation", 0).expect("count"), 2);
    assert_eq!(log.count_kind("reply", 0).expect("count"), 1);
    // Unfiltered replay still sees everything: views filter, never hide.
    assert_eq!(log.replay(0, 100).expect("replay").len(), 4);
}

#[test]
fn usage_counters_read_without_replaying_payloads() {
    let log = EventLog::in_memory().expect("in-memory log");
    for index in 0..3 {
        log.append(
            "s1",
            0,
            "usage",
            &format!("{{\"stt_secs\":{index}}}"),
            1000 + index,
        )
        .expect("append");
    }
    assert_eq!(log.count_kind("usage", 0).expect("count"), 3);
    assert_eq!(log.count_kind("dictation", 0).expect("count"), 0);
}

#[test]
fn lagged_live_receivers_skip_never_block() {
    let log = EventLog::in_memory().expect("in-memory log");
    let mut rx = log.subscribe();
    // Flood past the channel bound (10): the slow receiver lags.
    for index in 0..25 {
        log.publish_ephemeral("s1", 0, "VadPartial", &format!("{index}"), 1000 + index);
    }
    // Drain with Lagged-skip: stale generations are skipped, the
    // newest still arrives. Single producer, bounded fan-out.
    let mut newest = None;
    loop {
        match rx.try_recv() {
            Ok(event) => newest = Some(event),
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(
                tokio::sync::broadcast::error::TryRecvError::Empty
                | tokio::sync::broadcast::error::TryRecvError::Closed,
            ) => break,
        }
    }
    assert_eq!(
        newest.expect("newest arrives").payload,
        "24",
        "lagged receivers skip to the newest"
    );
}

#[test]
fn kill_mid_stream_reconnects_without_gaps_or_dupes() {
    use fluent_db::migrate::migrate;
    // Durability golden: the writer dies mid-stream (drop without
    // close) and the reader reconnects from a mid-stream cursor.
    // Replayed rows must be contiguous with zero gaps or dupes.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("events.db");
    {
        let store = fluent_db::store::SqliteStore::open(&path).expect("open");
        store
            .with_conn(|conn| {
                migrate(conn, &crate::schema::migrations())?;
                Ok(())
            })
            .expect("migrate");
        let log = EventLog::over(store);
        for index in 0..60 {
            log.append("s1", 1, "ReplyFinal", &format!("{{\"n\":{index}}}"), 1000 + index as u64)
                .expect("append");
        }
        // Drop without close: the kill under test.
    }
    let store = fluent_db::store::SqliteStore::open(&path).expect("reopen");
    let log = EventLog::over(store);
    let mut seen: Vec<i64> = Vec::new();
    let mut cursor = 20i64;
    loop {
        let page = log.replay(cursor, 25).expect("replay");
        if page.is_empty() {
            break;
        }
        for event in &page {
            cursor = cursor.max(event.seq);
        }
        seen.extend(page.iter().map(|event| event.seq));
        if page.len() < 25 {
            break;
        }
    }
    assert_eq!(seen.len(), 40, "reconnect resumes at the cursor: {seen:?}");
    for pair in seen.windows(2) {
        assert_eq!(pair[1], pair[0] + 1, "zero gaps after kill: {seen:?}");
    }
    let mut sorted = seen.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), seen.len(), "zero dupes after kill");
}
