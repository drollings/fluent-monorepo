//! Warmup honesty for the shell-built families.
//!
//! `warmup` is a promise: the first bar the batch path can answer. Every kernel
//! built through the windowed or chained shell must keep that promise exactly —
//! no bar early (an answer from a half-filled window) and no bar late (a
//! needless `na`). The two composed families are where a wrong pass count or a
//! wrong per-pass width would show up first.

use super::*;

/// A fixture with every leg aligned, so window and volume kernels all answer.
fn bars(count: usize) -> OwnedOhlcv {
    let close: Vec<f64> = (0..count as i32)
        .map(|index| 100.0 + f64::from(index) * 0.1)
        .collect();
    OwnedOhlcv {
        open: close.clone(),
        high: close.iter().map(|value| value + 0.5).collect(),
        low: close.iter().map(|value| value - 0.5).collect(),
        close,
        volume: (0..count as i32)
            .map(|index| 1_000.0 + f64::from(index))
            .collect(),
    }
}

#[test]
fn every_shell_kernel_answers_exactly_at_its_declared_warmup() {
    let registry = builtin_registry().expect("registry builds");
    let data = bars(160);
    let cases = [
        ("sma", 20_i64),
        ("ema", 20),
        ("rma", 20),
        ("wma", 20),
        ("vwma", 20),
        ("alma", 20),
        ("dema", 14),
        ("tema", 10),
        ("trima", 20),
    ];
    for (id, length) in cases {
        let indicator = registry.get(id).expect("registered");
        let params = indicator.spec().default_params().with_int("length", length);
        let warmup = indicator.warmup(&params).expect("warmup resolves");
        let batch = indicator.batch(&data.views(), &params).expect("batch runs");
        assert_eq!(batch.values.len(), data.len(), "{id}: one output per bar");
        let first = batch
            .values
            .iter()
            .position(Option::is_some)
            .unwrap_or_else(|| panic!("{id}: no output at all"));
        assert_eq!(
            first + 1,
            warmup,
            "{id}: the declared warmup must be the first answered bar"
        );
        assert!(
            batch.values[..first].iter().all(Option::is_none),
            "{id}: no answers before warmup"
        );
    }
}

/// The same promise, over **every** registered kernel: the reported warmup is
/// the first bar the batch path answers. The per-family cases above pin the
/// shells with explicit parameters; this one pins the whole registry with its
/// declared defaults, so a kernel that drifts either way is named.
#[test]
fn every_registered_kernel_answers_exactly_at_its_reported_warmup() {
    let registry = builtin_registry().expect("registry builds");
    let data = bars(400);
    let mut mismatches = Vec::new();
    for id in registry.ids() {
        let indicator = registry.get(&id).expect("registered");
        let params = indicator.spec().default_params();
        let warmup = indicator.warmup(&params).expect("warmup resolves");
        let batch = indicator
            .batch(&data.views(), &params)
            .unwrap_or_else(|error| panic!("{id}: defaults refused: {error}"));
        let Some(first) = batch.values.iter().position(Option::is_some) else {
            mismatches.push(format!("{id}: no primary output within 400 bars"));
            continue;
        };
        if first + 1 != warmup {
            mismatches.push(format!(
                "{id}: reported {warmup}, the primary first answers at bar {}",
                first + 1
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "warmup must be the first answered primary bar:\n{}",
        mismatches.join("\n")
    );
}
