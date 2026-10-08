use super::*;
use crate::order::Position;
use protopyne_types::Money;
use std::collections::HashMap;

fn position(underlying: &str, qty: i64) -> Position {
    Position::new()
        .underlying(underlying.to_owned())
        .quantity(qty)
        .avg_price(Money::zero())
        .build()
}

#[test]
fn agreement_is_quiet() {
    let journal = HashMap::from([("SPY".to_owned(), 2_i64)]);
    let report = reconcile(&journal, &[position("SPY", 2)]);
    assert!(report.matched);
    assert!(report.mismatches.is_empty());
    assert!(report.alerts.is_empty());
}

#[test]
fn every_disagreement_is_named() {
    let journal = HashMap::from([("SPY".to_owned(), 2_i64)]);
    let report = reconcile(&journal, &[position("SPY", 3)]);
    assert!(!report.matched);
    assert_eq!(report.mismatches.len(), 1);
    assert_eq!(report.mismatches[0].journal_qty, 2);
    assert_eq!(report.mismatches[0].venue_qty, 3);
    assert_eq!(report.alerts.len(), 1);

    let report = reconcile(&journal, &[position("QQQ", 1)]);
    assert_eq!(report.mismatches.len(), 2);
}

#[test]
fn gate_halts_entries_until_the_book_agrees() {
    let mut gate = ReconcileGate::default();
    assert!(!gate.entries_halted());
    assert!(gate.check_entries().is_ok());

    let journal = HashMap::from([("SPY".to_owned(), 2_i64)]);
    gate.observe(&reconcile(&journal, &[position("SPY", 5)]));
    assert!(gate.entries_halted());
    assert!(gate.check_entries().is_err());
    assert_eq!(gate.alerts().len(), 1);

    gate.observe(&reconcile(&journal, &[position("SPY", 2)]));
    assert!(!gate.entries_halted());
    assert!(gate.check_entries().is_ok());
}

#[test]
fn journal_net_accumulates_signed_fills() {
    let net = journal_net(&[
        ("SPY".to_owned(), 2),
        ("SPY".to_owned(), -1),
        ("QQQ".to_owned(), 4),
    ]);
    assert_eq!(net.get("SPY"), Some(&1));
    assert_eq!(net.get("QQQ"), Some(&4));
}
