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

fn ticked_state(closes: &[f64]) -> IndicatorState {
    let mut state = IndicatorState::new();
    for (index, close) in closes.iter().enumerate() {
        state.push(
            BarTick {
                open: close - 1.0,
                high: close + 2.0,
                low: close - 2.0,
                close: *close,
                volume: f64::from(index as i32),
            },
            64,
        );
    }
    state
}

#[test]
fn scratch_windows_reproduce_the_leg_iterators_exactly() {
    // The reuse buffer carries exactly what the collecting iterators yield:
    // same legs, same order, same values — only the allocation is gone.
    let closes: Vec<f64> = (0..14).map(|i| 100.0 + f64::from(i) * 0.5).collect();
    let expected: Vec<f64> = closes.clone();
    let mut state = ticked_state(&closes);
    assert_eq!(state.scratch_closes(), expected.as_slice());
    let expected_highs: Vec<f64> = closes.iter().map(|c| c + 2.0).collect();
    let expected_lows: Vec<f64> = closes.iter().map(|c| c - 2.0).collect();
    let (highs, lows) = state.scratch_highs_lows();
    assert_eq!(highs, expected_highs.as_slice());
    assert_eq!(lows, expected_lows.as_slice());
}

#[test]
fn child_params_are_built_once_per_slot() {
    // Composed kernels read their child parameters through this cache: the
    // first request builds the set, later bars reuse the same allocation
    // (the pointer is stable), and each slot keeps its own set.
    let mut state = IndicatorState::new();
    let first = std::ptr::from_ref(
        state
            .child_with_params(0, || ParamSet::new().with_int("length", 7))
            .1,
    );
    let again = std::ptr::from_ref(
        state
            .child_with_params(0, || ParamSet::new().with_int("length", 99))
            .1,
    );
    assert_eq!(first, again, "a slot reuses its cached parameter set");
    assert_eq!(
        state
            .child_with_params(0, ParamSet::new)
            .1
            .int("child", "length")
            .expect("cached length"),
        7,
        "the cached set wins over a later build"
    );
    let other = std::ptr::from_ref(
        state
            .child_with_params(1, || ParamSet::new().with_int("length", 5))
            .1,
    );
    assert_ne!(first, other, "each slot owns its own parameter set");
}

#[test]
fn scratch_buffer_reuses_its_allocation_across_refills() {
    // Clear-and-refill, never re-collect: refilling with fewer bars reuses
    // the buffer the first fill earned (same pointer, contents exact).
    let mut state = ticked_state(&(0..14).map(f64::from).collect::<Vec<_>>());
    let pointer = state.scratch_closes().as_ptr();
    for tick in 0..5 {
        state.push(BarTick::close_only(100.0 + f64::from(tick)), 5);
    }
    let refilled = state.scratch_closes();
    assert_eq!(refilled, &[100.0, 101.0, 102.0, 103.0, 104.0]);
    assert_eq!(refilled.as_ptr(), pointer, "the refill reuses the buffer");
}

#[test]
fn scratch_pair_and_triple_split_stacked_legs_exactly() {
    let mut state = ticked_state(&[10.0, 20.0, 30.0]);
    let (highs, lows) = state.scratch_highs_lows();
    assert_eq!(highs, &[12.0, 22.0, 32.0]);
    assert_eq!(lows, &[8.0, 18.0, 28.0]);
    let (highs, lows, closes) = state.scratch_highs_lows_closes();
    assert_eq!(highs, &[12.0, 22.0, 32.0]);
    assert_eq!(lows, &[8.0, 18.0, 28.0]);
    assert_eq!(closes, &[10.0, 20.0, 30.0]);
    let mut empty = IndicatorState::new();
    assert!(empty.scratch_closes().is_empty(), "no bars paint no window");
    assert!(empty.scratch_highs_lows().0.is_empty());
    assert!(empty.scratch_highs_lows_closes().2.is_empty());
    assert!(empty.scratch_volumes().is_empty());
    assert!(empty.scratch_closes_volumes().1.is_empty());
}

#[test]
fn scratch_closes_volumes_stacks_both_legs_exactly() {
    let mut state = ticked_state(&[10.0, 20.0]);
    let (closes, volumes) = state.scratch_closes_volumes();
    assert_eq!(closes, &[10.0, 20.0]);
    assert_eq!(volumes, &[0.0, 1.0]);
    assert_eq!(state.scratch_volumes(), &[0.0, 1.0]);
}

/// Allocation profile for the incremental path: after migration no kernel
/// `update` collects a window `Vec` per bar — every retained-leg read rides
/// the state-owned scratch buffer. This pins the profile statically
/// (a counting allocator would need `unsafe`, which the crate forbids):
/// each migrated family must read zero here, batch-path output `Vec`s
/// untouched. The reuse itself is proved dynamically by
/// `scratch_buffer_reuses_its_allocation_across_refills` above.
#[test]
fn incremental_path_collects_no_window_per_bar() {
    for (family, source) in [
        ("sma", include_str!("../src/sma.rs")),
        ("ema", include_str!("../src/ema.rs")),
        ("averages", include_str!("../src/averages.rs")),
        ("oscillators", include_str!("../src/oscillators.rs")),
        ("volatility", include_str!("../src/volatility.rs")),
        ("ta_overlap", include_str!("../src/ta_overlap.rs")),
        ("ta_momentum", include_str!("../src/ta_momentum.rs")),
        ("ta_stat", include_str!("../src/ta_stat.rs")),
        ("ta_volume", include_str!("../src/ta_volume.rs")),
        ("filters", include_str!("../src/filters.rs")),
    ] {
        for leg in [".closes().collect()", ".highs().collect()", ".lows().collect()", ".volumes().collect()"] {
            assert!(
                !source.contains(leg),
                "{family} still collects a per-bar window via {leg}"
            );
        }
    }
}
