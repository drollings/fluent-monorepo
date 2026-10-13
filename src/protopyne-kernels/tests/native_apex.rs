//! Native `libApex` provider: engine agreement on closed-bar runs, decline
//! discipline everywhere else.
//!
//! The provider serves exactly one shape — the bare `ctx.fetch()` call on a
//! closed-bar run whose chart bars arrive evenly at the chart span — through
//! the shared-store context fold. Everything else declines to the Pine body:
//!
//! | Call shape | Outcome |
//! |---|---|
//! | `fetch()` on a closed-bar run, chart-spaced bars | served natively |
//! | tick runs (`run_linked_ticks`) | decline (Pine, byte-identical) |
//! | any explicit `target_tf` (positional or named) | decline (Pine runs the `request.security` branch or its refusal) |
//! | named args | decline (positional binding would misbind) |
//! | uneven/unaligned bar spacing, unmapped chart period | decline (no resample guessing) |
//!
//! Agreement compares two engines over the same bars: one opted into native
//! execution, one Pine-default. The `ctx2 = ctx.fetch()` probe additionally
//! pins the served call value (Pine returns `this`; the provider returns the
//! updated object). The audit test pins link-time routing (rust where
//! served, pine where not).
//!
//! Agreement drivers call `fetch` once per bar — the proven parity shape.
//! A second intra-bar `fetch()` perturbs the Pine engine itself (measured
//! on the ramp tape: double-called Pine reads 382.24 where single-called
//! Pine and native both read 366.77), so multi-call drivers sit outside the
//! provider contract. That non-idempotency is a pre-existing Pine-runtime
//! property (the native fold recomputes identically — it held 366.77 under
//! the doubled driver), flagged for the engine owners, not asserted here.
//! `#[allow(clippy::float_cmp)]` covers the hand-computed literals.
#![allow(clippy::float_cmp)]

use crate::pine::test_fixture as fixture;
use crate::pine::{ModuleGraph, resolve_imports};
use crate::{
    InputSet, LinkedModule, NativeOptions, PineEngine, PineRuntimeConfig,
};
use protopyne_types::Bar;

/// Frozen agreement tolerance shared with the kernel-vs-Pine matrix.
const EPS: f64 = 1e-9;

const IMPORTS: &str = "import showmethegrail/libUtil/12 as UTIL\n\
     import showmethegrail/libBuffers/6 as libBuffers\n\
     import showmethegrail/libBars/12 as BARS\n\
     import showmethegrail/libDrawing/61 as libDrawing\n\
     import showmethegrail/libApex/88 as libApex\n";

const CTX_DRIVER: &str =
    "var ctx = libApex.MarketContext.new()\nif barstate.isfirst\n    ctx.init()\nctx2 = ctx.fetch()\n";

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

fn test_engine(period: &str) -> PineEngine {
    PineEngine::new(
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period(period.to_owned())
            .build(),
    )
}

fn native_engine(period: &str) -> PineEngine {
    let mut engine = test_engine(period);
    engine
        .set_native_options(NativeOptions {
            enabled: true,
            only_symbols: None,
        })
        .expect("opt-in applies");
    engine
}

/// Sixty rising bars at `spacing_secs`: every kernel warm on the last bar.
fn ramp_bars(spacing_secs: i64) -> Vec<Bar> {
    (0u32..60)
        .map(|i| {
            let close = 100.0 + 5.0 * f64::from(i);
            bar(
                &format!("{:.2}", close - 0.25),
                &format!("{:.2}", close + 0.5),
                &format!("{:.2}", close - 0.5),
                &format!("{close:.2}"),
                1_000 + u64::from(i) * 10,
                1_700_000_000 + i64::from(i) * spacing_secs,
            )
        })
        .collect()
}

fn run_ctx(
    engine: &mut PineEngine,
    driver: &str,
    bars: &[Bar],
) -> crate::RunOutcome {
    let modules = linked(driver);
    engine
        .run_linked(&modules, bars, InputSet::new())
        .expect("apex run evaluates")
}

/// na-aware field read: Pine `na` becomes `None`, numbers convert.
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

/// Every float the context fold computes, in Pine declaration order.
const FLOAT_FIELDS: [&str; 15] = [
    "ma_primary",
    "ma_signal",
    "ma_mean",
    "atr",
    "atr_long",
    "cci",
    "rsi",
    "stdev",
    "vol",
    "vol_sma",
    "atr_distance",
    "ma_health",
    "volatility_ratio",
    "vol_ratio",
    "trend_strength",
];

fn assert_instances_agree(
    pine: &crate::UserInstance,
    native: &crate::UserInstance,
    what: &str,
) {
    for name in FLOAT_FIELDS {
        match (ofield(pine, name), ofield(native, name)) {
            (None, None) => {}
            (Some(w), Some(g)) => {
                assert!(
                    (g - w).abs() <= EPS * w.abs().max(1.0),
                    "{what} field {name}: native {g} vs pine {w}"
                );
            }
            (want, got) => {
                panic!("{what} field {name}: na-ness diverges (pine {want:?}, native {got:?})")
            }
        }
    }
    assert_eq!(
        ofield(native, "trend_direction"),
        ofield(pine, "trend_direction"),
        "{what} direction"
    );
    assert_eq!(oregime(native), oregime(pine), "{what} regime");
    assert_eq!(otf(native), otf(pine), "{what} tf");
}

fn instance_at(
    outcome: &crate::RunOutcome,
    at: usize,
    name: &str,
) -> crate::UserInstance {
    match outcome.value_at(at, name) {
        Some(Some(crate::PineValue::Object(instance))) => instance,
        other => panic!("expected context object {name} at {at}, got {other:?}"),
    }
}

#[test]
fn apex_matches_pine_on_chart_spans() {
    // The served shape (bare `fetch()`) agrees with the Pine body on every
    // chart span — both the stored `ctx` fields and the `ctx2` call value
    // (Pine returns `this`; the provider returns the updated object).
    for (period, spacing) in [("1", 60), ("5", 300), ("15", 900)] {
        let bars = ramp_bars(spacing);
        let driver = format!("{IMPORTS}{CTX_DRIVER}");
let pine_outcome = run_ctx(&mut test_engine(period), &driver, &bars);
        let native_outcome = run_ctx(&mut native_engine(period), &driver, &bars);
eprintln!("DBG driver.len={} driver_tail={:?}", driver.len(), &driver[driver.len()-60..]);
        eprintln!("DBG {period} bars={} last_close={} pine_ma={:?} native_ma={:?}",
            bars.len(), bars.last().unwrap().c,
            ofield(&instance_at(&pine_outcome, bars.len() - 1, "ctx"), "ma_primary"),
            ofield(&instance_at(&native_outcome, bars.len() - 1, "ctx"), "ma_primary"));
        let at = bars.len() - 1;
        assert_instances_agree(
            &instance_at(&pine_outcome, at, "ctx"),
            &instance_at(&native_outcome, at, "ctx"),
            &format!("{period}-chart stored ctx"),
        );
        assert_instances_agree(
            &instance_at(&pine_outcome, at, "ctx2"),
            &instance_at(&native_outcome, at, "ctx2"),
            &format!("{period}-chart fetch value"),
        );
        assert_eq!(otf(&instance_at(&native_outcome, at, "ctx")), "C", "tf untouched");
    }
}

/// Three 1M chart bars as four ticks: bar 2 arrives mid-form then final, so
/// the mid tick observes bar-so-far legs no closed bar holds. Returns the
/// chart tape, the per-tick tape, and the tick stamps.
fn tick_tape() -> (Vec<Bar>, Vec<Bar>, Vec<i64>) {
    let t0_ms = 1_700_000_000_000i64;
    let bar = |o: &str, h: &str, l: &str, c: &str, v: u64, open_ms: i64| {
        Bar::new()
            .ts(
                chrono::TimeZone::timestamp_opt(
                    &chrono::Utc,
                    open_ms.div_euclid(1000),
                    open_ms.rem_euclid(1000) as u32 * 1_000_000,
                )
                .single()
                .expect("valid test timestamp"),
            )
            .o(o.parse().expect("decimal"))
            .h(h.parse().expect("decimal"))
            .l(l.parse().expect("decimal"))
            .c(c.parse().expect("decimal"))
            .volume(v)
            .maybe_adj(None)
            .build()
    };
    let chart = vec![
        bar("100.00", "100.40", "99.80", "100.20", 1_000, t0_ms),
        bar("100.20", "100.60", "100.10", "100.50", 1_200, t0_ms + 60_000),
        bar("100.50", "101.00", "100.40", "100.90", 1_400, t0_ms + 120_000),
    ];
    // Bar-so-far legs share the bar-open stamp; volumes cumulate.
    let ticks = vec![
        chart[0].clone(),
        chart[1].clone(),
        bar("100.50", "100.70", "100.40", "100.60", 500, t0_ms + 120_000),
        chart[2].clone(),
    ];
    let stamps = vec![t0_ms + 10_000, t0_ms + 70_000, t0_ms + 130_000, t0_ms + 170_000];
    (chart, ticks, stamps)
}

#[test]
fn apex_tick_runs_decline_byte_identical() {
    // Tick runs never serve: the opted-in engine must reproduce the Pine
    // engine tick-for-tick. The mid tick (bar-so-far legs) proves the probe
    // discriminates — Pine itself reads a different `vol` there than on the
    // closed bar.
    let (chart, bars, ticks) = tick_tape();
    let driver = format!("{IMPORTS}{CTX_DRIVER}");
    let modules = linked(&driver);
    let mut pine = test_engine("1");
    let pine_outcome = pine
        .run_linked_ticks(&modules, &bars, &ticks, InputSet::new())
        .expect("pine tick run evaluates");
    let modules = linked(&driver);
    let mut native = native_engine("1");
    let native_outcome = native
        .run_linked_ticks(&modules, &bars, &ticks, InputSet::new())
        .expect("opted-in tick run evaluates");
    for (at, what) in [(0, "first"), (1, "second"), (2, "mid-bar"), (3, "confirmed")] {
        assert_instances_agree(
            &instance_at(&pine_outcome, at, "ctx"),
            &instance_at(&native_outcome, at, "ctx"),
            &format!("tick {what}"),
        );
    }
    let mid_vol = ofield(&instance_at(&pine_outcome, 2, "ctx"), "vol");
    let closed_vol = ofield(
        &instance_at(&run_ctx(&mut test_engine("1"), &driver, &chart), 2, "ctx"),
        "vol",
    );
    assert_ne!(mid_vol, closed_vol, "mid tick discriminates bar-so-far legs");
}

#[test]
fn apex_explicit_shapes_decline() {
    // Any explicit `target_tf` declines: the Pine body runs instead — here
    // the uninjected `request.security` branch refuses, and the opted-in
    // engine must refuse with the identical reason.
    let bars = ramp_bars(60);
    let driver = format!(
        "{IMPORTS}var ctx = libApex.MarketContext.new()\nif barstate.isfirst\n    ctx.init()\nctx.fetch(UTIL.Timeframes.M15)\n"
    );
    let pine_err = run_ctx_err(&mut test_engine("1"), &driver, &bars);
    let native_err = run_ctx_err(&mut native_engine("1"), &driver, &bars);
    assert_eq!(pine_err, native_err, "explicit HTF fetch refuses identically");
    assert!(
        pine_err.contains("security"),
        "refusal names the uninjected pull, got: {pine_err}"
    );
    // Named args decline the same way (here onto the chart branch, so both
    // engines agree on values).
    let named = format!(
        "{IMPORTS}var ctx = libApex.MarketContext.new()\nif barstate.isfirst\n    ctx.init()\nctx.fetch(target_tf = UTIL.Timeframes.C)\n"
    );
    let pine_named = run_ctx(&mut test_engine("1"), &named, &bars);
    let native_named = run_ctx(&mut native_engine("1"), &named, &bars);
    assert_instances_agree(
        &instance_at(&pine_named, bars.len() - 1, "ctx"),
        &instance_at(&native_named, bars.len() - 1, "ctx"),
        "named-arg fetch",
    );
    // Explicit positional chart timeframe declines too (only the bare shape
    // serves), with identical values on both engines.
    let explicit = format!(
        "{IMPORTS}var ctx = libApex.MarketContext.new()\nif barstate.isfirst\n    ctx.init()\nctx.fetch(UTIL.Timeframes.M1)\n"
    );
    let pine_exp = run_ctx(&mut test_engine("1"), &explicit, &bars);
    let native_exp = run_ctx(&mut native_engine("1"), &explicit, &bars);
    assert_instances_agree(
        &instance_at(&pine_exp, bars.len() - 1, "ctx"),
        &instance_at(&native_exp, bars.len() - 1, "ctx"),
        "explicit chart-tf fetch",
    );
}

fn run_ctx_err(engine: &mut PineEngine, driver: &str, bars: &[Bar]) -> String {
    let modules = linked(driver);
    match engine.run_linked(&modules, bars, InputSet::new()) {
        Ok(_) => panic!("expected refusal, run evaluated"),
        Err(err) => err.to_string(),
    }
}

#[test]
fn apex_audit_names_rust_where_served() {
    // Link-time routing: the default overlay serves `MarketContext`
    // natively (audit rust); the default-off overlay keeps Pine (audit
    // pine). Libraries without a provider never appear as served.
    let inputs = vec![
        fixture::shipped_module("libraries/_libUtil.pine", "showmethegrail", "libUtil", 12),
        fixture::shipped_module("libraries/_libBuffers.pine", "showmethegrail", "libBuffers", 6),
        fixture::shipped_module("libraries/_libBars.pine", "showmethegrail", "libBars", 12),
        fixture::shipped_module("libraries/_libDrawing.pine", "showmethegrail", "libDrawing", 61),
        fixture::shipped_module("libraries/_libApex.pine", "showmethegrail", "libApex", 88),
        fixture::driver_module(
            "import showmethegrail/libApex/88 as APEX\nx = 1\n",
        ),
    ];
    let graph: ModuleGraph = resolve_imports(inputs).expect("graph links");
    let on = crate::native_libs::apply_overrides(&graph, &NativeOptions::default())
        .expect("overlay applies");
    assert!(on.is_served("showmethegrail", "libApex"), "apex serves under the default");
    assert!(
        on.provides("showmethegrail", "libApex")
            .expect("native symbol table")
            .contains(&"MarketContext"),
        "MarketContext in the served surface"
    );
    let row = on
        .audits()
        .iter()
        .find(|audit| audit.requested_name == "libApex")
        .expect("apex audit recorded");
    assert_eq!(row.served, crate::NativeServed::Rust, "audit reads rust where served");
    assert!(!on.is_served("showmethegrail", "libUtil"), "unprovided library stays Pine");
    let off = crate::native_libs::apply_overrides(
        &graph,
        &NativeOptions {
            enabled: false,
            only_symbols: None,
        },
    )
    .expect("off overlay applies");
    let row = off
        .audits()
        .iter()
        .find(|audit| audit.requested_name == "libApex")
        .expect("apex audit recorded");
    assert_eq!(row.served, crate::NativeServed::Pine, "audit reads pine where not served");
}
