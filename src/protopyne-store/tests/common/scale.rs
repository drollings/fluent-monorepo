//! Shared price-scale representability corpus.
//!
//! One home for the `(scale, value)` pairs that both the SQLite append path
//! and the sealed-snapshot writer must agree about, so the two representability
//! checks cannot drift: a value the store accepts but a snapshot refuses (or
//! vice versa) would be a silent inconsistency between the working tape and
//! its sealed artifact.
//!
//! Included once from `sqlite_tape.rs`; consumer suites `use` it, never
//! include it again. `dead_code` is allowed here so the single home stays
//! single without per-consumer shims.

#![allow(dead_code)]

/// One calibration case: a scale and the price text every OHLC leg carries.
pub struct FitCase {
    /// What the case proves.
    pub name: &'static str,
    /// Digits the market may carry.
    pub scale: u32,
    /// Price text for all four legs.
    pub value: &'static str,
}

/// Values that must round-trip byte-identically.
pub const FIT: &[FitCase] = &[
    FitCase {
        name: "cents",
        scale: 2,
        value: "100.55",
    },
    FitCase {
        name: "trailing zero is representation, not value",
        scale: 2,
        value: "100.50",
    },
    FitCase {
        name: "sub-cent",
        scale: 4,
        value: "100.0001",
    },
    FitCase {
        name: "crypto unit",
        scale: 8,
        value: "0.00000001",
    },
    FitCase {
        name: "crypto price",
        scale: 8,
        value: "61000.12345678",
    },
    FitCase {
        name: "whole-dollar",
        scale: 0,
        value: "100",
    },
    FitCase {
        name: "whole-dollar spelled with a point",
        scale: 0,
        value: "100.0",
    },
    FitCase {
        name: "decimal maximum",
        scale: 0,
        value: "79228162514264337593543950335",
    },
    FitCase {
        name: "top of the scale range",
        scale: 28,
        value: "1.1234567890123456789012345678",
    },
    FitCase {
        name: "one ulp below the boundary",
        scale: 2,
        value: "100.55",
    },
];

/// Values that must refuse: one digit past what the market carries.
pub const REFUSE: &[FitCase] = &[
    FitCase {
        name: "three digits at scale 2",
        scale: 2,
        value: "100.555",
    },
    FitCase {
        name: "one ulp above the boundary",
        scale: 2,
        value: "100.551",
    },
    FitCase {
        name: "tenths at scale 0",
        scale: 0,
        value: "100.1",
    },
    FitCase {
        name: "nine digits at scale 8",
        scale: 8,
        value: "0.000000001",
    },
];

/// The canonical stored spelling of a price text.
#[must_use]
pub fn canonical(text: &str) -> String {
    text.parse::<rust_decimal::Decimal>()
        .expect("calibration value parses")
        .normalize()
        .to_string()
}
