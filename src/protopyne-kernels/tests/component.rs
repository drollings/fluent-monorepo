use super::*;
use crate::indicator::{
    Indicator, IndicatorResult, IndicatorSpec, IndicatorState, Ohlcv, ParamSet,
};
use crate::sma::SmaIndicator;
use fluent_wvr::{Component, WorkContext, WorkUnit};
use std::sync::Arc;

/// A test indicator with spec dependencies, for ordering tests.
struct ChainIndicator {
    spec: IndicatorSpec,
}

impl ChainIndicator {
    fn new(id: &str, depends: Vec<String>) -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(id.to_owned())
                .display_name(id.to_owned())
                .warmup_bars(1_usize)
                .depends(depends)
                .build(),
        }
    }
}

impl Indicator for ChainIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, _params: &ParamSet) -> Result<usize, crate::error::KernelError> {
        Ok(1)
    }

    fn batch(
        &self,
        data: &Ohlcv<'_>,
        _params: &ParamSet,
    ) -> Result<IndicatorResult, crate::error::KernelError> {
        Ok(IndicatorResult::new(
            self.spec.id.clone(),
            vec![None; data.len()],
        ))
    }

    fn update(
        &self,
        _state: &mut IndicatorState,
        _bar: crate::indicator::BarTick,
        _params: &ParamSet,
    ) -> Result<Option<f64>, crate::error::KernelError> {
        Ok(None)
    }
}

fn sma_params() -> ParamSet {
    ParamSet::new().with_int("length", 3)
}

fn closes_input() -> Arc<OwnedOhlcv> {
    Arc::new(OwnedOhlcv {
        close: vec![1.0, 2.0, 3.0, 4.0, 5.0],
        ..OwnedOhlcv::default()
    })
}

#[test]
fn component_executes_batch_with_spec_identity() {
    let indicator: Arc<dyn Indicator> = Arc::new(SmaIndicator::new());
    let unit = IndicatorComponent::builder(Arc::clone(&indicator))
        .param_int("length", 3)
        .input(closes_input())
        .build();
    assert_eq!(unit.name(), "sma");
    let ctx = WorkContext::default();
    let output = unit.execute(&ctx).expect("batch executes");
    assert!(output.success);
    let direct = indicator
        .batch(&closes_input().views(), &sma_params())
        .expect("direct batch");
    let decoded: IndicatorResult = output.data_take().expect("typed output round-trips");
    assert_eq!(decoded, direct, "the adapter must not skew values");
}

#[test]
fn component_wires_spec_dependencies() {
    let indicator: Arc<dyn Indicator> =
        Arc::new(ChainIndicator::new("child", vec!["sma".to_string()]));
    let unit = IndicatorComponent::new(Arc::clone(&indicator), sma_params(), closes_input());
    let depends: Vec<String> = unit.depends().iter().map(ToString::to_string).collect();
    assert_eq!(depends, vec!["sma".to_string()]);
    let provides: Vec<String> = unit.provides().iter().map(ToString::to_string).collect();
    assert_eq!(provides, vec!["child".to_string()]);
}

#[test]
fn shared_registry_refuses_duplicates() {
    let registry = ComponentRegistry::new();
    registry
        .register(Arc::new(SmaIndicator::new()))
        .expect("first registration");
    let err = registry
        .register(Arc::new(SmaIndicator::new()))
        .expect_err("duplicate refused");
    assert!(err.to_string().contains("sma"));
    assert_eq!(registry.len(), 1);
}

#[test]
fn shared_registry_orders_dependencies_first() {
    let registry = ComponentRegistry::new();
    registry
        .register(Arc::new(SmaIndicator::new()))
        .expect("sma");
    registry
        .register(Arc::new(ChainIndicator::new(
            "child",
            vec!["sma".to_string()],
        )))
        .expect("child");
    let order = registry
        .execution_order(&["child".to_string()])
        .expect("order resolves");
    assert_eq!(order, vec!["sma".to_string(), "child".to_string()]);
    assert!(registry.execution_order(&["ghost".to_string()]).is_err());
}

#[test]
fn doubles_override_execute_without_touching_kernels() {
    use crate::component::indicator_double;
    let indicator: Arc<dyn Indicator> = Arc::new(SmaIndicator::new());
    let base: Arc<dyn Component> = Arc::new(IndicatorComponent::new(
        indicator,
        sma_params(),
        closes_input(),
    ));
    let canned = IndicatorResult::new("sma".to_string(), vec![Some(1.0)]);
    let double = indicator_double(base, &canned);
    assert_eq!(double.name(), "sma");
    let output = double
        .execute(&WorkContext::default())
        .expect("double executes");
    let decoded: IndicatorResult = output.data_take().expect("typed output round-trips");
    assert_eq!(decoded, canned);
}

#[test]
fn double_names_its_kernel_in_the_message() {
    // Loud by contract: the canned message names the doubled kernel, so a
    // payload failure would surface in the message instead of a null success.
    use crate::component::indicator_double;
    let indicator: Arc<dyn Indicator> = Arc::new(SmaIndicator::new());
    let base: Arc<dyn Component> = Arc::new(IndicatorComponent::new(
        indicator,
        sma_params(),
        closes_input(),
    ));
    let canned = IndicatorResult::new("sma".to_string(), vec![Some(1.0)]);
    let double = indicator_double(base, &canned);
    let output = double
        .execute(&WorkContext::default())
        .expect("double executes");
    assert!(
        output.message.contains("doubled sma"),
        "got: {}",
        output.message
    );
}

#[test]
fn dynamic_handles_run_bound_kernels() {
    use crate::component::dynamic_indicator;
    let indicator: Arc<dyn Indicator> = Arc::new(SmaIndicator::new());
    let handle = dynamic_indicator(Arc::clone(&indicator), sma_params(), closes_input());
    let output = handle
        .execute(&WorkContext::default())
        .expect("dynamic executes");
    let decoded: IndicatorResult = output.data_take().expect("typed output round-trips");
    let direct = indicator
        .batch(&closes_input().views(), &sma_params())
        .expect("direct batch");
    assert_eq!(decoded, direct);
}
