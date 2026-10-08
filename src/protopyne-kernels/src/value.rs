//! Pine scalar values: one copyable enum instead of three proxy types.
//!
//! Pine auto-casts `int` to `float` wherever a float is expected; no other
//! implicit conversion exists. [`PineValue::as_float`] encodes exactly that.

use std::fmt;

/// A Pine scalar. Absence is always `None` at the call site (Pine `na`),
/// never a sentinel value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PineValue {
    /// Pine `float`.
    Float(f64),
    /// Pine `int`; auto-casts to [`PineValue::Float`] on read.
    Int(i64),
    /// Pine `bool`.
    Bool(bool),
}

impl PineValue {
    /// Read as a float, applying Pine's int-to-float auto-cast.
    #[must_use]
    pub fn as_float(self) -> Option<f64> {
        match self {
            Self::Float(v) => Some(v),
            Self::Int(v) => Some(v as f64),
            Self::Bool(_) => None,
        }
    }

    /// Read as an int. Floats never implicitly truncate.
    #[must_use]
    pub fn as_int(self) -> Option<i64> {
        match self {
            Self::Int(v) => Some(v),
            Self::Float(_) | Self::Bool(_) => None,
        }
    }

    /// Read as a bool. Numbers are never truthy by accident.
    #[must_use]
    pub fn as_bool(self) -> Option<bool> {
        match self {
            Self::Bool(v) => Some(v),
            Self::Float(_) | Self::Int(_) => None,
        }
    }
}

impl fmt::Display for PineValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Float(v) => write!(f, "{v}"),
            Self::Int(v) => write!(f, "{v}"),
            Self::Bool(v) => write!(f, "{v}"),
        }
    }
}

#[cfg(test)]
#[path = "../tests/value.rs"]
mod tests;
