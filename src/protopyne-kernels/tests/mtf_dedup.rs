//! Parametric dependency edges: composer kernels declare the frame-computable
//! sub-series they compose with real parameters, so a sub-key requested
//! directly and declared as a dependency resolves to one cache entry that
//! computes once.
//!
//! The contract pinned here: `Indicator::deps_with_params` maps a composer's
//! own parameters to `(kernel id, sub-parameters)` pairs; `build_graph`
//! resolves those into real `SeriesKey` edges (same span, real params, the
//! request's leg selection) instead of lifting default-params keys nobody
//! asked for. Sub-passes over *intermediate* series (second TRIMA pass,
//! MACD signal line) stay inline inside the kernel and never become edges.
//! Static `spec.depends` keeps the id-level ordering channel
//! (`execution_order`, supervision `depends`) and must name exactly the ids
//! the parametric form can return.

use super::*;
use crate::{
    BarTick, Indicator, IndicatorId, IndicatorRegistry, IndicatorResult, IndicatorSpec,
    IndicatorState, Ohlcv, ParamSet, SmaIndicator, TrimaIndicator,
};
use protopyne_types::Bar;
use rust_decimal::Decimal;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Minute-aligned base for synthetic tapes.
const T0: i64 = 1_699_999_980_000;
const MIN: i64 = 60_000;

/// One synthetic closed bar: bucket-open stamp, distinct OHLCV on an
/// arithmetic progression.
fn bar_at(open_ms: i64, step: i64, volume: u64) -> Bar {
    let cents = 10_000 + step * 5;
    Bar::new()
        .ts(chrono::DateTime::from_timestamp(open_ms / 1000, 0).expect("in range"))
        .o(Decimal::new(cents, 2))
        .h(Decimal::new(cents + 12, 2))
        .l(Decimal::new(cents - 12, 2))
        .c(Decimal::new(cents + 2, 2))
        .volume(volume)
        .maybe_adj(None)
        .build()
}

/// Ten aligned minute bars: closes `100.02 + 0.05*i`, volumes `100*(i+1)`.
fn ten_bars() -> Vec<Bar> {
    (0..10).map(|i| bar_at(T0 + i * MIN, i, 100 * (i as u64 + 1))).collect()
}

fn stored() -> MtfStore {
    let mut store = MtfStore::new();
    store.set_base(&ten_bars()).expect("base loads");
    store
}

/// Deterministic 3-bar mean over closes shared by the doubles below.
fn mean3(close: &[f64]) -> Vec<Option<f64>> {
    let mut values = Vec::with_capacity(close.len());
    for at in 0..close.len() {
        let from = at.saturating_sub(2);
        let slice = &close[from..=at];
        let mean = slice.iter().sum::<f64>() / slice.len() as f64;
        values.push(if at < 2 { None } else { Some(mean) });
    }
    values
}

/// Counting leaf double: no dependencies, batch calls counted.
struct Leaf {
    spec: IndicatorSpec,
    calls: Arc<AtomicUsize>,
}

impl Leaf {
    fn new(calls: &Arc<AtomicUsize>) -> Arc<Self> {
        Arc::new(Self {
            spec: IndicatorSpec::new()
                .id("leaf".to_owned())
                .display_name("counting leaf".to_owned())
                .warmup_bars(2_usize)
                .build(),
            calls: Arc::clone(calls),
        })
    }
}

impl Indicator for Leaf {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, _params: &ParamSet) -> Result<usize, KernelError> {
        Ok(2)
    }

    fn batch(&self, data: &Ohlcv<'_>, _params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(IndicatorResult::new(self.spec.id.clone(), mean3(data.close)))
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

/// Counting composer double: own `length` halves to the leaf's `length`
/// (integer halving), declared both as the static ordering id and through
/// the parametric form. Invalid lengths declare nothing; the compute step
/// surfaces the parameter error.
struct Composer {
    spec: IndicatorSpec,
    calls: Arc<AtomicUsize>,
}

impl Composer {
    fn new(calls: &Arc<AtomicUsize>) -> Arc<Self> {
        Arc::new(Self {
            spec: IndicatorSpec::new()
                .id("composer".to_owned())
                .display_name("counting composer".to_owned())
                .warmup_bars(2_usize)
                .depends(vec!["leaf".to_owned()])
                .build(),
            calls: Arc::clone(calls),
        })
    }

    fn leaf_len(params: &ParamSet) -> Option<usize> {
        params
            .int("composer", "length")
            .ok()
            .and_then(|len| usize::try_from(len).ok())
            .map(|len| len / 2)
            .filter(|len| *len >= 1)
    }
}

impl Indicator for Composer {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, _params: &ParamSet) -> Result<usize, KernelError> {
        Ok(2)
    }

    fn batch(&self, data: &Ohlcv<'_>, _params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(IndicatorResult::new(self.spec.id.clone(), mean3(data.close)))
    }

    fn update(
        &self,
        _state: &mut IndicatorState,
        _bar: BarTick,
        _params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        Ok(None)
    }

    fn deps_with_params(&self, params: &ParamSet) -> Vec<(IndicatorId, ParamSet)> {
        match Self::leaf_len(params) {
            Some(len) => vec![(
                "leaf".to_owned(),
                ParamSet::new().with_int("length", len as i64),
            )],
            None => Vec::new(),
        }
    }
}

/// Declared-but-unregistered deps refuse at graph build (fail-closed, never
/// an empty order downstream).
struct GhostComposer {
    spec: IndicatorSpec,
}

impl Indicator for GhostComposer {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, _params: &ParamSet) -> Result<usize, KernelError> {
        Ok(1)
    }

    fn batch(&self, data: &Ohlcv<'_>, _params: &ParamSet) -> Result<IndicatorResult, KernelError> {
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

    fn deps_with_params(&self, _params: &ParamSet) -> Vec<(IndicatorId, ParamSet)> {
        vec![("ghost".to_owned(), ParamSet::new())]
    }
}

fn doubles_registry(leaf_calls: &Arc<AtomicUsize>, composer_calls: &Arc<AtomicUsize>) -> IndicatorRegistry {
    let mut registry = IndicatorRegistry::new();
    registry.register(Leaf::new(leaf_calls)).expect("leaf registers");
    registry.register(Composer::new(composer_calls)).expect("composer registers");
    registry
}

fn leaf_key(length: i64) -> SeriesKey {
    SeriesKey::new(MIN, "leaf", &ParamSet::new().with_int("length", length)).expect("leaf key")
}

fn composer_key(length: i64) -> SeriesKey {
    SeriesKey::new(MIN, "composer", &ParamSet::new().with_int("length", length))
        .expect("composer key")
}

fn composer_params(length: i64) -> ParamSet {
    ParamSet::new().with_int("length", length)
}

fn default_leaf_key() -> SeriesKey {
    SeriesKey::new(MIN, "leaf", &ParamSet::new()).expect("default leaf key")
}

#[test]
fn shared_parametric_dep_computes_once() {
    let leaf_calls = Arc::new(AtomicUsize::new(0));
    let composer_calls = Arc::new(AtomicUsize::new(0));
    let registry = doubles_registry(&leaf_calls, &composer_calls);
    let mut store = stored();
    let now = T0 + 10 * MIN;
    // Composer(20) needs leaf(10); leaf(10) is also requested directly.
    let requests = vec![
        (composer_key(20), composer_params(20)),
        (leaf_key(10), ParamSet::new().with_int("length", 10)),
    ];
    let ordered = store
        .ensure(&registry, &requests, now, Lookahead::Off)
        .expect("parametric fill");
    assert_eq!(leaf_calls.load(Ordering::SeqCst), 1, "shared sub-key computes once");
    assert_eq!(composer_calls.load(Ordering::SeqCst), 1, "composer computes once");
    let cached = store.cache(&leaf_key(10)).expect("dep key cached");
    assert_eq!(cached.values.len(), 10, "dep series covers the frame");
    assert!(
        store.cache(&default_leaf_key()).is_none(),
        "no default-params squatter caches a series nobody asked for"
    );
    let leaf_pos = ordered.iter().position(|k| k == &leaf_key(10)).expect("dep ordered");
    let composer_pos = ordered.iter().position(|k| k == &composer_key(20)).expect("composer ordered");
    assert!(leaf_pos < composer_pos, "dep-first order");
}

#[test]
fn distinct_param_keys_compute_separately() {
    let leaf_calls = Arc::new(AtomicUsize::new(0));
    let composer_calls = Arc::new(AtomicUsize::new(0));
    let registry = doubles_registry(&leaf_calls, &composer_calls);
    let mut store = stored();
    let now = T0 + 10 * MIN;
    // Composer(20) needs leaf(10); leaf(7) is a different series.
    let requests = vec![
        (composer_key(20), composer_params(20)),
        (leaf_key(7), ParamSet::new().with_int("length", 7)),
    ];
    store
        .ensure(&registry, &requests, now, Lookahead::Off)
        .expect("distinct keys fill");
    assert_eq!(leaf_calls.load(Ordering::SeqCst), 2, "one compute per series key");
    assert_eq!(composer_calls.load(Ordering::SeqCst), 1, "composer computes once");
    assert!(store.cache(&leaf_key(10)).is_some(), "dep key cached");
    assert!(store.cache(&leaf_key(7)).is_some(), "direct key cached");
    assert!(store.cache(&default_leaf_key()).is_none(), "no default-params squatter");
}

#[test]
fn parametric_edges_are_dep_first_with_real_params() {
    let leaf_calls = Arc::new(AtomicUsize::new(0));
    let composer_calls = Arc::new(AtomicUsize::new(0));
    let registry = doubles_registry(&leaf_calls, &composer_calls);
    let roots = vec![(composer_key(20), composer_params(20))];
    let (graph, dep_params) = build_graph(&registry, &roots).expect("parametric graph");
    assert!(graph.nodes().contains(&leaf_key(10)), "real-params dep is a node");
    assert!(!graph.nodes().contains(&default_leaf_key()), "default lift is gone");
    let order = order_for(&graph, &[composer_key(20)]).expect("order");
    let leaf_pos = order.iter().position(|k| k == &leaf_key(10)).expect("dep ordered");
    let composer_pos = order.iter().position(|k| k == &composer_key(20)).expect("composer ordered");
    assert!(leaf_pos < composer_pos, "dep-first order");
    // Resolved parameters travel with the graph so the compute step reads
    // the dep's real params, never a default fallback.
    assert_eq!(
        dep_params.get(&leaf_key(10)).map(ParamSet::fingerprint),
        Some(ParamSet::new().with_int("length", 10).fingerprint()),
        "dep params resolve"
    );
    // Leg selection follows the request: a volume-leg parent declares
    // volume-leg deps, a close-leg parent close-leg deps.
    let vol_root = SeriesKey::new(MIN, "composer", &composer_params(20))
        .expect("composer key")
        .with_volume_source();
    let (vol_graph, _) = build_graph(&registry, &[(vol_root, composer_params(20))]).expect("graph");
    let dep = vol_graph.nodes().iter().find(|k| k.kernel == "leaf").expect("dep node");
    assert!(dep.volume_source, "dep inherits the request's leg selection");
}

#[test]
fn unknown_parametric_dep_refuses() {
    let mut registry = IndicatorRegistry::new();
    registry
        .register(Arc::new(GhostComposer {
            spec: IndicatorSpec::new()
                .id("haunt".to_owned())
                .display_name("ghost composer".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }))
        .expect("ghost registers");
    let key = SeriesKey::new(MIN, "haunt", &ParamSet::new()).expect("key");
    assert!(build_graph(&registry, &[(key, ParamSet::new())]).is_err(), "ghost dep refuses");
}

fn length_params(length: i64) -> ParamSet {
    ParamSet::new().with_int("length", length)
}

#[test]
fn builtin_composer_edges_use_real_params() {
    let registry = crate::indicator::builtin_registry().expect("builtins load");
    let sma = |length: i64| {
        SeriesKey::new(MIN, "sma", &length_params(length)).expect("sma key")
    };
    let ema = |length: i64| {
        SeriesKey::new(MIN, "ema", &length_params(length)).expect("ema key")
    };
    let ad = SeriesKey::new(MIN, "ad", &ParamSet::new()).expect("ad key");
    let macd_params = ParamSet::new()
        .with_int("fast_length", 12)
        .with_int("slow_length", 26)
        .with_int("signal_length", 9);
    let adosc_params = ParamSet::new().with_int("fast_length", 3).with_int("slow_length", 10);
    let cases: Vec<(&str, ParamSet, Vec<SeriesKey>)> = vec![
        ("trima", length_params(20), vec![sma(10), sma(11)]),
        ("dema", length_params(14), vec![ema(14)]),
        ("tema", length_params(14), vec![ema(14)]),
        ("macd", macd_params, vec![ema(12), ema(26)]),
        ("adosc", adosc_params, vec![ad]),
    ];
    for (id, params, deps) in &cases {
        let key = SeriesKey::new(MIN, id, params).expect("composer key");
        let (graph, _) =
            build_graph(&registry, &[(key.clone(), params.clone())]).expect("composer graphs");
        for dep in deps {
            assert!(graph.nodes().contains(dep), "{id} resolves dep {dep}");
        }
        // No default-params squatter for any dep kernel that was not
        // explicitly declared with default params.
        for dep in deps {
            let squatter =
                SeriesKey::new(MIN, &dep.kernel, &ParamSet::new()).expect("squatter key");
            if !deps.contains(&squatter) {
                assert!(!graph.nodes().contains(&squatter), "{id} lifts no {squatter}");
            }
        }
        let order = order_for(&graph, std::slice::from_ref(&key)).expect("composer orders");
        let composer_pos = order.iter().position(|k| k == &key).expect("composer ordered");
        for dep in deps {
            let dep_pos = order.iter().position(|k| k == dep).expect("dep ordered");
            assert!(dep_pos < composer_pos, "{id}: {dep} precedes composer");
        }
    }
    // The MACD signal line smooths the intermediate MACD line, not the
    // frame: it stays inline and never becomes an edge.
    let macd_key = SeriesKey::new(MIN, "macd", &cases[3].1).expect("macd key");
    let (macd_graph, _) = build_graph(&registry, &[(macd_key, cases[3].1.clone())]).expect("graph");
    assert!(!macd_graph.nodes().contains(&ema(9)), "signal pass stays inline");
}

#[test]
fn builtin_execution_order_is_dep_first() {
    let registry = crate::indicator::builtin_registry().expect("builtins load");
    let order = crate::execution_order(&registry, &["trima".to_owned()]).expect("trima orders");
    assert_eq!(order, vec!["sma".to_owned(), "trima".to_owned()], "sma precedes trima");
    let order = crate::execution_order(&registry, &["dema".to_owned()]).expect("dema orders");
    assert_eq!(order, vec!["ema".to_owned(), "dema".to_owned()], "ema precedes dema");
    let order = crate::execution_order(&registry, &["macd".to_owned()]).expect("macd orders");
    assert_eq!(order, vec!["ema".to_owned(), "macd".to_owned()], "ema precedes macd");
    let order = crate::execution_order(&registry, &["adosc".to_owned()]).expect("adosc orders");
    assert_eq!(order, vec!["ad".to_owned(), "adosc".to_owned()], "ad precedes adosc");
}

#[test]
fn static_and_parametric_deps_agree() {
    let registry = crate::indicator::builtin_registry().expect("builtins load");
    let macd_params = ParamSet::new()
        .with_int("fast_length", 12)
        .with_int("slow_length", 26)
        .with_int("signal_length", 9);
    let adosc_params = ParamSet::new().with_int("fast_length", 3).with_int("slow_length", 10);
    let cases: Vec<(&str, ParamSet)> = vec![
        ("trima", length_params(20)),
        ("dema", length_params(14)),
        ("tema", length_params(14)),
        ("macd", macd_params),
        ("adosc", adosc_params),
    ];
    for (id, params) in &cases {
        let kernel = registry.get(id).unwrap_or_else(|| panic!("{id} registered"));
        let mut from_params: Vec<String> =
            kernel.deps_with_params(params).iter().map(|(dep, _)| dep.clone()).collect();
        from_params.sort();
        from_params.dedup();
        let mut from_spec = kernel.spec().depends.clone();
        from_spec.sort();
        assert_eq!(from_params, from_spec, "{id}: static ids match parametric ids");
    }
}

#[test]
fn trima_store_values_match_direct_batch() {
    let registry = crate::indicator::builtin_registry().expect("builtins load");
    let mut store = stored();
    let now = T0 + 10 * MIN;
    let trima_params = length_params(20);
    let sma_params = length_params(10);
    let trima_key = SeriesKey::new(MIN, "trima", &trima_params).expect("trima key");
    let sma_key = SeriesKey::new(MIN, "sma", &sma_params).expect("sma key");
    store
        .ensure(
            &registry,
            &[
                (trima_key.clone(), trima_params.clone()),
                (sma_key.clone(), sma_params.clone()),
            ],
            now,
            Lookahead::Off,
        )
        .expect("composed fill");
    let frame = store.frame(MIN).expect("frame materialized");
    let direct_trima = TrimaIndicator::new()
        .batch(&frame.views(), &trima_params)
        .expect("direct trima");
    assert_eq!(
        store.cache(&trima_key).expect("trima cached").values,
        direct_trima.values,
        "store trima equals direct batch"
    );
    let direct_sma = SmaIndicator::new()
        .batch(&frame.views(), &sma_params)
        .expect("direct sma");
    assert_eq!(
        store.cache(&sma_key).expect("sma cached").values,
        direct_sma.values,
        "shared sub-key equals direct batch"
    );
    let squatter = SeriesKey::new(MIN, "sma", &ParamSet::new()).expect("squatter key");
    assert!(store.cache(&squatter).is_none(), "no default-params squatter");
}

#[test]
fn kernel_unit_depends_provides_derive_from_spec() {
    use crate::timed::KernelUnit;
    use fluent_wvr::prelude::WorkUnit;
    let input = Arc::new(OwnedOhlcv {
        open: vec![],
        high: vec![],
        low: vec![],
        close: vec![1.0],
        volume: vec![],
    });
    let trima = KernelUnit::new(Arc::new(TrimaIndicator::new()), length_params(20), input.clone());
    let depends: Vec<String> = trima.depends().iter().map(ToString::to_string).collect();
    assert_eq!(depends, vec!["sma".to_owned()], "composer unit depends on its sub-kernel");
    let provides: Vec<String> = trima.provides().iter().map(ToString::to_string).collect();
    assert_eq!(provides, vec!["trima".to_owned()], "unit provides its own id");
    let sma = KernelUnit::new(Arc::new(SmaIndicator::new()), length_params(10), input);
    assert!(sma.depends().is_empty(), "leaf unit depends on nothing");
    let provides: Vec<String> = sma.provides().iter().map(ToString::to_string).collect();
    assert_eq!(provides, vec!["sma".to_owned()], "leaf provides its own id");
}
