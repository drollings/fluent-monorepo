use super::*;
use fluent_dag::dep_graph::GraphError;

fn spec(id: &str, depends: &[&str]) -> IndicatorSpec {
    IndicatorSpec::new()
        .id(id.to_owned())
        .display_name(id.to_owned())
        .warmup_bars(1_usize)
        .depends(depends.iter().map(ToString::to_string).collect())
        .build()
}

struct Stub {
    spec: IndicatorSpec,
}

impl Indicator for Stub {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, _params: &ParamSet) -> Result<usize, KernelError> {
        Ok(1)
    }

    fn batch(
        &self,
        data: &Ohlcv<'_>,
        _params: &ParamSet,
    ) -> Result<IndicatorResult, KernelError> {
        Ok(IndicatorResult::new(
            self.spec.id.clone(),
            data.close.iter().map(|&c| Some(c)).collect(),
        ))
    }

    fn update(
        &self,
        _state: &mut IndicatorState,
        bar: BarTick,
        _params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        Ok(Some(bar.close))
    }
}

fn registry_with(order: &[(&str, &[&str])]) -> IndicatorRegistry {
    let mut registry = IndicatorRegistry::new();
    for (id, depends) in order {
        registry
            .register(Arc::new(Stub {
                spec: spec(id, depends),
            }))
            .expect("test registration");
    }
    registry
}

#[test]
fn registry_lookup_and_duplicate_rejection() {
    let mut registry = IndicatorRegistry::new();
    registry
        .register(Arc::new(Stub {
            spec: spec("sma", &[]),
        }))
        .expect("first registration");
    assert!(registry.get("sma").is_some());
    assert!(registry.get("nope").is_none());
    let dup = registry.register(Arc::new(Stub {
        spec: spec("sma", &[]),
    }));
    assert!(matches!(dup, Err(KernelError::DuplicateIndicator(_))));
}

#[test]
fn topo_order_respects_depends() {
    let registry = registry_with(&[("signal", &["sma"]), ("sma", &[])]);
    let order = execution_order(&registry, &["signal".to_owned()]).expect("acyclic");
    assert_eq!(order, vec!["sma".to_owned(), "signal".to_owned()]);
}

#[test]
fn cycle_surfaces_circular_dependency() {
    let registry = registry_with(&[("a", &["b"]), ("b", &["a"])]);
    let err = execution_order(&registry, &["a".to_owned()]).expect_err("must fail");
    assert!(matches!(err, GraphError::CircularDependency(_)));
}

#[test]
fn unknown_requested_or_unregistered_dep_is_not_found() {
    let registry = registry_with(&[("sma", &[])]);
    let err = execution_order(&registry, &["nope".to_owned()]).expect_err("unknown root");
    assert!(matches!(err, GraphError::NodeNotFound(_)));

    let dangling = registry_with(&[("signal", &["ghost"])]);
    let err = execution_order(&dangling, &["signal".to_owned()]).expect_err("unknown dep");
    assert!(matches!(err, GraphError::NodeNotFound(_)));
}
