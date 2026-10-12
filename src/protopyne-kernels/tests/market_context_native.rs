//! Native `MarketContext` parity: the same tapes through the shipped
//! `libApex` driver (oracle) and the native view over one shared store.
//!
//! Every float field must agree within `EPS` (na-ness included — the
//! warming and flat probes settle the oracle's na-semantics empirically,
//! and the port mirrors whatever the engine reports). `tf` is compared
//! separately: the engine reports chart-relative `C` while the native view
//! always resolves fixed-ms spans (locked decision) — a representation
//! difference, never a value difference.
//! `#[allow(clippy::float_cmp)]` covers the hand-computed literals.
#![allow(clippy::float_cmp)]

use super::*;
use crate::pine::test_fixture as fixture;
use crate::{InputSet, LinkedModule, PineEngine, PineRuntimeConfig};
use protopyne_types::Timeframe;
use protopyne_types::Bar;

/// Frozen agreement tolerance shared with the kernel-vs-Pine matrix.
const EPS: f64 = 1e-9;

const IMPORTS: &str = "import showmethegrail/libUtil/12 as UTIL\n\
     import showmethegrail/libBuffers/6 as libBuffers\n\
     import showmethegrail/libBars/12 as BARS\n\
     import showmethegrail/libDrawing/61 as libDrawing\n\
     import showmethegrail/libApex/88 as libApex\n";

const CTX_DRIVER: &str =
    "var ctx = libApex.MarketContext.new()\nif barstate.isfirst\n    ctx.init()\nctx.fetch()\n";

/// One closed bar from decimal legs and a whole-second timestamp.
fn bar(open: &str, high: &str, low: &str, close: &str, volume: u64, ts_secs: i64) -> Bar {
    Bar::new()
        .ts(
            chrono::TimeZone::timestamp_opt(&chrono::Utc, ts_secs, 0)
                .single()
                .expect("valid test timestamp"),
        )
        .o(open.parse().expect("decimal"))
        .h(high.parse().expect("decimal"))
        .l(low.parse().expect("decimal"))
        .c(close.parse().expect("decimal"))
        .volume(volume)
        .maybe_adj(None)
        .build()
}

fn linked(driver: &str) -> Vec<LinkedModule> {
    fixture::linked_modules(vec![
        fixture::shipped_module("libraries/_libUtil.pine", "showmethegrail", "libUtil", 12),
        fixture::shipped_module("libraries/_libBuffers.pine", "showmethegrail", "libBuffers", 6),
        fixture::shipped_module("libraries/_libBars.pine", "showmethegrail", "libBars", 12),
        fixture::shipped_module("libraries/_libDrawing.pine", "showmethegrail", "libDrawing", 61),
        fixture::shipped_module("libraries/_libApex.pine", "showmethegrail", "libApex", 88),
        fixture::driver_module(driver),
    ])
}

fn test_engine() -> PineEngine {
    PineEngine::new(
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period("5".to_owned())
            .build(),
    )
}

fn oracle_ctx(driver: &str, bars: &[Bar], at_bar: usize) -> crate::UserInstance {
    let modules = linked(driver);
    let mut engine = test_engine();
    let outcome = engine
        .run_linked(&modules, bars, InputSet::new())
        .expect("oracle evaluates");
    match outcome.value_at(at_bar, "ctx") {
        Some(Some(crate::PineValue::Object(instance))) => instance,
        other => panic!("expected MarketContext on bar {at_bar}, got {other:?}"),
    }
}

/// na-aware oracle field read: Pine `na` becomes `None`, numbers convert.
fn ofield(instance: &crate::UserInstance, name: &str) -> Option<f64> {
    match instance.fields.get(name) {
        Some(Some(crate::PineValue::Float(value))) => Some(*value),
        Some(Some(crate::PineValue::Int(value))) => Some(*value as f64),
        Some(None) => None,
        other => panic!("expected numeric-or-na field {name}, got {other:?}"),
    }
}

fn oregime(instance: &crate::UserInstance) -> String {
    match instance.fields.get("regime") {
        Some(Some(crate::PineValue::EnumHandle(handle))) => handle.member.clone(),
        other => panic!("expected regime enum, got {other:?}"),
    }
}

fn otf(instance: &crate::UserInstance) -> String {
    match instance.fields.get("tf") {
        Some(Some(crate::PineValue::EnumHandle(handle))) => handle.member.clone(),
        other => panic!("expected tf enum, got {other:?}"),
    }
}

/// Every float the native view computes, in Pine declaration order.
fn native_fields(ctx: &MarketContext) -> [(&'static str, Option<f64>); 15] {
    [
        ("ma_primary", ctx.ma_primary),
        ("ma_signal", ctx.ma_signal),
        ("ma_mean", ctx.ma_mean),
        ("atr", ctx.atr),
        ("atr_long", ctx.atr_long),
        ("cci", ctx.cci),
        ("rsi", ctx.rsi),
        ("stdev", ctx.stdev),
        ("vol", ctx.vol),
        ("vol_sma", ctx.vol_sma),
        ("atr_distance", ctx.atr_distance),
        ("ma_health", ctx.ma_health),
        ("volatility_ratio", ctx.volatility_ratio),
        ("vol_ratio", ctx.vol_ratio),
        ("trend_strength", ctx.trend_strength),
    ]
}

fn assert_ctx_parity(oracle: &crate::UserInstance, native: &MarketContext, what: &str) {
    for (name, got) in native_fields(native) {
        let want = ofield(oracle, name);
        match (want, got) {
            (None, None) => {}
            (Some(w), Some(g)) => {
                assert!(
                    (g - w).abs() <= EPS * w.abs().max(1.0),
                    "{what} field {name}: native {g} vs oracle {w}"
                );
            }
            _ => panic!("{what} field {name}: na-ness diverges (oracle {want:?}, native {got:?})"),
        }
    }
    assert_eq!(native.trend_direction, ofield(oracle, "trend_direction"), "{what} direction");
    assert_eq!(native.regime.to_string(), oregime(oracle), "{what} regime");
}

/// Sixty steep rising bars (slope 5, Pine `ramp_bars` shape): every kernel
/// warm on the last bar, averages separated past the range.
fn ramp_bars() -> Vec<Bar> {
    (0u32..60)
        .map(|i| {
            let close = 100.0 + 5.0 * f64::from(i);
            bar(
                &format!("{:.2}", close - 0.25),
                &format!("{:.2}", close + 0.5),
                &format!("{:.2}", close - 0.5),
                &format!("{close:.2}"),
                1_000 + u64::from(i) * 10,
                1_700_000_000 + i64::from(i) * 300,
            )
        })
        .collect()
}

fn close_now(bars: &[Bar], spacing_secs: i64) -> i64 {
    bars.last().expect("nonempty").ts.timestamp_millis() + spacing_secs * 1000
}

fn native_last(
    bars: &[Bar],
    tf: Timeframe,
    now_ms: i64,
) -> (MtfStore, MarketContext) {
    let registry = crate::indicator::builtin_registry().expect("builtins");
    let mut store = MtfStore::new();
    store.set_base(bars).expect("base loads");
    let key = MarketContextKey::new(tf);
    let contexts = fetch_all(&mut store, &registry, &[(key, None)], now_ms, Lookahead::Off)
        .expect("native fetch fills");
    (store, contexts.into_iter().next().expect("one context"))
}

#[test]
fn chart_branch_parity_at_eps() {
    let bars = ramp_bars();
    let oracle = oracle_ctx(&format!("{IMPORTS}{CTX_DRIVER}"), &bars, bars.len() - 1);
    let now = close_now(&bars, 300);
    let (_store, native) = native_last(&bars, Timeframe::M5, now);
    assert_ctx_parity(&oracle, &native, "ramp last bar");
    assert_eq!(otf(&oracle), "C", "oracle keeps chart-relative tf");
    assert_eq!(native.tf, Timeframe::M5, "native resolves the fixed-ms span");
}

#[test]
fn injected_legs_parity_at_eps() {
    // The auditable MTF path (Pine `fetch_numbers_reach_mtf_through_injected_legs`
    // shape): 15-minute bars injected as run legs.
    let bars: Vec<Bar> = (0u32..60)
        .map(|i| {
            let close = 500.0 + f64::from(i) * 5.0;
            bar(
                &format!("{:.2}", close - 0.5),
                &format!("{:.2}", close + 1.0),
                &format!("{:.2}", close - 1.0),
                &format!("{close:.2}"),
                5_000 + u64::from(i) * 20,
                1_700_000_000 + i64::from(i) * 900,
            )
        })
        .collect();
    let oracle = oracle_ctx(&format!("{IMPORTS}{CTX_DRIVER}"), &bars, bars.len() - 1);
    let now = close_now(&bars, 900);
    let (_store, native) = native_last(&bars, Timeframe::M15, now);
    assert_ctx_parity(&oracle, &native, "injected 15M last bar");
    assert_eq!(native.regime, MarketRegime::Volatile, "dispersion wins on injected legs too");
}

#[test]
fn warming_bars_propagate_na() {
    // Five bars: no kernel is warm, so the oracle's na-semantics (regime,
    // direction, normalization on absent inputs) become the port's contract.
    let bars: Vec<Bar> = (0u32..5)
        .map(|i| {
            let close = 100.0 + f64::from(i);
            bar(
                &format!("{:.2}", close - 0.2),
                &format!("{:.2}", close + 0.4),
                &format!("{:.2}", close - 0.3),
                &format!("{close:.2}"),
                1_000,
                1_700_000_000 + i64::from(i) * 300,
            )
        })
        .collect();
    let oracle = oracle_ctx(&format!("{IMPORTS}{CTX_DRIVER}"), &bars, bars.len() - 1);
    let now = close_now(&bars, 300);
    let (store, _) = native_last(&bars, Timeframe::M5, now);
    let key = MarketContextKey::new(Timeframe::M5);
    let at = store.frame_stamps(key.tf_ms()).expect("frame").len() - 1;
    let native = update(&store, &key, at, None).expect("warming update reads");
    assert_ctx_parity(&oracle, &native, "warming bar");
}

#[test]
fn flat_tape_oracle_probe() {
    // Thirty exactly-flat bars: ATR reads zero, so every atr-guarded
    // derivation runs the oracle's divide-by-zero path. Whatever the engine
    // reports becomes the port's contract — predicted quiet, verified here.
    let bars: Vec<Bar> = (0u32..30)
        .map(|i| bar("100", "100", "100", "100", 1_000, 1_700_000_000 + i64::from(i) * 300))
        .collect();
    let oracle = oracle_ctx(&format!("{IMPORTS}{CTX_DRIVER}"), &bars, bars.len() - 1);
    let now = close_now(&bars, 300);
    let (_store, native) = native_last(&bars, Timeframe::M5, now);
    assert_ctx_parity(&oracle, &native, "flat tape");
    assert!(
        native.regime == MarketRegime::LowVolume || native.regime == MarketRegime::Ranging,
        "flat tape stays quiet: {}",
        native.regime
    );
}

#[test]
fn health_mas_units_match_hand_computation() {
    // Oracle-pinned literals from `pine_libapex` health goldens.
    assert_eq!(health_mas(Some(2.0), Some(110.0), Some(105.0), Some(100.0), 2.0), Some(110.0));
    assert_eq!(health_mas(Some(2.0), Some(100.0), Some(105.0), Some(110.0), 2.0), Some(-150.0));
    assert_eq!(health_mas(Some(2.0), Some(105.0), Some(100.0), Some(102.0), 2.0), Some(5.625));
    assert_eq!(health_mas(Some(2.0), Some(100.0), Some(100.0), Some(100.0), 2.0), Some(0.0));
    // Absence mirrors Pine `na` structurally: unaligned stacks with
    // absent legs read exactly 0.0 (warming/flat oracle probes); a true
    // comparison against an absent distance poisons to na.
    assert_eq!(health_mas(Some(2.0), Some(100.0), Some(100.0), Some(100.0), 2.0), Some(0.0));
    assert_eq!(health_mas(None, None, None, None, 2.0), Some(0.0));
    // Aligned comparisons with absent distances refuse (unreachable with
    // the frozen bundle — alignment needs the slow average, which warms
    // long after ATR — so fail-closed na, never a guess).
    assert_eq!(health_mas(Some(0.0), Some(110.0), Some(105.0), Some(100.0), 2.0), None);
    assert_eq!(health_mas(None, Some(110.0), Some(105.0), Some(100.0), 2.0), None);
    // Partial absence blends: mid/slow present and truly ordered, fast
    // absent — the true leg's distance carries the blend to exactly 10.0
    // (-10 + 1.75/1.75*20 with slow_dist 2.5 weighted 0.7).
    assert_eq!(health_mas(Some(2.0), None, Some(105.0), Some(100.0), 2.0), Some(10.0));
}

#[test]
fn fetch_all_shares_one_store_across_spans() {
    // Thirty 1M bars feed two contexts (M1 + M5) from one store, one
    // revision counter, one ensure — the M8 fan-out shape in miniature.
    let bars: Vec<Bar> = (0u32..30)
        .map(|i| {
            let close = 100.0 + f64::from(i);
            bar(
                &format!("{:.2}", close - 0.2),
                &format!("{:.2}", close + 0.4),
                &format!("{:.2}", close - 0.3),
                &format!("{close:.2}"),
                1_000,
                1_700_000_000 + i64::from(i) * 60,
            )
        })
        .collect();
    let registry = crate::indicator::builtin_registry().expect("builtins");
    let mut store = MtfStore::new();
    store.set_base(&bars).expect("base loads");
    let now = close_now(&bars, 60);
    let keys = [MarketContextKey::new(Timeframe::M1), MarketContextKey::new(Timeframe::M5)];
    let requests = [(keys[0].clone(), None), (keys[1].clone(), None)];
    let contexts = fetch_all(&mut store, &registry, &requests, now, Lookahead::Off)
        .expect("multi-span fetch fills");
    assert_eq!(contexts.len(), 2, "one context per requested span");
    assert_eq!(contexts[0].tf, Timeframe::M1);
    assert_eq!(contexts[1].tf, Timeframe::M5);
    assert_eq!(store.frame_stamps(60_000).expect("1m frame").len(), 30);
    assert_eq!(store.frame_stamps(300_000).expect("5m frame").len(), 6);
    // The M1 context warms everything but the 50-bar ATR over thirty bars.
    assert!(contexts[0].ma_mean.is_some(), "mean warm on thirty 1M buckets");
    assert!(contexts[0].atr.is_some(), "atr warm");
    assert!(contexts[0].vol_sma.is_some(), "volume sma warm");
    assert_eq!(contexts[0].atr_long, None, "atr50 floor unmet at thirty bars");
    // The M5 context sees six buckets: long kernels stay absent and the
    // all-absent scores resolve to the fail-closed quiet label.
    assert_eq!(contexts[1].ma_mean, None, "ema21 needs twenty-one 5M buckets");
    assert_eq!(contexts[1].regime, MarketRegime::LowVolume, "short frame stays quiet");
}

#[test]
fn update_carry_and_fail_closed_probes() {
    let bars = ramp_bars();
    let registry = crate::indicator::builtin_registry().expect("builtins");
    let mut store = MtfStore::new();
    store.set_base(&bars).expect("base loads");
    let key = MarketContextKey::new(Timeframe::M5);
    let now = close_now(&bars, 300);
    // Unensured series refuse (caller must ensure first — never silent na).
    assert!(update(&store, &key, 59, None).is_err(), "unensured update refuses");
    fetch_all(&mut store, &registry, &[ (key.clone(), None)], now, Lookahead::Off)
        .expect("fetch fills");
    let at = store.frame_stamps(key.tf_ms()).expect("frame").len() - 1;
    assert!(update(&store, &key, at + 100, None).is_err(), "out-of-range bar refuses");
    // Carry mechanics: an explicit prior ratio survives a guard-failing
    // tape (zero-volume bars block the vol update; the prior holds).
    let zeros: Vec<Bar> = (0u32..30)
        .map(|i| {
            let close = 100.0 + f64::from(i % 2) * 0.4;
            bar(
                &format!("{:.2}", close - 0.2),
                &format!("{:.2}", close + 0.4),
                &format!("{:.2}", close - 0.3),
                &format!("{close:.2}"),
                0,
                1_700_000_000 + i64::from(i) * 300,
            )
        })
        .collect();
    let mut prior = update(&store, &key, at, None).expect("prior reads");
    prior.vol_ratio = Some(7.0);
    let mut zero_store = MtfStore::new();
    zero_store.set_base(&zeros).expect("zero tape loads");
    let zero_now = close_now(&zeros, 300);
    fetch_all(&mut zero_store, &registry, &[ (key.clone(), None)], zero_now, Lookahead::Off)
        .expect("zero fetch fills");
    let zero_at = zero_store.frame_stamps(key.tf_ms()).expect("frame").len() - 1;
    let carried = update(&zero_store, &key, zero_at, Some(&prior)).expect("carry reads");
    assert_eq!(carried.vol_ratio, Some(7.0), "guard-fail carries the prior ratio");
    assert_eq!(carried.vol, Some(0.0), "zero volume still reports (nz shape)");
    // Empty confirmed frame refuses: now inside the first bucket, before
    // any bucket closes (bucket-aligned math, not bar-aligned).
    let mut early = MtfStore::new();
    early.set_base(&bars).expect("base loads");
    let first_ts = bars[0].ts.timestamp_millis();
    let first_open = first_ts.div_euclid(300_000) * 300_000;
    assert!(
        fetch_all(&mut early, &registry, &[(key.clone(), None)], first_open + 300_000 - 1, Lookahead::Off)
            .is_err(),
        "no confirmed bucket refuses"
    );
}

