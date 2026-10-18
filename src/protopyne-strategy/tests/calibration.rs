//! Grant threshold calibration: must-fire vs. must-NOT-fire sets plus the
//! precision/recall report on a labeled proposal corpus.
//!
//! Protocol: the must-fire set proves each threshold can deny (a threshold
//! that never fires guards nothing); the must-NOT-fire control set proves a
//! correct proposal still passes (a threshold that denies everything prices
//! nothing). Thresholds below are frozen only because both sets pass
//! together; any retune re-runs this corpus first.

use crate::*;
use chrono::{TimeZone, Utc};
use protopyne_types::{Money, Qty};
use rust_decimal::Decimal;

fn money(value: i64) -> Money {
    Money::new(Decimal::from(value))
}

fn proposal(id: &str) -> Proposal {
    Proposal::new()
        .id(id.to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .direction(Direction::Long)
        .side(ProposalSide::Entry)
        .structure(StructureKind::Vertical)
        .quantity(Qty::new(Decimal::from(2)))
        .limit_price(money(250))
        .max_loss(money(300))
        .notional(money(1_000))
        .dte_days(30)
        .conviction(0.8)
        .activity_score(0.7)
        .state(ProposalState::Proposed)
        .created_at(Utc::now())
        .build()
}

fn grant() -> Grant {
    Grant::new()
        .id("cal".to_owned())
        .scope(
            GrantScope::new()
                .strategies(vec!["ribbon".to_owned()])
                .underlyings(vec!["SPY".to_owned()])
                .structures(vec![StructureKind::Vertical, StructureKind::NakedShort])
                .directions(vec![Direction::Long, Direction::Short])
                .permissions(GrantPermission {
                    entries: true,
                    exits: true,
                    rolls: false,
                })
                .build(),
        )
        .budgets(
            GrantBudgets::new()
                .max_trades(10)
                .max_contracts(100)
                .max_loss(money(5_000))
                .max_notional(money(100_000))
                .build(),
        )
        .signal(
            SignalThresholds::new()
                .min_conviction(0.5)
                .min_activity_score(0.5)
                .build(),
        )
        .trade(
            TradeThresholds::new()
                .max_loss_per_trade(money(1_000))
                .max_spread_width(money(50))
                .min_dte_days(7)
                .max_dte_days(60)
                .build(),
        )
        .portfolio(
            PortfolioThresholds::new()
                .max_open_risk(money(10_000))
                .quote_max_age_secs(60)
                .build(),
        )
        .breakers(BreakerLimits::new().max_consecutive_errors(3).build())
        .valid_from(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap())
        .valid_until(Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap())
        .build()
}

fn fresh_market() -> GrantMarket {
    GrantMarket::new()
        .quote_age_secs(5)
        .spread_width(money(10))
        .build()
}

fn now() -> chrono::DateTime<chrono::Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap()
}

/// One labeled corpus row: the proposal, the market and usage it is judged
/// under, and whether the grant should cover it.
struct CorpusRow {
    proposal: Proposal,
    usage: GrantUsage,
    market: GrantMarket,
    breakers: BreakerState,
    expect_allow: bool,
    label: &'static str,
}

fn corpus() -> Vec<CorpusRow> {
    let usage = GrantUsage::new().build();
    let market = fresh_market();
    let breakers = BreakerState::new().build();
    let mut rows = vec![
        CorpusRow {
            proposal: proposal("control-valid-vertical"),
            usage,
            market,
            breakers: breakers.clone(),
            expect_allow: true,
            label: "control: valid defined-risk vertical inside all budgets",
        },
        CorpusRow {
            proposal: {
                let mut p = proposal("control-valid-exit");
                p.side = ProposalSide::Exit;
                p
            },
            usage,
            market,
            breakers: breakers.clone(),
            expect_allow: true,
            label: "control: valid exit under an exit-permitting grant",
        },
    ];

    let mut over_budget = proposal("fire-over-budget");
    over_budget.max_loss = money(900);
    rows.push(CorpusRow {
        proposal: over_budget,
        usage: GrantUsage::new().used_loss(money(4_500)).build(),
        market,
        breakers: breakers.clone(),
        expect_allow: false,
        label: "must-fire: committed loss exceeds the max-loss budget",
    });

    rows.push(CorpusRow {
        proposal: proposal("fire-stale-quote"),
        usage,
        market: GrantMarket::new()
            .quote_age_secs(600)
            .spread_width(money(10))
            .build(),
        breakers: breakers.clone(),
        expect_allow: false,
        label: "must-fire: stale quote against a fresh-quote gate",
    });

    let mut naked = proposal("fire-naked-short");
    naked.structure = StructureKind::NakedShort;
    rows.push(CorpusRow {
        proposal: naked,
        usage: GrantUsage::new().build(),
        market,
        breakers: breakers.clone(),
        expect_allow: false,
        label: "must-fire: naked short excluded from defined-risk grants",
    });

    let mut weak = proposal("fire-weak-signal");
    weak.conviction = 0.2;
    rows.push(CorpusRow {
        proposal: weak,
        usage: GrantUsage::new().build(),
        market,
        breakers: breakers.clone(),
        expect_allow: false,
        label: "must-fire: conviction below the signal threshold",
    });

    let mut wide = proposal("fire-wide-market");
    wide.dte_days = 200;
    rows.push(CorpusRow {
        proposal: wide,
        usage: GrantUsage::new().build(),
        market,
        breakers,
        expect_allow: false,
        label: "must-fire: DTE outside the trade window",
    });

    rows
}

/// Precision/recall tuning report over the labeled corpus. Computed fresh on
/// every run, before any threshold is cached or persisted anywhere.
struct TuningReport {
    true_positives: usize,
    false_positives: usize,
    false_negatives: usize,
    precision: f64,
    recall: f64,
}

fn tuning_report(rows: &[CorpusRow], grant: &Grant) -> (TuningReport, Vec<String>) {
    let mut true_positives = 0_usize;
    let mut false_positives = 0_usize;
    let mut false_negatives = 0_usize;
    let mut failures = Vec::new();
    for row in rows {
        let allowed = evaluate_grant(
            &row.proposal,
            grant,
            &row.usage,
            &row.market,
            &row.breakers,
            now(),
        )
        .is_allow();
        match (allowed, row.expect_allow) {
            (true, true) => true_positives += 1,
            (true, false) => {
                false_positives += 1;
                failures.push(format!("false allow: {}", row.label));
            }
            (false, true) => {
                false_negatives += 1;
                failures.push(format!("false deny: {}", row.label));
            }
            (false, false) => {}
        }
    }
    let precision = true_positives as f64 / (true_positives + false_positives) as f64;
    let recall = true_positives as f64 / (true_positives + false_negatives) as f64;
    (
        TuningReport {
            true_positives,
            false_positives,
            false_negatives,
            precision,
            recall,
        },
        failures,
    )
}

#[test]
fn thresholds_fire_where_they_must_and_nowhere_else() {
    let rows = corpus();
    assert!(
        rows.iter().any(|r| r.expect_allow),
        "corpus needs a must-NOT-fire control set"
    );
    assert!(
        rows.iter().any(|r| !r.expect_allow),
        "corpus needs a must-fire set"
    );
    let (report, failures) = tuning_report(&rows, &grant());
    assert!(
        failures.is_empty(),
        "calibration failures: {failures:?} (precision={:.3}, recall={:.3})",
        report.precision,
        report.recall
    );
    assert_eq!(report.false_positives, 0, "no false allows on the corpus");
    assert_eq!(report.false_negatives, 0, "no false denies on the corpus");
    assert!(report.true_positives >= 2, "control set must allow");
}

#[test]
fn grant_evaluation_stays_pure_across_repeated_runs() {
    let rows = corpus();
    let first: Vec<bool> = rows
        .iter()
        .map(|r| {
            evaluate_grant(
                &r.proposal,
                &grant(),
                &r.usage,
                &r.market,
                &r.breakers,
                now(),
            )
            .is_allow()
        })
        .collect();
    let second: Vec<bool> = rows
        .iter()
        .map(|r| {
            evaluate_grant(
                &r.proposal,
                &grant(),
                &r.usage,
                &r.market,
                &r.breakers,
                now(),
            )
            .is_allow()
        })
        .collect();
    assert_eq!(first, second, "same inputs must decide identically");
}
