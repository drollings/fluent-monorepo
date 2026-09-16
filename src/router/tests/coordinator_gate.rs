use super::*;
use std::sync::Arc;

use crate::dag_session::SessionRegistry;
use crate::kv_cache::{ColdSnapshotIndex, HotSnapshotIndex, SnapshotStore};
use crate::ledger::orchestrator::{CoordinatorGate, LedgerAgentCoordinator, OrchestratorConfig};
use crate::ledger::prompt::{LodSpec, PromptBudget};
use crate::ledger::tiering::{LedgerTierWorker, TierConfig};
use crate::node_store::ContentNodeStore;
use crate::test_stubs::CountingBackend;
use fluent_llm::client::ChatBackend;

fn temp_store() -> Arc<ContentNodeStore> {
    Arc::new(ContentNodeStore::open_in_memory().unwrap())
}

fn fork_kv() -> SnapshotStore {
    use crate::instances::stub::StubServer;
    use crate::instances::InstanceClient;

    type SnapshotHandler = Arc<dyn Fn(&str, &str, &str) -> (u16, String) + Send + Sync>;
    let handler: SnapshotHandler = Arc::new(|method, path, _body| {
        if method == "POST" && path.ends_with("/snapshot") {
            (200, "{}".into())
        } else {
            (200, "[]".into())
        }
    });
    let stub = StubServer::start(handler);
    let fork = Arc::new(InstanceClient::new(
        reqwest::Client::new(),
        stub.base_url(),
        None,
    ));
    let dir = tempfile::tempdir().unwrap();
    let hot = Arc::new(HotSnapshotIndex::new(10, 1024));
    SnapshotStore::new(
        Arc::clone(&hot),
        Arc::new(ColdSnapshotIndex::new(dir.path(), 1024, 86400)),
    )
    .with_fork_io(fork)
}

fn coordinator_with() -> (Arc<LedgerAgentCoordinator>, Arc<CountingBackend>) {
    let store = temp_store();
    let sessions = Arc::new(SessionRegistry::with_kv_cache(fork_kv()));
    let tiers = Arc::new(LedgerTierWorker::new(
        Arc::clone(&store),
        Arc::new(crate::test_stubs::StubChatBackend::always(
            "SUMMARY: s\nDESCRIPTION: d",
        )),
        vec![4, 5],
        TierConfig {
            poll_interval_ms: 5,
            ..Default::default()
        },
        fluent_concurrency::tokio_runtime(),
    ));
    let counting = Arc::new(CountingBackend::new("agent answer"));
    let coord = LedgerAgentCoordinator::new(
        Arc::clone(&store),
        Arc::clone(&sessions),
        sessions.kv_cache().clone(),
        Arc::clone(&tiers),
        crate::ledger::prompt::LedgerPromptAssembler,
        Arc::clone(&counting) as Arc<dyn ChatBackend>,
        OrchestratorConfig {
            kv_policy: crate::dag_session::KvSnapshotPolicy::NeverRestore,
            budget: PromptBudget::new(8192),
            lod_spec: LodSpec::full(),
            role: "agent".into(),
        },
    );
    let _ = store;
    (Arc::new(coord), counting)
}

#[test]
fn gate_defaults_open() {
    let gate = CoordinatorGate::default();
    assert!(gate.is_open(), "fresh gate admits coordinator traffic");
    let gate = CoordinatorGate::open();
    assert!(gate.is_open());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_gate_dispatches_through_coordinator() {
    let (coord, counting) = coordinator_with();
    let gate = CoordinatorGate::open();
    let resp = coordinator_dispatch(Some(&coord), &gate, "sess-1", "model-a", "hello").await;
    assert!(resp.is_some(), "coordinator handled the request");
    assert_eq!(counting.calls(), 1, "exactly one agent turn ran");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closed_gate_falls_through_without_touching_the_backend() {
    let (coord, counting) = coordinator_with();
    let gate = CoordinatorGate::open();
    gate.set_open(false);
    assert!(!gate.is_open());
    let resp = coordinator_dispatch(Some(&coord), &gate, "sess-1", "model-a", "hello").await;
    assert!(resp.is_none(), "closed gate falls through to the pipeline");
    assert_eq!(counting.calls(), 0, "no agent turn ran while rolled back");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn absent_coordinator_falls_through_regardless_of_gate() {
    let gate = CoordinatorGate::open();
    assert!(coordinator_dispatch(None, &gate, "s", "m", "hi")
        .await
        .is_none());
    gate.set_open(false);
    assert!(coordinator_dispatch(None, &gate, "s", "m", "hi")
        .await
        .is_none());
}

#[test]
fn gate_reopens_after_close() {
    let gate = CoordinatorGate::open();
    gate.set_open(false);
    assert!(!gate.is_open());
    gate.set_open(true);
    assert!(
        gate.is_open(),
        "re-enable path exists (restart also restores)"
    );
}

#[test]
fn rollback_endpoint_closes_gate_idempotently() {
    use hyper::StatusCode;
    let gate = CoordinatorGate::open();
    let resp = crate::server::admin::handle_coordinator_rollback(&gate);
    assert_eq!(resp.status(), StatusCode::OK, "rollback reports success");
    assert!(!gate.is_open(), "gate closed by the endpoint");
    // Second call: idempotent, still 200, still closed.
    let resp = crate::server::admin::handle_coordinator_rollback(&gate);
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(!gate.is_open());
}
