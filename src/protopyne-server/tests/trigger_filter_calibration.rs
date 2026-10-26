//! Trigger and filter calibration: controls plus frozen treatment tallies.
//!
//! The slot algebra (triggers, filters) is untrusted until its controls stay
//! quiet and its treatment tallies are frozen. Each control below is chosen
//! so that a naive component would fire or admit on it: a flat tape, a bar
//! with no previous value, a value exactly on its level (crossings are
//! strict on both the native and the reference path), and a tape walked for
//! a bar that arms both sides at once (impossible for one directed crossing,
//! so any such bar is a defect). The calibrated components must stay quiet
//! on all of them.
//!
//! The treatment freezes, per baseline configuration, the admitted-bar index
//! set with a score fingerprint plus every installed trigger's armed-bar
//! index sets over the same deterministic tape the pre-refactor baseline
//! used. The recompute compares number-for-number; precision and recall over
//! the admitted sets are 1.0 by exact index-set equality, and every control
//! admits or fires nothing.

use std::collections::BTreeSet;
use std::path::PathBuf;

use chrono::TimeZone as _;
use protopyne_strategy::elements::{Signal, SignalConfig};
use protopyne_strategy::integration_lite::IntegrationLiteStrategyConfig;
use protopyne_strategy::signals_lite::{
    build_triggers, IntegrationLiteSignal, LiteIndicatorParams,
};
use protopyne_strategy::slots::filter::{Admission, Filter};
use protopyne_strategy::slots::trigger::Trigger;
use protopyne_strategy::tape::Tape;
use protopyne_strategy::{Direction, MarketData, MarketView};
use protopyne_types::{Bar, Timeframe};
use rust_decimal::Decimal;

fn historical(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/protopyne-rs/historical")
        .join(name)
}

fn load(path: &std::path::Path) -> Vec<Bar> {
    crate::history::load_bars_file(path).expect("fixture loads")
}

fn rounded(value: f64) -> f64 {
    (value * 1e12).round() / 1e12
}

fn verdicts(
    bars: &[Bar],
    tweak: impl Fn(&mut IntegrationLiteStrategyConfig),
) -> Vec<serde_json::Value> {
    let mut config = IntegrationLiteStrategyConfig {
        warmup_bars: 30,
        ..IntegrationLiteStrategyConfig::default()
    };
    tweak(&mut config);
    let signal = IntegrationLiteSignal::new(LiteIndicatorParams::default(), config)
        .expect("the default dials validate");
    let timeframe = Timeframe::infer(bars).unwrap_or(Timeframe::D1);
    let mut instance = signal
        .bind(&SignalConfig::new(timeframe))
        .expect("the signal binds");
    let closes: Vec<f64> = bars
        .iter()
        .map(|bar| {
            use rust_decimal::prelude::ToPrimitive as _;
            bar.c.to_f64().unwrap_or(f64::NAN)
        })
        .collect();
    let mut out = Vec::with_capacity(bars.len());
    for index in 0..bars.len() {
        let window = &bars[..=index];
        let data = MarketData {
            bars: window,
            chain: None,
            dte_days: 0,
            timeframe,
        };
        let view = MarketView {
            data: &data,
            closes: &closes[..=index],
            as_of: bars[index].ts,
        };
        out.push(match instance.evaluate(&view) {
            Ok(output) => serde_json::json!([
                match output.direction {
                    Direction::Long => "long",
                    Direction::Short => "short",
                },
                rounded(output.conviction),
                rounded(output.activity_score),
            ]),
            Err(_) => serde_json::Value::Null,
        });
    }
    out
}

type Tweak = fn(&mut IntegrationLiteStrategyConfig);

fn configurations() -> Vec<(&'static str, Tweak)> {
    fn none(_: &mut IntegrationLiteStrategyConfig) {}
    fn exhaust(c: &mut IntegrationLiteStrategyConfig) {
        c.sig_momentum = false;
        c.sig_exhaust_entry = true;
    }
    fn ribbon_flip(c: &mut IntegrationLiteStrategyConfig) {
        c.sig_momentum = false;
        c.sig_ribbon_flip = true;
    }
    fn osc_cross(c: &mut IntegrationLiteStrategyConfig) {
        c.sig_momentum = false;
        c.sig_osc_cross = true;
    }
    fn dual(c: &mut IntegrationLiteStrategyConfig) {
        c.sig_momentum = false;
        c.sig_dual = true;
    }
    fn all_triggers(c: &mut IntegrationLiteStrategyConfig) {
        c.sig_exhaust_entry = true;
        c.sig_ribbon_flip = true;
        c.sig_osc_cross = true;
        c.sig_dual = true;
    }
    fn no_filters(c: &mut IntegrationLiteStrategyConfig) {
        c.filt_ribbon = false;
        c.filt_ma_health = false;
    }
    fn ribbon_health(c: &mut IntegrationLiteStrategyConfig) {
        c.filt_ribbon = false;
        c.filt_ma_health = false;
        c.filt_ribbon_health = true;
    }
    fn slow(c: &mut IntegrationLiteStrategyConfig) {
        c.filt_ribbon = false;
        c.filt_ma_health = false;
        c.filt_slow = true;
    }
    fn delta(c: &mut IntegrationLiteStrategyConfig) {
        c.filt_ribbon = false;
        c.filt_ma_health = false;
        c.filt_delta = true;
    }
    fn volume(c: &mut IntegrationLiteStrategyConfig) {
        c.filt_ribbon = false;
        c.filt_ma_health = false;
        c.filt_volume = true;
    }
    fn extension(c: &mut IntegrationLiteStrategyConfig) {
        c.filt_ribbon = false;
        c.filt_ma_health = false;
        c.filt_extension = true;
    }
    fn regime_lowvol(c: &mut IntegrationLiteStrategyConfig) {
        c.filt_ribbon = false;
        c.filt_ma_health = false;
        c.filt_regime_lowvol = true;
    }
    fn regime_volatile(c: &mut IntegrationLiteStrategyConfig) {
        c.filt_ribbon = false;
        c.filt_ma_health = false;
        c.filt_regime_volatile = true;
    }
    fn all_filters(c: &mut IntegrationLiteStrategyConfig) {
        c.filt_ribbon_health = true;
        c.filt_slow = true;
        c.filt_delta = true;
        c.filt_volume = true;
        c.filt_extension = true;
        c.filt_regime_lowvol = true;
        c.filt_regime_volatile = true;
    }
    vec![
        ("default", none as Tweak),
        ("exhaustion_only", exhaust),
        ("ribbon_flip_only", ribbon_flip),
        ("osc_cross_only", osc_cross),
        ("dual_only", dual),
        ("all_triggers", all_triggers),
        ("no_filters", no_filters),
        ("ribbon_health_only", ribbon_health),
        ("slow_side_only", slow),
        ("delta_only", delta),
        ("volume_only", volume),
        ("extension_only", extension),
        ("skip_low_volume", regime_lowvol),
        ("skip_volatile", regime_volatile),
        ("all_filters", all_filters),
    ]
}

fn synthetic_tape(len: usize) -> Vec<Bar> {
    let mut state: u64 = 0x5EED_1234_ABCD_0001;
    let mut jitter = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((state >> 33) as f64) / f64::from(1u32 << 31)
    };
    let start = chrono::Utc
        .with_ymd_and_hms(2026, 1, 5, 14, 30, 0)
        .single()
        .expect("valid instant");
    let mut bars = Vec::with_capacity(len);
    let mut previous = 500.0_f64;
    for index in 0..len {
        let phase = index as f64;
        let close = 500.0
            + 40.0 * (phase / 17.0).sin()
            + 15.0 * (phase / 5.3).sin()
            + 4.0 * (jitter() - 0.5);
        let open = previous;
        let high = open.max(close) + 1.5;
        let low = open.min(close) - 1.5;
        let volume = 10_000.0 + 3_000.0 * (phase / 7.0).sin() + 500.0 * jitter();
        let value = |amount: f64| Decimal::from_f64_retain(amount).expect("finite price");
        bars.push(
            Bar::new()
                .ts(start + chrono::Duration::minutes(index as i64))
                .o(value(open))
                .h(value(high))
                .l(value(low))
                .c(value(close))
                .volume(volume as u64)
                .maybe_adj(None)
                .build(),
        );
        previous = close;
    }
    bars
}

fn flat_tape(len: usize) -> Vec<Bar> {
    let start = chrono::Utc
        .with_ymd_and_hms(2026, 1, 5, 14, 30, 0)
        .single()
        .expect("valid instant");
    (0..len)
        .map(|index| {
            Bar::new()
                .ts(start + chrono::Duration::minutes(index as i64))
                .o(Decimal::from(500))
                .h(Decimal::from(500))
                .l(Decimal::from(500))
                .c(Decimal::from(500))
                .volume(10_000)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

fn batch_of(bars: &[Bar]) -> protopyne_kernels::lite::LiteLegs {
    protopyne_kernels::lite::lite_legs(bars, &LiteIndicatorParams::default(), 14)
        .expect("legs batch")
}

fn closes_of(bars: &[Bar]) -> Vec<f64> {
    use rust_decimal::prelude::ToPrimitive as _;
    bars.iter()
        .map(|bar| bar.c.to_f64().unwrap_or(f64::NAN))
        .collect()
}

fn all_triggers_config() -> IntegrationLiteStrategyConfig {
    let mut config = IntegrationLiteStrategyConfig::default();
    all_triggers_tweak(&mut config);
    config
}

fn all_triggers_tweak(config: &mut IntegrationLiteStrategyConfig) {
    config.sig_exhaust_entry = true;
    config.sig_ribbon_flip = true;
    config.sig_osc_cross = true;
    config.sig_dual = true;
}

/// One trigger's armed bars over the tape, long and short separately.
fn armed_bars(
    trigger: &std::sync::Arc<dyn Trigger>,
    bars: &[Bar],
    legs: &protopyne_kernels::lite::LiteLegs,
    closes: &[f64],
) -> (Vec<usize>, Vec<usize>, Vec<usize>) {
    let mut long = Vec::new();
    let mut short = Vec::new();
    let mut both = Vec::new();
    for index in 0..bars.len() {
        let tape = Tape::new(bars, legs, closes, index).expect("tape binds");
        let verdict = trigger.eval(&tape);
        if verdict.long && verdict.short {
            both.push(index);
        }
        if verdict.long {
            long.push(index);
        }
        if verdict.short {
            short.push(index);
        }
    }
    (long, short, both)
}

// ── Trigger controls: must not fire ─────────────────────────────────────────

#[test]
fn a_flat_tape_arms_no_trigger() {
    let bars = flat_tape(120);
    let legs = batch_of(&bars);
    let closes = closes_of(&bars);
    let triggers = build_triggers(&all_triggers_config());
    assert!(
        !triggers.is_empty(),
        "the control needs every trigger armed"
    );
    for trigger in &triggers {
        let (long, short, _) = armed_bars(trigger, &bars, &legs, &closes);
        assert!(long.is_empty(), "flat tape arms no long side");
        assert!(short.is_empty(), "flat tape arms no short side");
    }
}

#[test]
fn a_bar_with_no_previous_value_arms_nothing() {
    // Index zero has no previous bar, and a one-bar batch has no warmed leg:
    // every crossing needs a previous value, so nothing may fire.
    let bars = synthetic_tape(60);
    let legs = batch_of(&bars);
    let closes = closes_of(&bars);
    let triggers = build_triggers(&all_triggers_config());
    for trigger in &triggers {
        let tape = Tape::new(&bars, &legs, &closes, 0).expect("tape binds");
        let verdict = trigger.eval(&tape);
        assert!(
            !verdict.long && !verdict.short,
            "index zero must stay idle (no previous value)"
        );
    }
    let one = flat_tape(1);
    let one_legs = batch_of(&one);
    let one_closes = closes_of(&one);
    for trigger in &triggers {
        let tape = Tape::new(&one, &one_legs, &one_closes, 0).expect("tape binds");
        let verdict = trigger.eval(&tape);
        assert!(
            !verdict.long && !verdict.short,
            "an unwarmable single bar must stay idle"
        );
    }
}

fn constant_legs(
    level: f64,
    len: usize,
) -> (Vec<Bar>, protopyne_kernels::lite::LiteLegs, Vec<f64>) {
    let bars = flat_tape(len);
    let closes = closes_of(&bars);
    let series = vec![Some(level); len];
    let legs = protopyne_kernels::lite::LiteLegs {
        fast: series.clone(),
        slow: series.clone(),
        fast_raw: series.clone(),
        slow_raw: series.clone(),
        adaptive: series.clone(),
        signal: series.clone(),
        mean: series.clone(),
        ribbon_health: series.clone(),
        ma_health: series.clone(),
        atr: series.clone(),
        atr_risk: series.clone(),
        atr_long: series.clone(),
        stdev: series.clone(),
        vol_ratio: vec![1.0; len],
        vol_sma: series.clone(),
        close: closes.clone(),
    };
    (bars, legs, closes)
}

#[test]
fn a_value_exactly_on_its_level_never_crosses() {
    // Strict crossings never fire on equality: `prev == curr == level` is
    // not a cross, and a series pinned exactly on the level crosses neither
    // up nor down on any bar.
    for level in [50.0, 200.0, -50.0] {
        let (bars, legs, closes) = constant_legs(level, 4);
        let triggers = build_triggers(&all_triggers_config());
        for trigger in &triggers {
            let (long, short, _) = armed_bars(trigger, &bars, &legs, &closes);
            assert!(
                long.is_empty() && short.is_empty(),
                "a series pinned at {level} must cross nothing"
            );
        }
    }
}

#[test]
fn nan_legs_arm_nothing() {
    let (bars, mut legs, closes) = constant_legs(50.0, 4);
    legs.fast = vec![Some(f64::NAN); 4];
    let triggers = build_triggers(&all_triggers_config());
    for trigger in &triggers {
        let (long, short, _) = armed_bars(trigger, &bars, &legs, &closes);
        assert!(
            long.is_empty() && short.is_empty(),
            "an unmeasurable leg must arm nothing"
        );
    }
}

#[test]
fn no_bar_arms_both_sides_at_once() {
    // One directed crossing per bar: a single series cannot cross its level
    // upward and downward on the same bar, so any both-sides verdict is a
    // defect in the component, not a strong signal.
    let bars = synthetic_tape(400);
    let legs = batch_of(&bars);
    let closes = closes_of(&bars);
    let triggers = build_triggers(&all_triggers_config());
    for trigger in &triggers {
        let (_, _, both) = armed_bars(trigger, &bars, &legs, &closes);
        assert!(
            both.is_empty(),
            "no bar may arm both sides at once: {both:?}"
        );
    }
}

// ── Filter controls ─────────────────────────────────────────────────────────
// Admission is task-value text for the skip line: a blocked thesis stays out.
// Three boundaries below admit on equality exactly as the reference does
// (`vol_ratio >= min`, `atr_distance <= max`, block-only-if-skipped); they
// are pinned as admits, with the reference line named beside each.

fn legs_for_bars(bars: &[Bar]) -> (protopyne_kernels::lite::LiteLegs, Vec<f64>) {
    let legs = batch_of(bars);
    let closes = closes_of(bars);
    (legs, closes)
}

#[test]
fn a_neutral_ribbon_admits_neither_side() {
    use protopyne_strategy::slots::filter::RibbonFilter;
    let (bars, legs, closes) = constant_legs(1.0, 4);
    let filter = RibbonFilter;
    for index in 0..bars.len() {
        let tape = Tape::new(&bars, &legs, &closes, index).expect("tape binds");
        assert_eq!(
            filter.admit(&tape, Direction::Long),
            Admission::Block("ribbon is not aligned with the side")
        );
        assert_eq!(
            filter.admit(&tape, Direction::Short),
            Admission::Block("ribbon is not aligned with the side")
        );
    }
}

#[test]
fn a_zero_health_reading_admits_nothing() {
    use protopyne_strategy::slots::filter::{MaHealthFilter, RibbonHealthFilter};
    let (bars, mut legs, closes) = constant_legs(5.0, 4);
    legs.ma_health = vec![Some(0.0); 4];
    legs.ribbon_health = vec![Some(0.0); 4];
    let ma = MaHealthFilter { minimum: 10.0 };
    let ribbon = RibbonHealthFilter;
    for index in 0..bars.len() {
        let tape = Tape::new(&bars, &legs, &closes, index).expect("tape binds");
        assert_eq!(
            ma.admit(&tape, Direction::Long),
            Admission::Block("MA health is not aligned or is too weak")
        );
        assert_eq!(
            ma.admit(&tape, Direction::Short),
            Admission::Block("MA health is not aligned or is too weak")
        );
        assert_eq!(
            ribbon.admit(&tape, Direction::Long),
            Admission::Block("ribbon health is not aligned with the side")
        );
    }
}

#[test]
fn a_zero_delta_sum_admits_neither_side() {
    use protopyne_strategy::slots::filter::DeltaSideFilter;
    let start = chrono::Utc
        .with_ymd_and_hms(2026, 1, 5, 14, 30, 0)
        .single()
        .expect("valid instant");
    let bars: Vec<Bar> = (0..6)
        .map(|index| {
            Bar::new()
                .ts(start + chrono::Duration::minutes(index))
                .o(Decimal::from(500))
                .h(Decimal::from(501))
                .l(Decimal::from(499))
                .c(Decimal::from(500))
                .volume(0)
                .maybe_adj(None)
                .build()
        })
        .collect();
    let (legs, closes) = legs_for_bars(&bars);
    let filter = DeltaSideFilter { length: 3 };
    for index in 0..bars.len() {
        let tape = Tape::new(&bars, &legs, &closes, index).expect("tape binds");
        assert!(
            filter.admit(&tape, Direction::Long) != Admission::Pass,
            "a zero delta sum is not a long alignment"
        );
        assert!(
            filter.admit(&tape, Direction::Short) != Admission::Pass,
            "a zero delta sum is not a short alignment"
        );
    }
}

#[test]
fn a_volume_ratio_below_its_floor_blocks_and_the_floor_itself_admits() {
    use protopyne_strategy::slots::filter::VolumeMinFilter;
    let (bars, mut legs, closes) = constant_legs(1.0, 4);
    let filter = VolumeMinFilter { minimum: 0.8 };
    legs.vol_ratio = vec![0.79; 4];
    let tape = Tape::new(&bars, &legs, &closes, 2).expect("tape binds");
    assert_eq!(
        filter.admit(&tape, Direction::Long),
        Admission::Block("relative volume is below the floor")
    );
    // The reference admits on equality (`vol_ratio >= min`): the floor is
    // the first admissible ratio, not a block.
    legs.vol_ratio = vec![0.8; 4];
    let tape = Tape::new(&bars, &legs, &closes, 2).expect("tape binds");
    assert_eq!(filter.admit(&tape, Direction::Long), Admission::Pass);
}

#[test]
fn an_over_extended_bar_blocks_and_the_max_itself_admits() {
    use protopyne_strategy::slots::filter::AtrDistanceMaxFilter;
    let (bars, mut legs, closes) = constant_legs(1.0, 4);
    legs.mean = vec![Some(100.0); 4];
    legs.atr = vec![Some(1.0); 4];
    legs.close = vec![102.6; 4];
    let filter = AtrDistanceMaxFilter { maximum: 2.5 };
    let tape = Tape::new(&bars, &legs, &closes, 2).expect("tape binds");
    assert_eq!(
        filter.admit(&tape, Direction::Long),
        Admission::Block("price is too extended from the mean")
    );
    // The reference admits on equality (`atr_distance <= max`): the max is
    // the last admissible distance, not a block.
    legs.close = vec![102.5; 4];
    let tape = Tape::new(&bars, &legs, &closes, 2).expect("tape binds");
    assert_eq!(filter.admit(&tape, Direction::Long), Admission::Pass);
}

fn regime_legs(
    health: Option<f64>,
    dispersion: Option<f64>,
    participation: f64,
    len: usize,
) -> (Vec<Bar>, protopyne_kernels::lite::LiteLegs, Vec<f64>) {
    let (bars, mut legs, closes) = constant_legs(1.0, len);
    legs.ma_health = vec![health; len];
    legs.stdev = vec![dispersion; len];
    legs.atr = vec![Some(1.0); len];
    legs.vol_ratio = vec![participation; len];
    (bars, legs, closes)
}

#[test]
fn a_skipped_regime_blocks_and_an_unskipped_regime_admits() {
    use protopyne_strategy::slots::filter::RegimeSkip;
    // All-absent surfaces read the quiet label; with the quiet arm skipped
    // the bar is blocked.
    let (bars, legs, closes) = regime_legs(None, None, 1.0, 4);
    let skip = RegimeSkip::new(true, false);
    let tape = Tape::new(&bars, &legs, &closes, 2).expect("tape binds");
    assert_eq!(
        skip.admit(&tape, Direction::Long),
        Admission::Block("regime is too quiet")
    );
    // A regime outside the skip set is admitted: the reference blocks only
    // a regime it was told to skip.
    let (bars, legs, closes) = regime_legs(Some(5.0), Some(1.0), 3.0, 4);
    let skip = RegimeSkip::new(true, false);
    let tape = Tape::new(&bars, &legs, &closes, 2).expect("tape binds");
    assert_eq!(skip.admit(&tape, Direction::Long), Admission::Pass);
    // The skipped volatile arm blocks on a dispersion win.
    let (bars, legs, closes) = regime_legs(Some(10.0), Some(3.0), 1.0, 4);
    let skip = RegimeSkip::new(false, true);
    let tape = Tape::new(&bars, &legs, &closes, 2).expect("tape binds");
    assert_eq!(
        skip.admit(&tape, Direction::Long),
        Admission::Block("regime is too volatile")
    );
}

// ── Treatment: frozen admitted and armed index sets ─────────────────────────

fn score_fingerprint(verdicts: &[serde_json::Value]) -> serde_json::Value {
    let mut count = 0_usize;
    let mut conviction_sum = 0_i64;
    let mut activity_sum = 0_i64;
    for verdict in verdicts {
        if let Some(row) = verdict.as_array() {
            count += 1;
            conviction_sum += (row[1].as_f64().expect("score") * 1e12).round() as i64;
            activity_sum += (row[2].as_f64().expect("score") * 1e12).round() as i64;
        }
    }
    serde_json::json!({
        "admitted": count,
        "conviction_sum": conviction_sum,
        "activity_sum": activity_sum,
    })
}

fn treatment_tally() -> serde_json::Value {
    let tape = synthetic_tape(400);
    let legs = batch_of(&tape);
    let closes = closes_of(&tape);
    let mut configs = serde_json::Map::new();
    for (name, tweak) in configurations() {
        let fired = verdicts(&tape, tweak);
        let admitted: Vec<usize> = fired
            .iter()
            .enumerate()
            .filter_map(|(index, verdict)| verdict.as_array().map(|_| index))
            .collect();
        let mut config = IntegrationLiteStrategyConfig {
            warmup_bars: 30,
            ..IntegrationLiteStrategyConfig::default()
        };
        tweak(&mut config);
        let mut armed = serde_json::Map::new();
        for trigger in build_triggers(&config) {
            let probe = Tape::new(&tape, &legs, &closes, 0).expect("tape binds");
            let reason = trigger.eval(&probe).reason.to_owned();
            let (long, short, both) = armed_bars(&trigger, &tape, &legs, &closes);
            assert!(both.is_empty(), "{name}: no bar may arm both sides at once");
            armed.insert(reason, serde_json::json!({ "long": long, "short": short }));
        }
        configs.insert(
            name.to_owned(),
            serde_json::json!({
                "admitted": admitted,
                "scores": score_fingerprint(&fired),
                "triggers": armed,
            }),
        );
    }
    let one = load(&historical("SPY_1M.csv"));
    let anchor = verdicts(&one, |_| {});
    let anchor_admitted: Vec<usize> = anchor
        .iter()
        .enumerate()
        .filter_map(|(index, verdict)| verdict.as_array().map(|_| index))
        .collect();
    serde_json::json!({
        "method": "Per baseline configuration over the deterministic synthetic tape (full trigger/filter matrix) and the one-minute trim (default dials): the admitted-bar index set with a score fingerprint, plus every installed trigger's armed-bar index sets. The recompute compares number-for-number; precision and recall over the admitted sets are 1.0 by exact index-set equality. Evidence, not code: the index sets are the slot algebra's contract.",
        "scope": "native strategy signal only (triggers plus filters): no sizing, no exits, no order path",
        "fixtures": {
            "synthetic": { "bars": tape.len(), "configs": configs },
            "SPY_1M": {
                "bars": one.len(),
                "configs": { "default": {
                    "admitted": anchor_admitted,
                    "scores": score_fingerprint(&anchor),
                } },
            },
        },
    })
}

fn tally_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("slot_decision_tally.json")
}

fn precision_recall(expected: &BTreeSet<usize>, actual: &BTreeSet<usize>) -> (f64, f64) {
    if expected.is_empty() && actual.is_empty() {
        return (1.0, 1.0);
    }
    let hits = expected.intersection(actual).count() as f64;
    let precision = if actual.is_empty() {
        0.0
    } else {
        hits / actual.len() as f64
    };
    let recall = if expected.is_empty() {
        0.0
    } else {
        hits / expected.len() as f64
    };
    (precision, recall)
}

#[test]
fn the_slot_tally_matches_the_frozen_index_sets() {
    let actual = treatment_tally();
    let path = tally_path();
    if std::env::var("PROTOPYNE_RECORD").is_ok() {
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&actual).expect("serializes") + "\n",
        )
        .expect("tally records");
        return;
    }
    let text = std::fs::read_to_string(&path).expect("the slot tally is checked in");
    let recorded: serde_json::Value = serde_json::from_str(&text).expect("tally is JSON");
    assert_eq!(
        actual, recorded,
        "a trigger or filter change moved an admitted or armed bar: investigate before re-recording"
    );
}

#[test]
fn admitted_precision_and_recall_are_exact() {
    let text = std::fs::read_to_string(tally_path()).expect("the slot tally is checked in");
    let recorded: serde_json::Value = serde_json::from_str(&text).expect("tally is JSON");
    let tape = synthetic_tape(400);
    for (name, tweak) in configurations() {
        let frozen: BTreeSet<usize> = recorded["fixtures"]["synthetic"]["configs"][name]
            ["admitted"]
            .as_array()
            .expect("admitted indices")
            .iter()
            .map(|index| index.as_u64().expect("index") as usize)
            .collect();
        let live: BTreeSet<usize> = verdicts(&tape, tweak)
            .iter()
            .enumerate()
            .filter_map(|(index, verdict)| verdict.as_array().map(|_| index))
            .collect();
        let (precision, recall) = precision_recall(&frozen, &live);
        assert!(
            (precision - 1.0).abs() <= f64::EPSILON,
            "{name}: admitted precision must be exact"
        );
        assert!(
            (recall - 1.0).abs() <= f64::EPSILON,
            "{name}: admitted recall must be exact"
        );
    }
    let one = load(&historical("SPY_1M.csv"));
    let frozen: BTreeSet<usize> = recorded["fixtures"]["SPY_1M"]["configs"]["default"]["admitted"]
        .as_array()
        .expect("admitted indices")
        .iter()
        .map(|index| index.as_u64().expect("index") as usize)
        .collect();
    let live: BTreeSet<usize> = verdicts(&one, |_| {})
        .iter()
        .enumerate()
        .filter_map(|(index, verdict)| verdict.as_array().map(|_| index))
        .collect();
    let (precision, recall) = precision_recall(&frozen, &live);
    assert!(
        (precision - 1.0).abs() <= f64::EPSILON,
        "SPY_1M anchor: admitted precision must be exact"
    );
    assert!(
        (recall - 1.0).abs() <= f64::EPSILON,
        "SPY_1M anchor: admitted recall must be exact"
    );
}

/// The leg batch is causal: at every index, a batch over the whole tape reads
/// exactly what a batch over that bar's prefix would have read.
///
/// This is the licence for computing once per run instead of once per bar. It
/// covers the shipped trims and the synthetic oscillator tape, and it is the
/// reason a long run can stay linear without changing a single decision.
#[test]
fn the_leg_batch_is_prefix_equivalent_on_every_fixture() {
    use protopyne_kernels::lite::{lite_legs, LiteParams};

    let mut fixtures: Vec<(&str, Vec<Bar>)> = vec![
        (
            "SPY_5M",
            load(
                &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/data")
                    .join("SPY_5M.csv"),
            ),
        ),
        ("SPY_1M", load(&historical("SPY_1M.csv"))),
        ("synthetic", synthetic_tape(400)),
    ];
    let params = LiteParams::default();
    for (name, bars) in fixtures.drain(..) {
        let whole = lite_legs(&bars, &params, 14).expect("whole tape batches");
        for index in 0..bars.len() {
            let prefix = lite_legs(&bars[..=index], &params, 14).expect("prefix batches");
            assert_eq!(
                whole.at(index),
                prefix.at(index),
                "{name} index {index}: the batch must not look ahead"
            );
        }
    }
}
