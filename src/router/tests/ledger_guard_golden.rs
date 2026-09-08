use crate::test_support::capture_logs;

#[test]
fn emit_write_audit_on_flagged() {
    let (_, logs) = capture_logs(|| {
        // In-memory: no `/tmp` file is created (SQLite `-wal`/`-shm`
        // sidecars of a deleted main file would otherwise be left behind).
        let ledger = crate::ledger::ContentNodeLedger::open_in_memory().unwrap();
        ledger.record_request("sess", "req", "Contact user@example.com now").unwrap();
    });
    let joined = logs.join("\n");
    assert!(joined.contains("write_path") || joined.contains("router.audit"), "audit emitted {joined}");
}
