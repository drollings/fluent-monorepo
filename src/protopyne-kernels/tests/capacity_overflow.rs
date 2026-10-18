//! Capacity proof: `max_candles` (chart-over-LTF ratio plus padding) bounds
//! the ring, and the two implementations disagree past it by design.
//!
//! Sizing argument: one rebuild covers at most one chart bar — `ratio`
//! lower-timeframe rows plus one remainder slot and gap slop, all inside
//! `ratio + BUFFER_PADDING`. A regular session never needs more retained
//! simultaneously: `reconcile` clears the ring before loading, and the tick
//! path finalizes bucket by bucket, so the longest span any one rebuild
//! must hold is the chart bar itself. The canonical config table pins
//! must-fit (`ratio + 5` rows: everything retained, totals exact) and
//! must-overflow (`ratio + 6`: eviction fires exactly once). If the proof
//! ever needs a bigger floor, the constant changes with the test that
//! justifies it — today it does not.
//!
//! Divergence note: past capacity the shipped Pine `_processLTFArrays`
//! STOPS (`if not _hasBufferSpace() break` — keeps the OLDEST rows) while
//! the native ring EVICTS oldest (keeps the NEWEST). The Pine-stop arm is
//! pinned by executing the real `_processLTFArrays` on a 12-into-10
//! overfeed; the sizing proof above is why real input never reaches it.
//! Tick-path eviction agrees on both sides (Pine wraps its ring buffer;
//! native pops the front) and is pinned bucket-by-bucket.
//!
//! Span-ratio nuance (noted, not changed): native sizes with floored
//! division while Pine rounds the ratio up, so non-dividing spans differ
//! by at most one slot. Every canonical (and every covered) span divides
//! evenly, where the two rules coincide.
//!
//! Pine-side reads below are exact small-integer expectations (counts,
//! hand-built 1.0–12.0 legs), covered by the float-compare allow; measured
//! floats compare through the `assert_close` helper.
//! `#[allow(clippy::float_cmp)]` covers those literals.
#![allow(clippy::float_cmp)]

use super::*;

const MIN: i64 = 60_000;

fn rows(count: usize, ltf_ms: i64) -> Vec<LtfRow> {
    let base = 1_699_999_980_000;
    (0..count)
        .map(|i| {
            let price = 100.0 + i as f64;
            LtfRow {
                ts_ms: base + i as i64 * ltf_ms,
                o: price,
                h: price + 0.5,
                l: price - 0.5,
                c: price,
                volume: 10.0,
            }
        })
        .collect()
}

fn assert_close(got: f64, want: f64, what: &str) {
    assert!((got - want).abs() <= 1e-9, "{what}: got {got}, want {want}");
}

#[test]
fn sizing_table_fits_and_overflows() {
    // (ltf, ctf, ratio, max): every canonical span divides evenly.
    for (ltf_ms, ctf_ms, ratio) in [
        (MIN, 5 * MIN, 5),
        (MIN, 15 * MIN, 15),
        (MIN, 60 * MIN, 60),
        (5 * MIN, 15 * MIN, 3),
        (5 * MIN, 60 * MIN, 12),
        (15 * MIN, 60 * MIN, 4),
    ] {
        let config = BarsConfig::new(ltf_ms, ltf_ms, ctf_ms, 86_400_000);
        let max = ratio + BUFFER_PADDING;
        assert_eq!(
            config.max_candles(),
            max,
            "sizing rule for {ltf_ms}/{ctf_ms}"
        );
        // Must-fit: ratio + 5 rows all retained, totals exact.
        let fit = rows(max, ltf_ms);
        let mut ring = BarRing::new(&config).expect("valid config");
        let reference: f64 = fit.iter().map(|row| row.volume).sum();
        let outcome = ring.reconcile(&fit, reference, 100.0);
        assert_eq!(
            ring.len(),
            max,
            "must-fit retains everything for {ltf_ms}/{ctf_ms}"
        );
        assert!(
            outcome.remainder.is_none(),
            "exact reference tops up nothing"
        );
        assert_close(ring.total_volume(), reference, "must-fit totals exact");
        // Must-overflow: one row past capacity evicts exactly once.
        let over = rows(max + 1, ltf_ms);
        let mut ring = BarRing::new(&config).expect("valid config");
        let reference: f64 = over.iter().map(|row| row.volume).sum();
        ring.reconcile(&over, reference, 100.0);
        assert_eq!(
            ring.len(),
            max,
            "must-overflow fires eviction for {ltf_ms}/{ctf_ms}"
        );
        assert_eq!(
            ring.get(0).expect("oldest readable").ts_ms,
            over[1].ts_ms,
            "eviction drops the oldest"
        );
    }
}

#[test]
fn overflow_keeps_newest() {
    let config = BarsConfig::new(MIN, MIN, 5 * MIN, 86_400_000);
    assert_eq!(config.max_candles(), 10, "10-slot ring for the pin");
    let over = rows(12, MIN);
    let mut ring = BarRing::new(&config).expect("valid config");
    ring.reconcile(&over, 120.0, 100.0);
    assert_eq!(ring.len(), 10, "overflow caps at capacity");
    assert_eq!(
        ring.get(0).expect("readable").ts_ms,
        over[2].ts_ms,
        "oldest two dropped"
    );
    assert_close(ring.get(9).expect("readable").c, 111.0, "newest row kept");
    assert_close(ring.total_volume(), 100.0, "totals cover the kept rows");
}

#[test]
fn tick_path_evicts_oldest() {
    let config = BarsConfig::new(MIN, MIN, 5 * MIN, 86_400_000);
    let mut ring = BarRing::new(&config).expect("valid config");
    let base = 1_699_999_980_000;
    ring.on_bar_open(100.0, 100.0, 100.0, 100.0, base);
    let mut cum = 0.0;
    for i in 0..12 {
        let price = 100.0 + i as f64;
        cum += 100.0;
        let stamp = base + i * MIN;
        ring.ingest_tick(
            crate::indicator::BarTick {
                open: price,
                high: price,
                low: price,
                close: price,
                volume: cum,
            },
            stamp,
        );
        ring.confirm_ltf(stamp, price, None);
    }
    assert_eq!(ring.len(), 10, "tick path caps at capacity");
    assert_eq!(
        ring.get(0).expect("readable").ts_ms,
        base + MIN,
        "tick eviction drops the oldest bucket"
    );
    assert_close(
        ring.total_volume(),
        1_000.0,
        "tick totals cover the kept buckets",
    );
}

/// Past-capacity split, executed on both sides: Pine stops (keeps the
/// oldest ten), native evicts (keeps the newest ten). Pine needs the
/// language runtime; the lean pins above cover the native arm alone.
#[cfg(feature = "pine")]
#[test]
fn pine_stop_keeps_oldest() {
    use crate::pine::test_fixture as fixture;
    use crate::{
        InputSet, LinkedModule, PineEngine, PineRuntimeConfig, PineValue, RunOutcome, UserInstance,
    };
    use chrono::TimeZone as _;
    use protopyne_types::Bar;

    const IMPORTS: &str =
        "import showmethegrail/libUtil/12 as UTIL\nimport showmethegrail/libBars/12 as BARS\n";
    const PROLOGUE: &str = "var bars = BARS.Bars.new()\nif barstate.isfirst\n    bars.init(UTIL.Timeframes.M1, UTIL.Timeframes.M1, UTIL.Timeframes.D1)\n";

    fn array_of(values: &[f64]) -> String {
        values
            .iter()
            .map(|v| format!("{v:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
    let vals: Vec<f64> = (1..=12).map(f64::from).collect();
    let vols = vec![10.0; 12];
    let driver = format!(
        "{IMPORTS}{PROLOGUE}\
        times = array.from(0, 60000, 120000, 180000, 240000, 300000, 360000, 420000, 480000, 540000, 600000, 660000)\n\
        opens = array.from({opens})\n\
        highs = array.from({opens})\n\
        lows = array.from({opens})\n\
        closes = array.from({opens})\n\
        vols = array.from({vols})\n\
        bars._processLTFArrays(times, opens, highs, lows, closes, vols)\n\
        [o0, h0, _, _, _, _] = bars.get(0)\n\
        [o9, h9, _, _, _, _] = bars.get(9)\n",
        opens = array_of(&vals),
        vols = array_of(&vols),
    );
    let modules: Vec<LinkedModule> = fixture::linked_modules(vec![
        fixture::shipped_module("libraries/_libUtil.pine", "showmethegrail", "libUtil", 12),
        fixture::shipped_module("libraries/_libBars.pine", "showmethegrail", "libBars", 12),
        fixture::driver_module(&driver),
    ]);
    let bar = Bar::new()
        .ts(chrono::Utc
            .timestamp_opt(1_699_999_980, 0)
            .single()
            .expect("valid stamp"))
        .o("100.00".parse().expect("decimal"))
        .h("101.00".parse().expect("decimal"))
        .l("99.00".parse().expect("decimal"))
        .c("100.00".parse().expect("decimal"))
        .volume(1_000_u64)
        .maybe_adj(None)
        .build();
    let mut engine = PineEngine::new(
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period("5".to_owned())
            .build(),
    );
    let outcome: RunOutcome = engine
        .run_linked(&modules, std::slice::from_ref(&bar), InputSet::new())
        .expect("linked run evaluates");
    let state: UserInstance = match outcome.value_at(0, "bars") {
        Some(Some(PineValue::Object(instance))) => instance,
        other => panic!("expected Bars object, got {other:?}"),
    };
    let field_num = |name: &str| match state.fields.get(name) {
        Some(Some(PineValue::Float(value))) => *value,
        Some(Some(PineValue::Int(value))) => *value as f64,
        other => panic!("expected number field {name}, got {other:?}"),
    };
    let num = |name: &str| match outcome.value_at(0, name) {
        Some(Some(PineValue::Float(value))) => value,
        Some(Some(PineValue::Int(value))) => value as f64,
        other => panic!("expected number for {name}, got {other:?}"),
    };
    // Pine stops at capacity: the oldest ten rows stay.
    assert_eq!(field_num("count"), 10.0, "pine stops at ten slots");
    assert_eq!(num("h0"), 1.0, "pine keeps the first row");
    assert_eq!(num("h9"), 10.0, "pine drops past the tenth row");
    // Native evicts: the newest ten rows stay.
    let over = rows(12, MIN);
    let mut ring = BarRing::new(&BarsConfig::new(MIN, MIN, 5 * MIN, 86_400_000)).expect("valid");
    ring.reconcile(&over, 120.0, 100.0);
    assert_eq!(ring.len(), 10, "native caps at ten slots");
    assert_eq!(
        ring.get(0).expect("readable").ts_ms,
        over[2].ts_ms,
        "native drops the oldest"
    );
    // The split, pinned both ways.
    assert_eq!(num("h0"), 1.0, "pine first row is the overfeed head");
    assert_close(
        ring.get(0).expect("readable").c,
        102.0,
        "native first row is third fed",
    );
}
