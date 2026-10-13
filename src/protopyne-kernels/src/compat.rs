//! Kernel compatibility matrix: per-kernel Expr-parity epsilon and status.
//!
//! `eps` is a **correctness** threshold: the maximum absolute error allowed
//! between a kernel's output and its `polars.Expr` reference on the
//! calibration sets. Numeric authority is the `Expr` output (the global
//! calibration gate measures every row against staged reference expressions
//! at its frozen eps) — that staged-`Expr` authority is what the
//! `ParityVerified` standing claims, here and everywhere the docs say
//! Expr-parity; the direct TradingView-export cross-check remains
//! unavailable to the prototype and is not what this status means (the
//! assumed-vs-verified ledger lives in `doc/protopyne-rs/DIVERGENCE_LOG.md`). A kernel
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
    KernelCompat {
        id: "native-libBars",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "native-market-context",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
    KernelCompat {
        id: "native-libBuffers",
        eps: 1e-9,
        status: CompatStatus::ParityVerified,
    },
];

/// Native-library agreement rows: `(matrix id, publisher, name)` naming the
/// native-table library whose freeze agreement each `native-*` row claims.
/// These rows are NOT Expr-checked: their authority is native-vs-engine
/// agreement at the same frozen eps (see the freeze suite), while kernel
/// rows carry Expr-parity. Row ids are
/// frozen literals (the milestone's `native-libBars` for the ring/tick/
/// resample surface, `native-market-context` for the `MarketContext` view
/// over `libApex`, `native-libBuffers` for the `BufferFloat` hot methods);
/// the publisher/name pair is what the native table keys on, so coverage
/// checks join through this one mapping, never string surgery on the ids.
pub const NATIVE_MATRIX_ROWS: &[(&str, &str, &str)] = &[
    ("native-libBars", "showmethegrail", "libBars"),
    ("native-market-context", "showmethegrail", "libApex"),
    ("native-libBuffers", "showmethegrail", "libBuffers"),
];

/// Matrix id of the row claiming `publisher/name`, if any.
#[must_use]
pub fn native_row_id(publisher: &str, name: &str) -> Option<&'static str> {
    NATIVE_MATRIX_ROWS
        .iter()
        .find(|(_, pub_, name_)| *pub_ == publisher && *name_ == name)
        .map(|(id, _, _)| *id)
}

/// Whether a matrix id is a native-library row (served-view agreement, not a
/// kernel in the builtin registry).
#[must_use]
pub fn is_native_row(id: &str) -> bool {
    NATIVE_MATRIX_ROWS.iter().any(|(row, _, _)| *row == id)
}

/// Coverage for the native domain: every `native-*` row names a library in
/// the native table, and every native-table library has its row — no
/// orphans either way. Provider existence is orthogonal (the overlay owns
/// serving): rows also cover served views like the `MarketContext`, which
/// holds freeze agreement without a link-time provider. Eps rules match the
/// kernel side: finite and positive, never widened.
///
/// Needs the `pine` feature: it reads the native link table, which ships
/// with the language runtime.
#[cfg(feature = "pine")]
pub fn assert_native_matrix_covers() -> Result<(), String> {
    for (id, publisher, name) in NATIVE_MATRIX_ROWS {
        if !crate::native_libs::NATIVE_LIBRARIES
            .iter()
            .any(|entry| entry.publisher == *publisher && entry.name == *name)
        {
            return Err(format!("native row {id:?} names no native-table library"));
        }
        let row = compat_for(id).ok_or_else(|| format!("no compatibility row for {id:?}"))?;
        if !(row.eps.is_finite() && row.eps > 0.0) {
            return Err(format!("non-positive eps for {id:?}"));
        }
    }
    for entry in crate::native_libs::NATIVE_LIBRARIES {
        let id = native_row_id(entry.publisher, entry.name)
            .ok_or_else(|| format!("no parity row for {}/{}", entry.publisher, entry.name))?;
        if compat_for(id).is_none() {
            return Err(format!("no compatibility row for {id:?}"));
        }
    }
    Ok(())
}

/// Look up a kernel's compatibility row.
#[must_use]
pub fn compat_for(id: &str) -> Option<&'static KernelCompat> {
    COMPATIBILITY.iter().find(|row| row.id == id)
}

/// Ids the matrix must cover: every builtin kernel, no more, no less —
/// plus the native-library rows, which are checked against the native
/// table (not the kernel registry) by [`assert_native_matrix_covers`].
/// Without the `pine` feature the link table is absent, so only the kernel
/// rows are checked here; the native half stays covered by
/// `assert_native_matrix_covers` wherever the runtime ships.
pub fn assert_matrix_covers(registry: &IndicatorRegistry) -> Result<(), String> {
    for id in registry.ids() {
        let row = compat_for(&id).ok_or_else(|| format!("no compatibility row for {id:?}"))?;
        if !(row.eps.is_finite() && row.eps > 0.0) {
            return Err(format!("non-positive eps for {id:?}"));
        }
    }
    for row in COMPATIBILITY {
        if is_native_row(row.id) {
            continue;
        }
        if registry.get(row.id).is_none() {
            return Err(format!("stale compatibility row {:?}", row.id));
        }
    }
    #[cfg(feature = "pine")]
    assert_native_matrix_covers()?;
    Ok(())
}

#[cfg(test)]
#[path = "../tests/compat.rs"]
mod tests;

#[cfg(all(test, feature = "research"))]
#[path = "../tests/cal.rs"]
mod cal_tests;

#[cfg(all(test, feature = "research"))]
#[path = "../tests/cal_global.rs"]
mod cal_global_tests;
