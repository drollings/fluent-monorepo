//! Exit intents: the legacy-loss mapping and the quantity/price vocabulary.

use std::sync::Arc;

use chrono::{TimeZone as _, Utc};
use protopyne_types::{Bar, Money};
use rust_decimal::Decimal;

use crate::elements::{LossAction, LossMgmt, MarkContext, PositionView};
use crate::proposal::Direction;
use crate::slots::exit::{ExitQuantity, ExitRule, LossExitRule, RestingPrice};
use crate::tape::Tape;

struct AlwaysStop;

impl LossMgmt for AlwaysStop {
    fn name(&self) -> &str {
        "always-stop"
    }

    fn review(&self, _position: &PositionView<'_>, _mark: &MarkContext) -> Vec<LossAction> {
        vec![LossAction {
            kind: "stop".to_owned(),
            reason: "stop touched".to_owned(),
        }]
    }
}

fn tape() -> (Vec<Bar>, protopyne_kernels::lite::LiteLegs) {
    let bars: Vec<Bar> = (0i64..3)
        .map(|index| {
            let value = Decimal::from(100 + index);
            Bar::new()
                .ts(Utc
                    .with_ymd_and_hms(2026, 1, 5, 15, 0, 0)
                    .single()
                    .expect("time")
                    + chrono::Duration::minutes(index))
                .o(value)
                .h(value)
                .l(value)
                .c(value)
                .volume(1_000)
                .maybe_adj(None)
                .build()
        })
        .collect();
    let legs = protopyne_kernels::lite::lite_legs(
        &bars,
        &crate::signals_lite::LiteIndicatorParams::default(),
        14,
    )
    .expect("legs");
    (bars, legs)
}

#[test]
fn a_legacy_loss_rule_maps_to_a_whole_position_market_intent() {
    let (bars, legs) = tape();
    let tape = Tape::new(&bars, &legs, &legs.close, 1).expect("tape");
    let rule = LossExitRule::new(Arc::new(AlwaysStop));
    let context = crate::slots::exit::ExitContext {
        tape: &tape,
        position: PositionView {
            underlying: "SPY".to_owned(),
            side: Direction::Long,
            quantity: 10,
            entry_avg: Money::from(100),
            max_loss: Money::from(100),
            dte_days: None,
            legs: None,
            events: None,
        },
        mark: Money::from(101),
        high: Money::from(102),
        low: Money::from(99),
        best: Money::from(102),
        bars_held: 2,
        in_profit: true,
    };
    let intents = rule.review_exits(&context);
    assert_eq!(intents.len(), 1);
    assert_eq!(intents[0].kind, "stop");
    assert_eq!(intents[0].reason, "stop touched");
    assert_eq!(intents[0].quantity, ExitQuantity::All);
    assert_eq!(intents[0].price, RestingPrice::Market);
}

#[test]
fn a_custom_exit_rule_carries_its_own_quantity_and_price() {
    struct HalfAtTarget;
    impl ExitRule for HalfAtTarget {
        fn name(&self) -> &str {
            "half-at-target"
        }

        fn review_exits(
            &self,
            _context: &crate::slots::exit::ExitContext<'_>,
        ) -> Vec<crate::slots::exit::ExitIntent> {
            vec![crate::slots::exit::ExitIntent::partial_units(
                "take-profit",
                "first target",
                5,
                RestingPrice::At(Money::from(105)),
            )]
        }
    }
    let (bars, legs) = tape();
    let tape = Tape::new(&bars, &legs, &legs.close, 1).expect("tape");
    let rule = HalfAtTarget;
    let context = crate::slots::exit::ExitContext {
        tape: &tape,
        position: PositionView {
            underlying: "SPY".to_owned(),
            side: Direction::Long,
            quantity: 10,
            entry_avg: Money::from(100),
            max_loss: Money::from(100),
            dte_days: None,
            legs: None,
            events: None,
        },
        mark: Money::from(101),
        high: Money::from(106),
        low: Money::from(99),
        best: Money::from(106),
        bars_held: 2,
        in_profit: true,
    };
    let intents = rule.review_exits(&context);
    assert_eq!(intents[0].quantity, ExitQuantity::Units(5));
    assert_eq!(intents[0].price, RestingPrice::At(Money::from(105)));
}

// ── The default mapping: quiet controls ─────────────────────────────────────

use crate::elements::{ProfitTarget, StopLoss, TimeStop};
fn loss_set() -> crate::elements::LossSet {
    crate::elements::LossSet::new(vec![
        std::sync::Arc::new(StopLoss::new(Money::from(200)).expect("valid stop")),
        std::sync::Arc::new(ProfitTarget::new(Money::from(400)).expect("valid target")),
        std::sync::Arc::new(TimeStop::new(7)),
    ])
}

fn exit_context<'a>(
    tape: &'a Tape<'a>,
    dte_days: Option<u32>,
    mark: Money,
) -> crate::slots::exit::ExitContext<'a> {
    let in_profit = mark.as_decimal() > Decimal::from(250);
    crate::slots::exit::ExitContext {
        tape,
        position: PositionView {
            underlying: "SPY".to_owned(),
            side: Direction::Long,
            quantity: 2,
            entry_avg: Money::from(250),
            max_loss: Money::from(600),
            dte_days,
            legs: None,
            events: None,
        },
        mark,
        high: mark,
        low: mark,
        best: mark,
        bars_held: 1,
        in_profit,
    }
}

fn intents_for(marks: &[i64], dtes: &[Option<u32>]) -> Vec<Vec<String>> {
    let (bars, legs) = tape();
    let view = Tape::new(&bars, &legs, &legs.close, 1).expect("tape");
    let rule = LossExitRule::new(std::sync::Arc::new(loss_set()));
    marks
        .iter()
        .zip(dtes.iter())
        .map(|(mark, dte)| {
            let context = exit_context(&view, *dte, Money::from(*mark));
            rule.review_exits(&context)
                .into_iter()
                .map(|intent| intent.kind)
                .collect()
        })
        .collect()
}

#[test]
fn a_flat_book_proposes_nothing_through_the_default_mapping() {
    let kinds = intents_for(&[250], &[Some(30)]);
    assert!(
        kinds[0].is_empty(),
        "no distance from entry means no intent"
    );
}

#[test]
fn a_book_exactly_on_a_threshold_proposes_nothing() {
    // Down exactly 200, up exactly 400, exactly 7 days left: the boundary
    // holds, so the mapped intents stay empty too.
    let kinds = intents_for(&[150, 450], &[Some(30), Some(30)]);
    assert!(kinds[0].is_empty(), "exactly on the stop is not past it");
    assert!(kinds[1].is_empty(), "exactly on the target is not past it");
    let time = intents_for(&[250], &[Some(7)]);
    assert!(time[0].is_empty(), "exactly on the floor is not below it");
}

#[test]
fn a_rule_without_expiry_data_proposes_nothing() {
    // The time arm with no DTE holds; the stop/target arms see a flat book.
    let kinds = intents_for(&[250], &[None]);
    assert!(kinds[0].is_empty(), "absence is not a trigger");
}

// ── Treatment: frozen firing bars per rule ──────────────────────────────────

fn walk_kinds(marks: &[i64], dtes: &[Option<u32>]) -> serde_json::Value {
    let firings = intents_for(marks, dtes);
    let mut by_kind: std::collections::BTreeMap<&str, Vec<usize>> = [
        ("stop", Vec::new()),
        ("profit-target", Vec::new()),
        ("time-stop", Vec::new()),
    ]
    .into_iter()
    .collect();
    for (bar, kinds) in firings.iter().enumerate() {
        for kind in kinds {
            by_kind
                .get_mut(kind.as_str())
                .expect("a rule fires a known kind")
                .push(bar);
        }
    }
    serde_json::json!({
        "stop": by_kind["stop"],
        "profit-target": by_kind["profit-target"],
        "time-stop": by_kind["time-stop"],
    })
}

fn exit_treatment_tally() -> serde_json::Value {
    let stop_walk = walk_kinds(
        &[250, 200, 150, 149],
        &[Some(30), Some(30), Some(30), Some(30)],
    );
    let target_walk = walk_kinds(
        &[250, 350, 450, 451],
        &[Some(30), Some(30), Some(30), Some(30)],
    );
    let flat = [250, 250, 250, 250];
    let time_walk = walk_kinds(&flat, &[Some(30), Some(8), Some(7), Some(6)]);
    for (name, walk) in [
        ("stop walk", &stop_walk),
        ("target walk", &target_walk),
        ("time walk", &time_walk),
    ] {
        let total: usize = ["stop", "profit-target", "time-stop"]
            .iter()
            .map(|kind| walk[kind].as_array().expect("bars").len())
            .sum();
        assert_eq!(total, 1, "{name} must fire exactly its own rule, once");
    }
    serde_json::json!({
        "method": "Each walk is reviewed bar-by-bar through the default exit-rule mapping (legacy loss review to whole-position market intents); the tally freezes which bars fire which kind. Evidence, not code: the firing bars are the rules' contract.",
        "scope": "exit-rule default mapping only: no manager state, no submission, no fill",
        "mapping": "every legacy action becomes a whole-position market intent, kind and reason preserved",
        "stop_path": stop_walk,
        "target_path": target_walk,
        "time_path": time_walk,
        "thresholds": "stop_max_loss=200;target_gain=400;time_floor_dte=7",
    })
}

fn exit_golden_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("exit_rule_tally.json")
}

#[test]
fn the_exit_rule_tally_matches_the_frozen_firing_bars() {
    let actual = serde_json::to_string_pretty(&exit_treatment_tally()).expect("serializable");
    let path = exit_golden_path();
    if std::env::var("PROTOPYNE_RECORD").is_ok() {
        std::fs::create_dir_all(path.parent().expect("tally has a parent"))
            .expect("tally directory exists");
        std::fs::write(&path, format!("{actual}\n")).expect("tally records");
        return;
    }
    let recorded: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&path).expect("the exit tally is checked in"),
    )
    .expect("tally is JSON");
    let actual_value: serde_json::Value = serde_json::from_str(&actual).expect("serializable");
    assert_eq!(
        actual_value, recorded,
        "an exit-rule change moved a firing bar"
    );
}
