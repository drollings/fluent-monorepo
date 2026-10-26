//! The stored-run registry: publish, read, drop.

use protopyne_view::BacktestRunView;

use super::BacktestEntry;

fn entry(id: &str) -> BacktestEntry {
    BacktestEntry {
        run_id: id.to_owned(),
        view: BacktestRunView {
            run_id: id.to_owned(),
            manifest_json: "{}".to_owned(),
            metrics: Vec::new(),
            equity: Vec::new(),
            trades: Vec::new(),
            markers: Vec::new(),
            status: None,
            warnings: Vec::new(),
        },
    }
}

#[test]
fn a_published_run_reads_back_identically() {
    let mut registry = super::BacktestRegistry::new();
    assert!(registry.is_empty());
    registry.publish(entry("backtest-01"));
    assert_eq!(registry.len(), 1);
    let stored = registry.get("backtest-01").expect("published");
    assert_eq!(stored.view.run_id, "backtest-01");
    assert_eq!(registry.ids(), vec!["backtest-01".to_owned()]);
}

#[test]
fn dropping_an_unknown_run_refuses_by_answer() {
    let mut registry = super::BacktestRegistry::new();
    assert!(!registry.remove("missing"));
    registry.publish(entry("backtest-01"));
    assert!(registry.remove("backtest-01"));
    assert!(registry.is_empty());
    assert!(registry.get("backtest-01").is_none());
}
