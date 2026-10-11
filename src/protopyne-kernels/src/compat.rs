//! Kernel compatibility matrix: per-kernel parity epsilon and status.
//!
//! `eps` is a **correctness** threshold: the maximum absolute error allowed
//! between a kernel's output and its `polars.Expr` reference on the
//! calibration sets. Numeric authority is the `Expr` output (the global
//! calibration gate measures every row against staged reference expressions
//! at its frozen eps); the direct TradingView-export cross-check remains
//! unavailable to the prototype and is not what this status means. A kernel
//! that cannot meet its eps stays [`CompatStatus::FormulaVerified`], never
//! gets a silently widened eps, and never guards money until it verifies.
//! Frozen eps values never widen afterwards: the global gate snapshots the
//! whole table.

use crate::indicator::IndicatorRegistry;

/// Verification standing of one kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompatStatus {
    /// Meets frozen eps against the `Expr` reference on the calibration sets
    /// with the global must-NOT-fire controls quiet.
    ParityVerified,
    /// Meets eps on the hand-computed formula sets; the `Expr` cross-check
    /// is pending for the stated reason.
    FormulaVerified(&'static str),
}

/// One row of the compatibility matrix.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KernelCompat {
    /// Registry id.
    pub id: &'static str,
    /// Maximum absolute error vs. the hand-computed reference.
    pub eps: f64,
    /// Verification standing.
    pub status: CompatStatus,
}

/// Frozen per-kernel epsilons. These values are set once the must-NOT-fire
/// control set passes untouched and never widened afterwards.
pub const COMPATIBILITY: &[KernelCompat] = &[
    KernelCompat {
        id: "sma",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "ema",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "rma",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "wma",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "hma",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "vwma",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "alma",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "dema",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "tema",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "rsi",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "macd",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "stoch",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "tr",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "atr",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "bb",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "kc",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "bbands",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "midpoint",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "midprice",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "trima",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "avgprice",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "medprice",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "typprice",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "wclprice",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "natr",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "apo",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "mom",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "ppo",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "roc",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "rocp",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "rocr",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "rocr100",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "rsv",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "stochf",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "trix",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "willr",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "aroon",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "stddev",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "var",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "correl",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "ad",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "adosc",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "obv",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "cci",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "cmo",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "mfi",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "vwap",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "eit",
        eps: 1e-9,
        status: CompatStatus::FormulaVerified("transcribed Pine recurrence, hand-stepped sets green; staged-expression cross-check pending"),
    },
    KernelCompat {
        id: "kalman",
        eps: 1e-9,
        status: CompatStatus::FormulaVerified("transcribed Pine recurrence, hand-stepped sets green; staged-expression cross-check pending"),
    },
    KernelCompat {
        id: "voss",
        eps: 1e-9,
        status: CompatStatus::FormulaVerified("transcribed Pine recurrence, hand-stepped sets green; staged-expression cross-check pending"),
    },
];

/// Look up a kernel's compatibility row.
#[must_use]
pub fn compat_for(id: &str) -> Option<&'static KernelCompat> {
    COMPATIBILITY.iter().find(|row| row.id == id)
}

/// Ids the matrix must cover: every builtin kernel, no more, no less.
pub fn assert_matrix_covers(registry: &IndicatorRegistry) -> Result<(), String> {
    for id in registry.ids() {
        let row = compat_for(&id).ok_or_else(|| format!("no compatibility row for {id:?}"))?;
        if !(row.eps.is_finite() && row.eps > 0.0) {
            return Err(format!("non-positive eps for {id:?}"));
        }
    }
    for row in COMPATIBILITY {
        if registry.get(row.id).is_none() {
            return Err(format!("stale compatibility row {:?}", row.id));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "../tests/compat.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/cal.rs"]
mod cal_tests;

#[cfg(test)]
#[path = "../tests/cal_global.rs"]
mod cal_global_tests;
