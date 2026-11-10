//! Inbound orchestrator contract: validated posts, claim/resolve
//! round-trips, plan import, ledger tail, and pack requests. Every
//! refusal leaves existing state unchanged; every pack matches the
//! direct builder byte for byte.

use crate::maintenance::MaintenanceQueue;
use crate::orchestrator::{
    import_plan, post_work, request_pack, tail_ledger, ContractError, EscalationLedger,
    EscalationLife, PostWork,
};
use crate::plans::PlanItemState;
use crate::query::context_pack::PackInput;

fn plan_item(id: &str) -> crate::plans::PlanItem {
    crate::plans::PlanItem {
        id: id.to_string(),
        target: "src/a.rs".to_string(),
        acceptance: crate::plans::AcceptanceGate {
            package: "pkg".to_string(),
            test_name: None,
        },
        state: PlanItemState::Open,
    }
}

#[test]
fn post_then_tail_shows_the_item() {
    let mut queue = MaintenanceQueue::new();
    post_work(
        &mut queue,
        &PostWork {
            id: "a".to_string(),
            scope: "pkg".to_string(),
            kind: "rustfmt".to_string(),
            deps: Vec::new(),
        },
    )
    .expect("post a");
    post_work(
        &mut queue,
        &PostWork {
            id: "b".to_string(),
            scope: "pkg".to_string(),
            kind: "index".to_string(),
            deps: vec!["a".to_string()],
        },
    )
    .expect("post b with dep");
    assert_eq!(queue.ready(), vec!["a".to_string()]);
    let dir = fluent_wvr_testutil::tempdir();
    let tail = tail_ledger(dir.path(), 0, 10);
    assert!(tail.is_empty(), "empty journal tails empty, never errors");
}

#[test]
fn invalid_enum_or_hash_refuses_without_queue_change() {
    let mut queue = MaintenanceQueue::new();
    assert_eq!(
        post_work(
            &mut queue,
            &PostWork {
                id: String::new(),
                scope: "pkg".to_string(),
                kind: "rustfmt".to_string(),
                deps: Vec::new(),
            }
        ),
        Err(ContractError::EmptyId)
    );
    assert_eq!(
        post_work(
            &mut queue,
            &PostWork {
                id: "a".to_string(),
                scope: "pkg".to_string(),
                kind: "autonomous-feature".to_string(),
                deps: Vec::new(),
            }
        ),
        Err(ContractError::UnknownKind("autonomous-feature".to_string()))
    );
    assert_eq!(
        post_work(
            &mut queue,
            &PostWork {
                id: "b".to_string(),
                scope: "pkg".to_string(),
                kind: "index".to_string(),
                deps: vec!["ghost".to_string()],
            }
        ),
        Err(ContractError::DanglingDep("ghost".to_string()))
    );
    assert!(queue.ready().is_empty(), "refusals mutate nothing");
    post_work(
        &mut queue,
        &PostWork {
            id: "a".to_string(),
            scope: "pkg".to_string(),
            kind: "rustfmt".to_string(),
            deps: Vec::new(),
        },
    )
    .expect("post a");
    assert_eq!(
        post_work(
            &mut queue,
            &PostWork {
                id: "a".to_string(),
                scope: "pkg".to_string(),
                kind: "rustfmt".to_string(),
                deps: Vec::new(),
            }
        ),
        Err(ContractError::Duplicate("a".to_string()))
    );
}

#[test]
fn escalation_claim_resolve_round_trip() {
    let mut ledger = EscalationLedger::default();
    ledger.file("esc-1").expect("file");
    assert_eq!(
        ledger.claim("missing", "ana"),
        Err(ContractError::UnknownEscalation("missing".to_string()))
    );
    ledger.claim("esc-1", "ana").expect("claim");
    assert_eq!(
        ledger.claim("esc-1", "bob"),
        Err(ContractError::BadTransition("claim esc-1".to_string()))
    );
    assert_eq!(
        ledger.resolve("esc-1", "bob"),
        Err(ContractError::BadTransition("resolve esc-1".to_string())),
        "wrong owner cannot resolve"
    );
    ledger.resolve("esc-1", "ana").expect("resolve");
    assert_eq!(ledger.entries["esc-1"].state, EscalationLife::Resolved);
    assert_eq!(
        ledger.resolve("esc-1", "ana"),
        Err(ContractError::BadTransition("resolve esc-1".to_string()))
    );
}

#[test]
fn plan_import_and_pack_match_direct_builder() {
    let plan = import_plan("p", vec![plan_item("a")]).expect("import");
    assert_eq!(plan.order, vec!["a".to_string()]);
    let input = PackInput {
        target_source: "fn a() {}".to_string(),
        target_path: "src/a.rs".to_string(),
        diagnostic: None,
        callee_sigs: Vec::new(),
        callers: Vec::new(),
        siblings: Vec::new(),
        test_excerpts: Vec::new(),
        budget_tokens: 8000,
    };
    let (pack, manifest) = request_pack(&input);
    let (direct_pack, direct_manifest) = crate::query::context_pack::build_task_pack(&input);
    assert_eq!(pack, direct_pack, "contract pack matches the builder");
    assert_eq!(manifest, direct_manifest);
    assert!(!manifest.suspicious);
}

#[test]
fn ledgers_and_plans_serialize_for_file_handoff() {
    let mut ledger = EscalationLedger::default();
    ledger.file("esc-1").expect("file");
    ledger.claim("esc-1", "ana").expect("claim");
    let raw = serde_json::to_string(&ledger).expect("json");
    let back: EscalationLedger = serde_json::from_str(&raw).expect("parse");
    assert_eq!(back, ledger, "ledger survives a file round-trip");
}
