//! Shared multi-timeframe store: one cache per series key, dependency
//! ordering through the canonical graph, revision stamps with backdating.
//!
//! The store owns no kernel math: frames resample through the M2 bucket
//! core and kernels run through `Indicator::batch` over borrowed views, so
//! every value asserted here is reproducible outside the store.
//! `#[allow(clippy::float_cmp)]` covers the hand-computed SMA literals.
#![allow(clippy::float_cmp)]

use super::*;
use crate::{
    BarTick, Indicator, IndicatorRegistry, IndicatorResult, IndicatorSpec, IndicatorState, Ohlcv,
    ParamSet,
};
use protopyne_types::{Bar, Timeframe};
use rust_decimal::Decimal;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Minute-aligned base for synthetic tapes.
const T0: i64 = 1_699_999_980_000;
const MIN: i64 = 60_000;

/// One synthetic closed bar: bucket-open stamp, distinct OHLCV on an
/// arithmetic progression (hand-computable aggregates).
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

/// Counting kernel double: SMA-3 over closes (deterministic, so backdating
/// is observable), batch calls counted. The store consumes the same
/// `Arc<dyn Indicator>` a supervision seam would wrap — the double never
/// touches components because the store calls `batch` directly by design.
struct CountMean {
    spec: IndicatorSpec,
    calls: Arc<AtomicUsize>,
}

impl CountMean {
    fn named(id: &str, depends: &[&str], calls: &Arc<AtomicUsize>) -> Arc<Self> {
        Arc::new(Self {
            spec: IndicatorSpec::new()
                .id(id.to_owned())
                .display_name(format!("counting double {id}"))
                .warmup_bars(2)
                .depends(depends.iter().map(|dep| (*dep).to_owned()).collect())
                .build(),
            calls: Arc::clone(calls),
        })
    }
}

impl Indicator for CountMean {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, _params: &ParamSet) -> Result<usize, KernelError> {
        Ok(2)
    }

    fn batch(&self, data: &Ohlcv<'_>, _params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut values = Vec::with_capacity(data.close.len());
        for at in 0..data.close.len() {
            let from = at.saturating_sub(2);
            let slice = &data.close[from..=at];
            let mean = slice.iter().sum::<f64>() / slice.len() as f64;
            values.push(if at < 2 { None } else { Some(mean) });
        }
        Ok(IndicatorResult::new(self.spec.id.clone(), values))
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

fn registry_with(doubles: Vec<Arc<CountMean>>) -> IndicatorRegistry {
    let mut registry = IndicatorRegistry::new();
    for double in doubles {
        registry.register(double).expect("double registers");
    }
    registry
}

fn key(tf_ms: i64, kernel: &str) -> SeriesKey {
    SeriesKey::new(tf_ms, kernel, &ParamSet::new()).expect("valid key")
}

fn stored() -> MtfStore {
    let mut store = MtfStore::new();
    store.set_base(&ten_bars()).expect("base loads");
    store
}

#[test]
fn shared_subexpression_computed_once() {
    let calls = Arc::new(AtomicUsize::new(0));
    let registry = registry_with(vec![CountMean::named("cnt", &[], &calls)]);
    let mut store = stored();
    let now = T0 + 10 * MIN;
    let request = (key(MIN, "cnt"), ParamSet::new());
    let first = store
        .ensure(&registry, std::slice::from_ref(&request), now, Lookahead::Off)
        .expect("first ensure fills");
    assert_eq!(first, vec![key(MIN, "cnt")], "ordered series keys return");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one batch for one key");
    let rev = store.cache(&key(MIN, "cnt")).expect("cached").revision;
    // Same key twice in one call and again across calls: still one batch.
    store
        .ensure(&registry, &[request.clone(), request.clone()], now, Lookahead::Off)
        .expect("duplicate keys dedupe");
    store.ensure(&registry, &[request], now, Lookahead::Off).expect("second fill");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "cache serves every repeat");
    assert_eq!(store.cache(&key(MIN, "cnt")).expect("cached").revision, rev, "revision stable");
    // Hand-computed SMA-3 over the ten 1M closes (100.02 + 0.05*i):
    // mean[i] = 100.02 + 0.05*(i-1) for i >= 2.
    let values = &store.cache(&key(MIN, "cnt")).expect("cached").values;
    assert_eq!(values.len(), 10);
    assert!(values[0].is_none() && values[1].is_none(), "warming bars read na");
    for (at, value) in values.iter().enumerate().skip(2) {
        let want = 100.02 + 0.05 * (at as f64 - 1.0);
        let got = value.expect("warmed");
        assert!((got - want).abs() <= 1e-9 * want.abs().max(1.0), "bar {at}: got {got}, want {want}");
    }
}

#[test]
fn real_kernel_matches_direct_batch() {
    use crate::SmaIndicator;
    let registry = crate::indicator::builtin_registry().expect("builtins load");
    let mut store = MtfStore::new();
    store.set_base(&ten_bars()).expect("base loads");
    let params = ParamSet::new().with_int("length", 3);
    let sma = SeriesKey::new(MIN, "sma", &params).expect("sma key");
    let now = T0 + 10 * MIN;
    store
        .ensure(&registry, &[(sma.clone(), params.clone())], now, Lookahead::Off)
        .expect("sma fills");
    let cached = store.cache(&sma).expect("sma cached");
    assert_eq!(cached.warmup, SmaIndicator::new().warmup(&params).expect("warmup"));
    // Independent reproduction: direct batch over the store's own frame is
    // the same code path, so pin hand-computed values instead — SMA-3 over
    // closes 100.02+0.05*i, warming pair na.
    assert_eq!(cached.values.len(), 10);
    for (at, value) in cached.values.iter().enumerate() {
        if at < 2 {
            assert!(value.is_none(), "bar {at} warming");
        } else {
            let want = 100.02 + 0.05 * (at as f64 - 1.0);
            let got = value.expect("warmed");
            assert!((got - want).abs() <= 1e-9, "bar {at}: got {got}, want {want}");
        }
    }
    // The key separates parameterizations: length-4 is a different series.
    let sma4 = SeriesKey::new(MIN, "sma", &ParamSet::new().with_int("length", 4)).expect("key");
    assert_ne!(sma, sma4, "params hash separates series");
    assert_eq!(Timeframe::M1.duration_ms(), MIN, "single ms table for keys");
}

#[test]
fn topo_order_respects_depends_diamond_and_chain() {
    let calls = Arc::new(AtomicUsize::new(0));
    let leaf = CountMean::named("leaf", &[], &calls);
    let left = CountMean::named("left", &["leaf"], &calls);
    let right = CountMean::named("right", &["leaf"], &calls);
    let top = CountMean::named("top", &["left", "right"], &calls);
    let chain1 = CountMean::named("c1", &[], &calls);
    let chain2 = CountMean::named("c2", &["c1"], &calls);
    let chain3 = CountMean::named("c3", &["c2"], &calls);
    let registry = registry_with(vec![leaf, left, right, top, chain1, chain2, chain3]);
    let wanted =
        ["top", "c3"].iter().map(|kernel| key(MIN, kernel)).collect::<Vec<_>>();
    let graph = build_graph(&registry, &wanted).expect("diamond graphs");
    let order = order_for(&graph, &wanted).expect("diamond orders");
    let pos = |kernel: &str| order.iter().position(|k| k == &key(MIN, kernel)).expect("present");
    assert!(pos("leaf") < pos("left") && pos("leaf") < pos("right"), "diamond base first");
    assert!(pos("left") < pos("top") && pos("right") < pos("top"), "diamond apex last");
    assert!(pos("c1") < pos("c2") && pos("c2") < pos("c3"), "chain ordered");
    // Lifted default-param deps share keys with explicit requests.
    let both = [key(MIN, "leaf"), key(MIN, "top")];
    let graph = build_graph(&registry, &both).expect("shared dep graphs");
    let order = order_for(&graph, &both).expect("shared dep orders");
    assert_eq!(order.iter().filter(|k| *k == &key(MIN, "leaf")).count(), 1, "one leaf node");
    // The ordered keys actually evaluate through the store untouched.
    let mut store = stored();
    let requests = wanted
        .iter()
        .map(|k| (k.clone(), ParamSet::new()))
        .collect::<Vec<_>>();
    store
        .ensure(&registry, &requests, T0 + 10 * MIN, Lookahead::Off)
        .expect("diamond fills");
    assert_eq!(calls.load(Ordering::SeqCst), 7, "seven series, seven batches");
}

#[test]
fn stamp_invalidation_dirties_only_downstream() {
    let calls = Arc::new(AtomicUsize::new(0));
    let registry = registry_with(vec![CountMean::named("cnt", &[], &calls)]);
    let mut store = stored();
    let keys =
        [(key(MIN, "cnt"), ParamSet::new()), (key(5 * MIN, "cnt"), ParamSet::new())];
    let now = T0 + 10 * MIN;
    store.ensure(&registry, &keys, now, Lookahead::Off).expect("both fill");
    let rev_min = store.cache(&key(MIN, "cnt")).expect("1m cached").revision;
    let rev_5m = store.cache(&key(5 * MIN, "cnt")).expect("5m cached").revision;
    assert_eq!(store.frame_stamps(5 * MIN).expect("5m frame").len(), 2, "two confirmed buckets");
    // Appending one forming minute bar extends the 1M frame but leaves the
    // confirmed 5M prefix exactly as it was.
    store.append_bar(&bar_at(T0 + 10 * MIN, 10, 1100)).expect("append advances");
    store.ensure(&registry, &keys, T0 + 11 * MIN, Lookahead::Off).expect("refill");
    assert_eq!(calls.load(Ordering::SeqCst), 3, "only the 1M series re-runs");
    assert_ne!(store.cache(&key(MIN, "cnt")).expect("1m cached").revision, rev_min, "1M bumped");
    assert_eq!(store.cache(&key(5 * MIN, "cnt")).expect("5m cached").revision, rev_5m, "5M kept");
    assert_eq!(store.cache(&key(5 * MIN, "cnt")).expect("5m cached").values.len(), 2);
}

#[test]
fn backdate_identical_recompute_keeps_revision() {
    let calls = Arc::new(AtomicUsize::new(0));
    let registry = registry_with(vec![CountMean::named("cnt", &[], &calls)]);
    let mut store = stored();
    let request = (key(MIN, "cnt"), ParamSet::new());
    let now = T0 + 10 * MIN;
    store.ensure(&registry, std::slice::from_ref(&request), now, Lookahead::Off).expect("fills");
    let rev = store.cache(&key(MIN, "cnt")).expect("cached").revision;
    let store_rev = store.revision();
    // Same tape, same now: nothing re-runs at all.
    store.ensure(&registry, std::slice::from_ref(&request), now, Lookahead::Off).expect("refill");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "fresh keys skip batch");
    // Same tape re-set (base revision bumps, buckets identical): frames
    // rematerialize, compare equal, keep revisions — downstream not re-run.
    store.set_base(&ten_bars()).expect("same tape resets");
    store.ensure(&registry, &[request], now, Lookahead::Off).expect("post-reset fill");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "identical recompute backdates");
    assert_eq!(store.cache(&key(MIN, "cnt")).expect("cached").revision, rev, "series rev kept");
    assert_eq!(store.revision(), store_rev, "store rev kept: no change recorded");
}

#[test]
fn ready_and_dependents_delegation_sanity() {
    let calls = Arc::new(AtomicUsize::new(0));
    let registry = registry_with(vec![
        CountMean::named("leaf", &[], &calls),
        CountMean::named("top", &["leaf"], &calls),
    ]);
    let wanted = vec![key(MIN, "top")];
    let graph = build_graph(&registry, &wanted).expect("graphs");
    let frame = SeriesKey::new(MIN, FRAME_KERNEL, &ParamSet::new()).expect("frame key");
    let leaf = key(MIN, "leaf");
    let top = key(MIN, "top");
    // Nothing satisfied: only the resample edge source is ready.
    assert_eq!(graph.ready_nodes(&HashSet::new()), vec![frame.clone()], "frame first");
    let satisfied: HashSet<SeriesKey> = [frame.clone()].into_iter().collect();
    let mut ready = graph.ready_nodes(&satisfied);
    ready.sort();
    assert_eq!(ready, vec![frame.clone(), leaf.clone()], "leaf unblocks on frame");
    // Dependents fan out through both edges.
    let mut down = graph.dependents_of(&frame);
    down.sort();
    assert_eq!(down, vec![leaf.clone(), top.clone()], "frame feeds both series");
    assert_eq!(graph.dependents_of(&leaf), vec![top.clone()], "leaf feeds top");
    assert_eq!(graph.provides_of(&top), Some([top].as_slice()), "self-provision");
}

#[test]
fn random_request_sets_evaluate_in_valid_topo_order() {
    // Deterministic xorshift: the property is reproducible, never flaky.
    let mut rng = 0x9E3779B97F4A7C15u64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let calls = Arc::new(AtomicUsize::new(0));
    // Fixed dep lattice: b,c depend on a; d depends on b,c; e stands alone.
    let registry = registry_with(vec![
        CountMean::named("a", &[], &calls),
        CountMean::named("b", &["a"], &calls),
        CountMean::named("c", &["a"], &calls),
        CountMean::named("d", &["b", "c"], &calls),
        CountMean::named("e", &[], &calls),
    ]);
    let kernels = ["a", "b", "c", "d", "e"];
    let spans = [MIN, 3 * MIN, 5 * MIN];
    for round in 0..200 {
        let _ = round;
        let mut wanted = Vec::new();
        for kernel in kernels {
            if next() % 2 == 0 {
                let span = match next() % 3 {
                    0 => spans[0],
                    1 => spans[1],
                    _ => spans[2],
                };
                wanted.push(key(span, kernel));
            }
        }
        if wanted.is_empty() {
            wanted.push(key(MIN, "a"));
        }
        let graph = build_graph(&registry, &wanted).expect("random graphs");
        let order = order_for(&graph, &wanted).expect("random orders");
        // Every edge resolves backwards: each node's deps are provided by
        // nodes positioned earlier in the returned order.
        let mut provided: HashSet<SeriesKey> = HashSet::new();
        for node in &order {
            for dep in graph.deps_of(node).unwrap_or(&[]) {
                // The frame edge is provided by construction (pre-pass
                // materializes every frame before any batch runs).
                if dep.kernel != FRAME_KERNEL {
                    assert!(provided.contains(dep), "dep {dep} precedes {node}");
                }
            }
            provided.insert(node.clone());
        }
        // Frames precede their series: the frame node sorts before every
        // key on its span present in the order.
        for node in &order {
            if node.kernel == FRAME_KERNEL {
                continue;
            }
            let frame = SeriesKey::new(node.tf_ms, FRAME_KERNEL, &ParamSet::new()).expect("key");
            let frame_pos = order.iter().position(|k| k == &frame).expect("frame ordered");
            let node_pos = order.iter().position(|k| k == node).expect("node ordered");
            assert!(frame_pos < node_pos, "frame precedes {node}");
        }
    }
}

#[test]
fn fail_closed_guards() {
    let calls = Arc::new(AtomicUsize::new(0));
    let registry = registry_with(vec![CountMean::named("cnt", &["ghost"], &calls)]);
    let mut store = MtfStore::new();
    assert!(store.set_base(&[]).is_err(), "empty tape refuses");
    assert!(store.ensure(&registry, &[(key(MIN, "cnt"), ParamSet::new())], T0, Lookahead::Off).is_err(), "empty store refuses work");
    assert!(SeriesKey::new(0, "cnt", &ParamSet::new()).is_err(), "non-positive span refuses");
    assert!(SeriesKey::new(-MIN, "cnt", &ParamSet::new()).is_err(), "negative span refuses");
    store.set_base(&ten_bars()).expect("base loads");
    assert!(store.append_bar(&bar_at(T0, 0, 50)).is_err(), "non-advancing append refuses");
    let unknown = SeriesKey::new(MIN, "no_such_kernel", &ParamSet::new()).expect("key builds");
    assert!(
        store
            .ensure(&registry, &[(unknown, ParamSet::new())], T0 + 10 * MIN, Lookahead::Off)
            .is_err(),
        "unknown kernel refuses"
    );
    assert!(
        store
            .ensure(&registry, &[(key(MIN, "cnt"), ParamSet::new())], T0 + 10 * MIN, Lookahead::On)
            .is_err(),
        "lookahead-on refuses"
    );
    // Declared-but-unregistered deps refuse at graph build (fail-closed,
    // never an empty order downstream).
    assert!(build_graph(&registry, &[key(MIN, "cnt")]).is_err(), "ghost dep refuses");
    assert!(store.cache(&key(MIN, "cnt")).is_none(), "refused work caches nothing");
}
