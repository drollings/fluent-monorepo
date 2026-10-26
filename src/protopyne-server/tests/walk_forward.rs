use super::*;
use crate::common_fixtures::money;
use protopyne_strategy::fixtures::grant_with;

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use rust_decimal::Decimal;

use protopyne_strategy::{
    elements::ExplicitStructure, Direction, FixedSignal, Grant, RiskEngine, RiskLimits, Strategy,
    StructureKind,
};
use protopyne_types::Bar;

fn strategy() -> Strategy {
    Strategy::with_signal(Arc::new(
        FixedSignal::new(Direction::Long, 0.8, 0.8).expect("valid signal"),
    ))
    .with_structure(Arc::new(ExplicitStructure {
        structure: StructureKind::Vertical,
    }))
}

fn risk() -> RiskEngine {
    RiskEngine::new(RiskLimits::new().build())
}

fn grant(min_conviction: f64) -> Grant {
    // The shared fixture's shape with the walk-forward conviction floor.
    grant_with(|grant| {
        grant.id = format!("walk-forward-{min_conviction}");
        grant.signal.min_conviction = min_conviction;
        grant.signal.min_activity_score = 0.5;
    })
}

fn venue_config() -> BacktestConfig {
    BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("wf-synth-v1".to_owned())
        .build()
}

fn runner_config() -> RunnerConfig {
    RunnerConfig::new(
        "SPY".to_owned(),
        "ribbon".to_owned(),
        money("400"),
        money("2_000"),
        30,
        0,
    )
}

fn synthetic_bar(close: Decimal, minute: i64) -> Bar {
    Bar::new()
        .ts(Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap() + Duration::minutes(minute))
        .o(close)
        .h(close)
        .l(close)
        .c(close)
        .volume(1_000)
        .maybe_adj(None)
        .build()
}

/// A series whose first `split` bars trend up (five per bar) and whose tail
/// trends down by the same step: a Long-only strategy has a real in-sample
/// edge that must not generalize out of sample.
fn edge_then_decay(bars: usize, split: usize) -> Vec<Bar> {
    (0..bars)
        .map(|index| {
            let close = if index < split {
                Decimal::from(100 + 5 * index as i64)
            } else {
                Decimal::from(100 + 5 * split as i64) - Decimal::from(5 * (index - split) as i64)
            };
            synthetic_bar(close, index as i64)
        })
        .collect()
}

#[test]
fn fold_plan_boundaries_are_hand_checked() {
    let folds = walk_forward_folds(10, 4, 2, 2).expect("plan");
    assert_eq!(
        folds,
        vec![
            WalkForwardFold {
                train_start: 0,
                train_end: 4,
                test_end: 6,
            },
            WalkForwardFold {
                train_start: 2,
                train_end: 6,
                test_end: 8,
            },
            WalkForwardFold {
                train_start: 4,
                train_end: 8,
                test_end: 10,
            },
        ]
    );
    assert_eq!(folds[1].train_range(), 2..6);
    assert_eq!(folds[1].test_range(), 6..8);
    assert_eq!(folds[1].train_len(), 4);
    assert_eq!(folds[1].test_len(), 2);

    // An anchor step (== train_len) overlaps nothing.
    let anchored = walk_forward_folds(10, 4, 2, 4).expect("anchored plan");
    assert_eq!(anchored.len(), 2);
    assert_eq!(anchored[1].train_start, 4);
    assert_eq!(anchored[1].test_end, 10);

    assert!(walk_forward_folds(10, 0, 2, 2).is_err());
    assert!(walk_forward_folds(10, 4, 0, 2).is_err());
    assert!(walk_forward_folds(10, 4, 2, 0).is_err());
    assert!(
        walk_forward_folds(10, 4, 4, 2).is_err(),
        "a step shorter than the test window would overlap OOS windows"
    );
    assert!(walk_forward_folds(3, 4, 2, 2).is_err(), "no window fits");
}

#[tokio::test]
async fn is_only_edge_degrades_out_of_sample() {
    let strategy = strategy();
    let risk = risk();
    let grant = grant(0.5);
    let bars = edge_then_decay(40, 20);
    let folds = vec![WalkForwardFold::sequential(0, 20, 20)];
    let run = run_walk_forward(
        &strategy,
        &risk,
        &grant,
        &venue_config(),
        &runner_config(),
        &bars,
        &folds,
        11,
    )
    .await
    .expect("walk-forward runs");
    assert_eq!(run.folds.len(), 1);
    let comparison = &run.report.comparisons[0];
    assert_eq!(comparison.in_sample.bars, 20);
    assert_eq!(comparison.out_of_sample.bars, 20);
    assert!(comparison.in_sample.fills > 0, "IS must fill");
    assert!(comparison.out_of_sample.fills > 0, "OOS must fill");
    // The section is a faithful projection of the standalone reports.
    assert_eq!(
        comparison.in_sample.fills,
        run.folds[0].in_sample.report.fills
    );
    assert_eq!(
        comparison.out_of_sample.fills,
        run.folds[0].out_of_sample.report.fills
    );
    assert_eq!(
        comparison.in_sample.sharpe,
        run.folds[0].in_sample.report.sharpe
    );
    assert_eq!(
        comparison.out_of_sample.capital_efficiency,
        run.folds[0].out_of_sample.report.capital_efficiency
    );

    let is_end = run.folds[0]
        .in_sample
        .report
        .equity_curve
        .last()
        .copied()
        .expect("IS curve");
    let oos_end = run.folds[0]
        .out_of_sample
        .report
        .equity_curve
        .last()
        .copied()
        .expect("OOS curve");
    assert!(
        is_end.as_decimal() > Decimal::ZERO,
        "the in-sample edge must be real, ended at {is_end}"
    );
    assert!(
        oos_end.as_decimal() < Decimal::ZERO,
        "the edge must not generalize, ended at {oos_end}"
    );
    assert!(
        comparison.out_of_sample.drawdown.as_decimal() > comparison.in_sample.drawdown.as_decimal(),
        "the out-of-sample window must draw down more than the in-sample one"
    );

    // The whole walk-forward pin is frozen: a rerun is byte-identical.
    let again = run_walk_forward(
        &strategy,
        &risk,
        &grant,
        &venue_config(),
        &runner_config(),
        &bars,
        &folds,
        11,
    )
    .await
    .expect("rerun");
    assert_eq!(run.to_canonical_json(), again.to_canonical_json());
    assert_eq!(
        run.manifest.to_canonical_json(),
        again.manifest.to_canonical_json()
    );
}

#[tokio::test]
async fn fold_concurrency_produces_index_sorted_output() {
    let strategy = strategy();
    let risk = risk();
    let grant = grant(0.5);
    let bars = edge_then_decay(80, 40);
    let folds = walk_forward_folds(bars.len(), 20, 10, 10).expect("plan");
    assert!(folds.len() >= 3, "several folds exercise the pool");
    let first = run_walk_forward(
        &strategy,
        &risk,
        &grant,
        &venue_config(),
        &runner_config(),
        &bars,
        &folds,
        11,
    )
    .await
    .expect("first run");
    let second = run_walk_forward(
        &strategy,
        &risk,
        &grant,
        &venue_config(),
        &runner_config(),
        &bars,
        &folds,
        11,
    )
    .await
    .expect("second run");
    assert_eq!(first, second, "parallel reruns identical wholesale");
    assert_eq!(first.manifest.folds, folds, "the pin keeps input order");
    assert_eq!(first.folds.len(), folds.len());
    // Index-sorted: fold `i`'s runs are the windows `i` names, whatever
    // order the pool completed them in.
    for (fold, run_fold) in folds.iter().zip(&first.folds) {
        assert_eq!(
            run_fold.in_sample.manifest.bar_count,
            fold.train_len() as u64
        );
        assert_eq!(
            run_fold.out_of_sample.manifest.bar_count,
            fold.test_len() as u64
        );
        assert_eq!(
            run_fold.in_sample.manifest.first_ts,
            bars[fold.train_start].ts.to_rfc3339()
        );
        assert_eq!(
            run_fold.out_of_sample.manifest.first_ts,
            bars[fold.train_end].ts.to_rfc3339()
        );
    }
}

#[tokio::test]
async fn degenerate_folds_refuse() {
    let strategy = strategy();
    let risk = risk();
    let grant = grant(0.5);
    let bars = edge_then_decay(40, 20);
    let venue = venue_config();
    let runner = runner_config();
    let ok = vec![WalkForwardFold::sequential(0, 10, 10)];

    assert!(
        run_walk_forward(&strategy, &risk, &grant, &venue, &runner, &bars, &[], 11)
            .await
            .is_err(),
        "an empty fold list refuses"
    );
    assert!(
        run_walk_forward(&strategy, &risk, &grant, &venue, &runner, &[], &ok, 11)
            .await
            .is_err(),
        "an empty history refuses"
    );
    let past = vec![WalkForwardFold {
        train_start: 0,
        train_end: 30,
        test_end: 50,
    }];
    assert!(
        run_walk_forward(&strategy, &risk, &grant, &venue, &runner, &bars, &past, 11)
            .await
            .is_err(),
        "a fold past the history refuses"
    );
    let overlap = vec![
        WalkForwardFold {
            train_start: 0,
            train_end: 10,
            test_end: 20,
        },
        WalkForwardFold {
            train_start: 0,
            train_end: 15,
            test_end: 25,
        },
    ];
    assert!(
        run_walk_forward(&strategy, &risk, &grant, &venue, &runner, &bars, &overlap, 11)
            .await
            .is_err(),
        "overlapping out-of-sample windows refuse"
    );
    let empty_train = vec![WalkForwardFold {
        train_start: 5,
        train_end: 5,
        test_end: 10,
    }];
    assert!(
        run_walk_forward(
            &strategy,
            &risk,
            &grant,
            &venue,
            &runner,
            &bars,
            &empty_train,
            11
        )
        .await
        .is_err(),
        "an empty train window refuses"
    );
}
