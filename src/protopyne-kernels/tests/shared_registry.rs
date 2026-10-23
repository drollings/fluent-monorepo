//! One immutable kernel registry per process.
//!
//! The builtin catalog is a *pure value*: 56 stateless kernels keyed by id.
//! Caching it is therefore not a violation of the instance-ownership rule —
//! per-run state lives in [`IndicatorState`](crate::indicator::IndicatorState),
//! never in the registry — so one shared handle is enough for every reader.
//!
//! These tests pin the three claims that cache makes: it is built at most once,
//! every reader sees the same address, and concurrent reads produce identical
//! numbers.

use super::*;

/// A window with enough bars for the smoothing defaults to warm up.
fn closes() -> Vec<f64> {
    (0..64)
        .map(|index| {
            let step = f64::from(index);
            100.0 + (step * 0.31).sin() * 3.0 + step * 0.05
        })
        .collect()
}

fn frame() -> OwnedOhlcv {
    let close = closes();
    OwnedOhlcv {
        open: close.iter().map(|price| price - 0.25).collect(),
        high: close.iter().map(|price| price + 1.0).collect(),
        low: close.iter().map(|price| price - 1.0).collect(),
        close,
        volume: vec![1_000.0; 64],
    }
}

#[test]
fn shared_registry_is_one_build_for_the_process() {
    let first = shared_registry().expect("shared registry builds");
    let second = shared_registry().expect("shared registry builds");
    assert!(
        std::ptr::eq(first, second),
        "the shared registry must be one instance per process"
    );
    assert_eq!(first.ids(), second.ids());
    assert_eq!(first.len(), second.len());
    assert!(!first.is_empty(), "the shared registry holds the builtins");
}

/// The migrated hot paths must *read* the shared handle. Before the migration
/// every `SmoothingFamily::evaluate` built a whole registry (56 `Arc`s plus 56
/// id `String`s), so a regression that reintroduces the owned constructor
/// shows up here as a second build instead of as an unread benchmark.
#[test]
fn the_smoothing_hot_path_never_rebuilds_the_registry() {
    let _ = shared_registry().expect("shared registry builds");
    let before = shared_registry_builds();
    let before_owned = owned_registry_builds();
    let frame = frame();
    let args = crate::smoothing::SmoothingArgs::length(14);
    for _ in 0..8 {
        for family in [
            crate::smoothing::SmoothingFamily::Ema,
            crate::smoothing::SmoothingFamily::Rma,
            crate::smoothing::SmoothingFamily::Wma,
            crate::smoothing::SmoothingFamily::Zlema,
        ] {
            let out = family.evaluate(&frame, &args).expect("smoothing evaluates");
            assert_eq!(out.len(), frame.len());
        }
    }
    assert_eq!(
        owned_registry_builds(),
        before_owned,
        "a hot path built its own registry instead of reading the shared one"
    );
    assert_eq!(
        shared_registry_builds(),
        before,
        "a hot path reinstalled the shared registry"
    );
    assert!(
        shared_registry_builds() <= 1,
        "the shared registry is built at most once"
    );
}

/// The component handle gets the same treatment: `from_builtins` used to
/// rebuild 56 registrations per call wherever a component registry was needed.
#[test]
fn shared_component_registry_is_one_build_for_the_process() {
    let first = crate::component::ComponentRegistry::shared().expect("shared components build");
    let second = crate::component::ComponentRegistry::shared().expect("shared components build");
    assert!(
        std::ptr::eq(first, second),
        "the shared component registry must be one instance per process"
    );
    assert_eq!(first.ids(), second.ids());
    assert_eq!(first.len(), second.len());
    assert!(!first.is_empty(), "the shared registry holds the builtins");
}

fn read_sma(data: &[f64], params: &ParamSet) -> (usize, Vec<Option<u64>>) {
    let registry = shared_registry().expect("shared registry builds");
    let sma = registry.get("sma").expect("sma is registered");
    let values = sma
        .batch(&Ohlcv::close_only(data), params)
        .expect("batch runs")
        .values;
    (std::ptr::from_ref(registry) as usize, bits_of(&values))
}

#[test]
fn concurrent_readers_share_one_registry_and_agree() {
    let data = closes();
    let params = ParamSet::new().with_int("length", 14);
    let (a, b) = std::thread::scope(|scope| {
        let first = scope.spawn(|| read_sma(&data, &params));
        let second = scope.spawn(|| read_sma(&data, &params));
        (
            first.join().expect("first thread joins"),
            second.join().expect("second thread joins"),
        )
    });
    assert_eq!(a.0, b.0, "both threads read the same registry address");
    assert_eq!(a.1, b.1, "concurrent reads produce identical numbers");
    assert_eq!(a.1.len(), data.len());
}
