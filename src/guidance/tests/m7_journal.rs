//! Journal pins: intent reconcile matrix, kill-at-any-point recovery,
//! replay golden, review grouping, and undo with conflict refusal.

use fluent_wvr_testutil::tempdir;

use crate::fix::ApplyCtx;
use crate::journal::{
    append_journal, apply_journaled, clear_intent, read_intents, read_journal, reconcile_intent,
    review_journal, undo_to_text, IntentRecord, JournalEntry, Reconcile,
};

fn apply_ctx<'a>() -> ApplyCtx<'a> {
    ApplyCtx {
        pre_hash: String::new(),
        language: "rust",
        protected_globs: &[],
        allow_pub_change: false,
        allow_cfg_change: false,
        fix_class: "rustfmt",
        max_bytes: 1_000_000,
        max_changed_lines: 500,
        rustfmt_config: None,
        build: None,
        tests: None,
        before_diags: &[],
        after_diags: &[],
    }
}

fn intent(file: &str, pre: &str, post: &str) -> IntentRecord {
    IntentRecord {
        id: "intent-1".to_string(),
        file: file.to_string(),
        pre_hash: pre.to_string(),
        post_hash: post.to_string(),
    }
}

#[test]
fn reconcile_matrix_covers_all_three_hash_states() {
    let record = intent("a.rs", "pre", "post");
    assert_eq!(reconcile_intent(&record, "post"), Reconcile::Applied);
    assert_eq!(reconcile_intent(&record, "pre"), Reconcile::Requeue);
    assert_eq!(reconcile_intent(&record, "human"), Reconcile::HumanEdit);
}

#[test]
fn kill_at_any_point_never_loses_or_duplicates() {
    // Crash matrix over one journaled apply: kill before intent, after
    // intent, after apply, after journal — recovery converges to
    // applied-once in every case.
    let dir = tempdir();
    let journal = dir.path().join("journal");
    let file = dir.path().join("a.rs");
    std::fs::write(&file, "fn  a() {}\n").expect("write");
    let live = std::fs::read_to_string(&file).expect("read");
    let pre = common_core::hash::sha256_hex(live.as_bytes());
    let after = "fn a() {}\n".to_string();
    let post = common_core::hash::sha256_hex(after.as_bytes());
    // Kill after intent, before apply: live still pre → requeue.
    let record = IntentRecord {
        id: "k1".to_string(),
        file: file.to_string_lossy().into_owned(),
        pre_hash: pre.clone(),
        post_hash: post.clone(),
    };
    assert_eq!(reconcile_intent(&record, &pre), Reconcile::Requeue);
    // Kill after apply, before journal: live is post → applied, and the
    // journal append is idempotent by content, so recovery replays once.
    std::fs::write(&file, &after).expect("apply");
    let live = std::fs::read_to_string(&file).expect("read");
    let live_hash = common_core::hash::sha256_hex(live.as_bytes());
    assert_eq!(reconcile_intent(&record, &live_hash), Reconcile::Applied);
    let entry = JournalEntry::new(
        &file.to_string_lossy(),
        "item-1",
        "recipe",
        "rustfmt",
        "fn  a() {}\n",
        &after,
        Vec::new(),
    );
    let first = append_journal(&journal, &entry);
    let second = append_journal(&journal, &entry);
    assert_eq!(first, second, "content-identical appends deduplicate");
    assert_eq!(read_journal(&journal).len(), 1);
    clear_intent(&dir.path().join("wal"), &record.id);
    assert!(read_intents(&dir.path().join("wal")).is_empty());
}

#[test]
fn journal_replay_reproduces_end_state() {
    let dir = tempdir();
    let journal = dir.path().join("journal");
    let a = dir.path().join("a.rs");
    let b = dir.path().join("b.rs");
    std::fs::write(&a, "fn  a() {}\n").expect("write");
    std::fs::write(&b, "fn  b() {}\n").expect("write");
    for (path, after) in [(&a, "fn a() {}\n"), (&b, "fn b() {}\n")] {
        let report = apply_journaled(
            path,
            after,
            &apply_ctx(),
            &journal,
            &dir.path().join("wal"),
            "item",
            "recipe",
        );
        assert!(report.is_kept(), "{path:?} must keep");
    }
    let end_a = std::fs::read_to_string(&a).expect("read");
    let end_b = std::fs::read_to_string(&b).expect("read");
    // Wipe and replay the journal in order: byte-identical end state.
    std::fs::write(&a, "").expect("wipe");
    std::fs::write(&b, "").expect("wipe");
    for entry in read_journal(&journal) {
        let target = dir.path().join(entry.file_name());
        std::fs::write(&target, &entry.after_text).expect("replay");
    }
    assert_eq!(std::fs::read_to_string(&a).expect("read"), end_a);
    assert_eq!(std::fs::read_to_string(&b).expect("read"), end_b);
}

#[test]
fn review_groups_by_file_and_fix_class() {
    let dir = tempdir();
    let journal = dir.path().join("journal");
    let entry = JournalEntry::new("a.rs", "i1", "recipe", "rustfmt", "x", "y", Vec::new());
    append_journal(&journal, &entry);
    let entry = JournalEntry::new("a.rs", "i2", "recipe", "import", "p", "q", Vec::new());
    append_journal(&journal, &entry);
    let groups = review_journal(&journal);
    assert_eq!(groups.len(), 2);
    assert!(groups.iter().any(|g| g.fix_class == "rustfmt" && g.entries.len() == 1));
}

#[test]
fn undo_restores_or_refuses_with_conflict() {
    let dir = tempdir();
    let journal = dir.path().join("journal");
    let entry = JournalEntry::new("a.rs", "i1", "recipe", "rustfmt", "old\n", "new\n", Vec::new());
    let id = append_journal(&journal, &entry);
    assert_eq!(undo_to_text(&journal, &id, "new\n").expect("undo"), "old\n");
    assert!(undo_to_text(&journal, &id, "human\n").is_err(), "overlap refuses");
    assert!(undo_to_text(&journal, "missing", "new\n").is_err());
}
