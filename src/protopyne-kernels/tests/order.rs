//! One topological sort, exercised through both registries.
//!
//! The owned and shared registries keep different storage (an owned map versus
//! shared handles) and must stay separate, but they feed the *same* sorter. These
//! pins are what keeps that true: identical orders for the same graph, and the
//! same refusals — an unknown id by name, a cycle by the names in the loop.

use std::sync::Arc;

use super::*;

use crate::component::ComponentRegistry;
use crate::indicator::{
    builtin_registry, execution_order, BarTick, Indicator, IndicatorResult, IndicatorSpec,
    IndicatorState, Ohlcv, ParamSet,
};
use crate::KernelError;
use fluent_dag::dep_graph::GraphError;

/// A kernel with declared edges and no math: ordering never evaluates it.
struct Stub {
    spec: IndicatorSpec,
}

impl Stub {
    fn new(id: &str, depends: &[&str]) -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(id.to_owned())
                .display_name(id.to_owned())
                .warmup_bars(1_usize)
                .depends(depends.iter().map(|dep| (*dep).to_owned()).collect())
                .build(),
        }
    }
}

impl Indicator for Stub {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn batch(&self, _data: &Ohlcv<'_>, _params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        Ok(IndicatorResult::new(self.spec.id.clone(), Vec::new()))
    }

    fn update(
        &self,
        _state: &mut IndicatorState,
        _bar: BarTick,
        _params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        Ok(None)
    }
}

fn owned_with(stubs: &[Stub]) -> crate::indicator::IndicatorRegistry {
    let mut registry = crate::indicator::IndicatorRegistry::new();
    for stub in stubs {
        registry
            .register(Arc::new(Stub::new(&stub.spec.id, &deps_of(stub))))
            .expect("unique ids");
    }
    registry
}

fn deps_of(stub: &Stub) -> Vec<&str> {
    stub.spec.depends.iter().map(String::as_str).collect()
}

fn shared_with(stubs: &[Stub]) -> ComponentRegistry {
    let registry = ComponentRegistry::new();
    for stub in stubs {
        registry
            .register(Arc::new(Stub::new(&stub.spec.id, &deps_of(stub))))
            .expect("unique ids");
    }
    registry
}

#[test]
fn both_registries_order_the_same_graph_identically() {
    let owned = builtin_registry().expect("builtins register");
    let shared = ComponentRegistry::from_builtins().expect("builtins register");
    for requested in [
        vec!["sma".to_owned()],
        vec!["trima".to_owned()],
        vec!["dema".to_owned()],
        vec!["tema".to_owned()],
        vec!["macd".to_owned()],
        vec!["adosc".to_owned()],
        vec!["trima".to_owned(), "macd".to_owned()],
    ] {
        let from_owned = execution_order(&owned, &requested).expect("the owned registry orders");
        let from_shared =
            execution_order_from(&shared, &requested).expect("the shared registry orders");
        assert_eq!(
            from_owned, from_shared,
            "{requested:?}: one sorter, one order"
        );
        // Dependencies really do precede their dependents, in both.
        let position = |id: &String| from_owned.iter().position(|seen| seen == id);
        for id in &from_owned {
            let indicator = owned.get(id).expect("ordered ids are registered");
            for dependency in &indicator.spec().depends {
                if let (Some(dep), Some(consumer)) = (position(dependency), position(id)) {
                    assert!(
                        dep < consumer,
                        "{id} must follow its dependency {dependency}"
                    );
                }
            }
        }
    }
}

#[test]
fn an_unknown_id_is_refused_by_name_in_both_registries() {
    let owned = builtin_registry().expect("builtins register");
    let shared = ComponentRegistry::from_builtins().expect("builtins register");
    let requested = vec!["not_a_kernel".to_owned()];
    for error in [
        execution_order(&owned, &requested).expect_err("unknown id refuses"),
        execution_order_from(&shared, &requested).expect_err("unknown id refuses"),
    ] {
        match error {
            GraphError::NodeNotFound(named) => assert_eq!(named, "not_a_kernel"),
            other => panic!("expected NodeNotFound, got {other:?}"),
        }
    }
}

#[test]
fn a_cycle_is_refused_by_name_in_both_registries() {
    let stubs = [
        Stub::new("cyc_a", &["cyc_b"]),
        Stub::new("cyc_b", &["cyc_a"]),
    ];
    let owned = owned_with(&stubs);
    let shared = shared_with(&stubs);
    let requested = vec!["cyc_a".to_owned()];
    for error in [
        execution_order(&owned, &requested).expect_err("a cycle refuses"),
        execution_order_from(&shared, &requested).expect_err("a cycle refuses"),
    ] {
        match error {
            GraphError::CircularDependency(named) => {
                assert!(named.contains("cyc_a"), "the loop names cyc_a: {named}");
                assert!(named.contains("cyc_b"), "the loop names cyc_b: {named}");
            }
            other => panic!("expected CircularDependency, got {other:?}"),
        }
    }
}
