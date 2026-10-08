//! Kernel compatibility matrix: per-kernel parity epsilon and status.
//!
//! `eps` is a **correctness** threshold: the maximum absolute error allowed
//! between a kernel's output and its hand-computed Pine-formula reference on
//! the calibration sets. A kernel that cannot meet its eps stays
//! [`CompatStatus::Partial`], never gets a silently widened eps, and never
//! guards money until it verifies. `ParityVerified` additionally requires a
//! direct TradingView-export cross-check, which needs export access the
//! prototype does not have; every kernel below therefore carries its
//! provenance honestly until that cross-check lands.

use crate::indicator::IndicatorRegistry;

/// Verification standing of one kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompatStatus {
    /// Meets eps on the calibration sets and matches a TradingView export.
    ParityVerified,
    /// Meets eps on the Pine-formula calibration sets; the direct
    /// TradingView-export cross-check is pending for the stated reason.
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
        status: CompatStatus::FormulaVerified("tradingview export cross-check pending"),
    },
    KernelCompat {
        id: "ema",
        eps: 1e-9,
        status: CompatStatus::FormulaVerified("tradingview export cross-check pending"),
    },
    KernelCompat {
        id: "rma",
        eps: 1e-9,
        status: CompatStatus::FormulaVerified("tradingview export cross-check pending"),
    },
    KernelCompat {
        id: "wma",
        eps: 1e-9,
        status: CompatStatus::FormulaVerified("tradingview export cross-check pending"),
    },
    KernelCompat {
        id: "hma",
        eps: 1e-9,
        status: CompatStatus::FormulaVerified("tradingview export cross-check pending"),
    },
    KernelCompat {
        id: "vwma",
        eps: 1e-9,
        status: CompatStatus::FormulaVerified("tradingview export cross-check pending"),
    },
    KernelCompat {
        id: "alma",
        eps: 1e-9,
        status: CompatStatus::FormulaVerified("tradingview export cross-check pending"),
    },
    KernelCompat {
        id: "rsi",
        eps: 1e-9,
        status: CompatStatus::FormulaVerified("tradingview export cross-check pending"),
    },
    KernelCompat {
        id: "macd",
        eps: 1e-9,
        status: CompatStatus::FormulaVerified("tradingview export cross-check pending"),
    },
    KernelCompat {
        id: "stoch",
        eps: 1e-9,
        status: CompatStatus::FormulaVerified("tradingview export cross-check pending"),
    },
    KernelCompat {
        id: "tr",
        eps: 1e-9,
        status: CompatStatus::FormulaVerified("tradingview export cross-check pending"),
    },
    KernelCompat {
        id: "atr",
        eps: 1e-9,
        status: CompatStatus::FormulaVerified("tradingview export cross-check pending"),
    },
    KernelCompat {
        id: "bb",
        eps: 1e-9,
        status: CompatStatus::FormulaVerified("tradingview export cross-check pending"),
    },
    KernelCompat {
        id: "kc",
        eps: 1e-9,
        status: CompatStatus::FormulaVerified("tradingview export cross-check pending"),
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
