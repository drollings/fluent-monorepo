use super::*;
use crate::lod::Lod;
use crate::lod_policy::{LodSpec, PromptBudget, RelevanceSignal, WorkerContext};
use crate::views::ParallelLedger;
use fluent_concept::content_store::InMemoryContentNodeStore;
use fluent_types::ContentNode;
use std::sync::Arc;

fn node(content: &str, session: &str) -> ContentNode {
    ContentNode {
        lod: vec![content.to_string()],
        session_id: Some(session.to_string()),
        ..Default::default()
    }
}

fn store() -> Arc<InMemoryContentNodeStore> {
    let s = Arc::new(InMemoryContentNodeStore::new());
    s.insert(node("alpha first", "s"));
    s.insert(node("beta middle one", "s"));
    s.insert(node("beta middle two", "s"));
    s.insert(node("omega last", "s"));
    s
}

fn worker() -> WorkerContext {
    WorkerContext::new("reviewer", "Be terse.")
}

#[test]
fn anchors_always_lod0_even_beyond_budget() {
    let s = store();
    let view = ParallelLedger::for_session(s, "s");
    let asm = LedgerPromptAssembler;
    let out = asm.assemble(
        &view,
        &worker(),
        &PromptBudget::new(10),
        None,
        &LodSpec::full(),
    );
    let plan = out.node_plan;
    assert_eq!(plan.first().map(|(_, l)| l), Some(&Lod::LOD0));
    assert_eq!(plan.last().map(|(_, l)| l), Some(&Lod::LOD0));
    assert!(out.system.contains("reviewer"));
}

#[test]
fn no_signal_uses_uniform_mid_tier() {
    let s = store();
    let view = ParallelLedger::for_session(s, "s");
    let asm = LedgerPromptAssembler;
    let out = asm.assemble(
        &view,
        &worker(),
        &PromptBudget::new(10_000),
        None,
        &LodSpec::full(),
    );
    // Intermediates render at the band midpoint (LOD3); anchors at LOD0.
    assert_eq!(out.node_plan.len(), 4);
    assert!(out.node_plan[1..3].iter().all(|(_, l)| *l == Lod::LOD3));
}

#[test]
fn tight_budget_drops_intermediates_keeps_anchors() {
    let s = store();
    let view = ParallelLedger::for_session(s, "s");
    let asm = LedgerPromptAssembler;
    let out = asm.assemble(
        &view,
        &worker(),
        &PromptBudget::new(0),
        None,
        &LodSpec::full(),
    );
    assert_eq!(out.node_plan.len(), 2, "only anchors survive a zero budget");
}

#[test]
fn relevance_orders_finer_first() {
    let s = Arc::new(InMemoryContentNodeStore::new());
    let near = ContentNode {
        lod: vec!["near query".to_string()],
        session_id: Some("s".to_string()),
        embedding: Some(vec![1.0, 0.0]),
        ..Default::default()
    };
    let far = ContentNode {
        lod: vec!["far away".to_string()],
        session_id: Some("s".to_string()),
        embedding: Some(vec![0.0, 1.0]),
        ..Default::default()
    };
    s.insert(node("anchor first", "s"));
    let near_id = s.insert(near);
    let far_id = s.insert(far);
    s.insert(node("anchor last", "s"));
    let view = ParallelLedger::for_session(s, "s");
    let asm = LedgerPromptAssembler;
    let out = asm.assemble(
        &view,
        &worker(),
        &PromptBudget::new(10_000),
        Some(&RelevanceSignal::query(vec![1.0, 0.0])),
        &LodSpec::full(),
    );
    let pos = |id| out.node_plan.iter().position(|(n, _)| *n == id).unwrap();
    let near_lod = out.node_plan[pos(near_id)].1;
    let far_lod = out.node_plan[pos(far_id)].1;
    assert!(near_lod <= far_lod, "near renders finer than far");
}

#[test]
fn empty_view_assembles_empty() {
    let s: Arc<InMemoryContentNodeStore> = Arc::new(InMemoryContentNodeStore::new());
    let view = ParallelLedger::for_session(s, "nope");
    let asm = LedgerPromptAssembler;
    let out = asm.assemble(
        &view,
        &worker(),
        &PromptBudget::new(100),
        None,
        &LodSpec::full(),
    );
    assert!(out.node_plan.is_empty());
    assert!(out.body.is_empty());
}
