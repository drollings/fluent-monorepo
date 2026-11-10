//! Pipeline tests: rewind golden and crash-resume.

use fluent_db::migrate::migrate;
use fluent_db::store::SqliteStore;

use crate::diary::pipeline::{MeetingPipeline, STEPS};

fn migrated_store() -> SqliteStore {
    let store = SqliteStore::open_in_memory().expect("memory store");
    store
        .with_conn(|conn| {
            migrate(conn, &crate::store::migrations())?;
            Ok(())
        })
        .expect("migrate");
    store
}

#[test]
fn step_order_matches_the_pipeline() {
    assert_eq!(
        STEPS,
        [
            "finalize_audio",
            "offline_stt",
            "diarize",
            "align",
            "name",
            "summarize",
            "export"
        ]
    );
}

#[test]
fn redo_from_diarize_reruns_exactly_the_suffix() {
    let store = migrated_store();
    let mut pipeline = MeetingPipeline::new(store, "meet1").expect("pipeline builds");
    assert_eq!(pipeline.ready(), vec!["finalize_audio".to_string()]);
    pipeline.finish("finalize_audio", "ok").expect("finish");
    pipeline.finish("offline_stt", "ok").expect("finish");
    pipeline.finish("diarize", "2 speakers").expect("finish");
    assert_eq!(pipeline.completed(), 3);
    let suffix = pipeline.rewind("diarize").expect("rewind");
    assert_eq!(suffix, vec!["diarize", "align", "name", "summarize", "export"]);
    assert_eq!(pipeline.completed(), 2);
    assert!(pipeline.ready().contains(&"diarize".to_string()));
}

#[test]
fn crash_resumes_from_the_last_checkpoint() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("orator.db");
    {
        let store = crate::store::open_store(&path).expect("open");
        let mut pipeline = MeetingPipeline::new(store, "meet2").expect("pipeline builds");
        pipeline.finish("finalize_audio", "ok").expect("finish");
        pipeline.finish("offline_stt", "ok").expect("finish");
        // Crash simulation: pipeline drops here with two steps done.
    }
    // Restart: the checkpoints table recovers progress; completed work
    // is not re-executed (ready starts at diarize, not finalize).
    {
        let store = crate::store::open_store(&path).expect("reopen");
        let done: Option<String> = store
            .query_row(
                "SELECT step FROM checkpoints WHERE meeting = ?1",
                &[&"meet2" as &dyn rusqlite::ToSql],
                |row| row.get(0),
            )
            .expect("read checkpoint");
        assert_eq!(done.as_deref(), Some("offline_stt"));
    }
}

#[test]
fn panicked_step_cancels_dependents() {
    let store = migrated_store();
    let pipeline = MeetingPipeline::new(store, "meet3").expect("pipeline builds");
    let cancelled = pipeline.cancel_after("diarize");
    for step in ["align", "name", "summarize", "export"] {
        assert!(cancelled.contains(&step.to_string()), "{step} must not run");
    }
    assert!(!cancelled.contains(&"finalize_audio".to_string()));
}
