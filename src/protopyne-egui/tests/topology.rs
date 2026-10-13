use super::*;
use protopyne_kernels::indicator::{ParamSet, builtin_registry};

fn position(order: &[String], node: &str) -> usize {
    order.iter().position(|n| n == node).unwrap_or_else(|| panic!("{node} ordered"))
}

#[test]
fn standard_topology_orders_nine_panes_dependencies_first() {
    let plan = standard_plan().expect("standard topology builds");
    let order = plan.order().expect("acyclic plan");
    assert_eq!(order.len(), 9, "every pane ordered exactly once: {order:?}");
    assert!(position(&order, PRICE_NODE) < position(&order, ADAPTIVE_NODE));
    assert!(position(&order, ADAPTIVE_NODE) < position(&order, SIGNAL_NODE));
    assert!(position(&order, SIGNAL_NODE) < position(&order, RIBBON_NODE));
    assert!(position(&order, ADAPTIVE_NODE) < position(&order, RIBBON_NODE));
    assert!(position(&order, RIBBON_NODE) < position(&order, HEALTH_RIBBON_NODE));
    assert!(position(&order, PRICE_NODE) < position(&order, HEALTH_MA_NODE));
    assert!(position(&order, ADAPTIVE_NODE) < position(&order, OSC_FAST_NODE));
    assert!(position(&order, ADAPTIVE_NODE) < position(&order, OSC_SLOW_NODE));
}

#[test]
fn standard_topology_readiness_and_cancel_scope() {
    let mut plan = standard_plan().expect("standard topology builds");
    assert_eq!(plan.ready_steps(), vec![PRICE_NODE.to_owned(), VOLUME_NODE.to_owned()]);
    plan.complete(PRICE_NODE);
    assert!(plan.is_ready(ADAPTIVE_NODE));
    assert!(plan.is_ready(HEALTH_MA_NODE));
    assert!(!plan.is_ready(RIBBON_NODE));
    plan.complete(ADAPTIVE_NODE);
    plan.complete(SIGNAL_NODE);
    assert!(plan.is_ready(RIBBON_NODE));

    let dirty = plan.dirty_set(SIGNAL_NODE);
    for node in [SIGNAL_NODE, RIBBON_NODE, HEALTH_RIBBON_NODE] {
        assert!(dirty.contains(&node.to_owned()), "{node} dirty");
    }
    for node in [PRICE_NODE, VOLUME_NODE, ADAPTIVE_NODE, OSC_FAST_NODE, OSC_SLOW_NODE, HEALTH_MA_NODE]
    {
        assert!(!dirty.contains(&node.to_owned()), "{node} stays clean");
    }
    let root_dirty = plan.dirty_set(PRICE_NODE);
    assert!(!root_dirty.contains(&VOLUME_NODE.to_owned()), "volume is independent of price");
    assert!(root_dirty.contains(&HEALTH_RIBBON_NODE.to_owned()));
}

#[test]
fn parameter_sets_key_panes_directly() {
    let params = ParamSet::new().with_int("length", 14).with_float("k", 1.2);
    let again = ParamSet::new().with_float("k", 1.2).with_int("length", 14);
    assert_eq!(params.fingerprint(), again.fingerprint(), "insertion order never matters");
    let expected = fluent_egui::cache_key(RIBBON_NODE, params.fingerprint());
    assert_eq!(cache_key_for(RIBBON_NODE, &params), expected, "parameter sets key directly");
    assert_ne!(
        cache_key_for(RIBBON_NODE, &params),
        cache_key_for(OSC_FAST_NODE, &params),
        "node names key distinctly"
    );
    assert_ne!(
        cache_key_for(RIBBON_NODE, &params),
        cache_key_for(RIBBON_NODE, &ParamSet::new()),
        "parameters key distinctly"
    );
}

#[test]
fn kernel_waves_order_through_the_registry() {
    let registry = builtin_registry().expect("builtin kernels register");
    let order = kernel_order(&registry, &["dema".to_owned()]).expect("dema orders");
    let ema = order.iter().position(|id| id == "ema").expect("ema ordered");
    let dema = order.iter().position(|id| id == "dema").expect("dema ordered");
    assert!(ema < dema, "dependencies execute first: {order:?}");
    assert!(kernel_order(&registry, &["no-such-kernel".to_owned()]).is_err());
}

#[test]
fn standard_replay_controller_carries_the_topology() {
    let controller = standard_replay().expect("standard replay builds");
    let order = controller.plan().order().expect("plan orders");
    assert_eq!(order.len(), 9, "replay controller holds nine panes");
    let key = cache_key_for(RIBBON_NODE, &ParamSet::new().with_int("length", 14));
    assert_eq!(controller.revision_of(key), None, "nothing published yet");
}
