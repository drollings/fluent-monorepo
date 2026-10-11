//! Pine scalar values: one owned enum instead of three proxy types.
//!
//! Pine auto-casts `int` to `float` wherever a float is expected; no other
//! implicit conversion exists. [`PineValue::as_float`] encodes exactly that,
//! and the readers refuse every other mismatch — numbers are never truthy,
//! floats never truncate, and display handles never read as numbers.
//!
//! Absence (Pine `na`) is always `None` at the call site, never a sentinel;
//! IEEE `NaN` inside `f64` legs means the same absence and maps back to
//! `None` at the output boundary via `fin_or_none`. [`nz`] fills absence
//! with a default and [`fixnan`] forward-fills a leg without ever
//! resurrecting a leading warmup prefix.

use std::fmt;

/// A Pine scalar. Old numeric variants keep their exact reader behavior;
/// `Str` and the opaque display handles travel beside numerics and never
/// enter arithmetic. `Array` and `IntMap` are structural values for the
/// script collection namespaces: elements may hold absence (`None`), and
/// neither reads as a number, an int, or a bool.
#[derive(Debug, Clone, PartialEq)]
pub enum PineValue {
    /// Pine `float`.
    Float(f64),
    /// Pine `int`; auto-casts to [`PineValue::Float`] on read.
    Int(i64),
    /// Pine `bool`.
    Bool(bool),
    /// Pine `string` (owned; produced by `str.*`, consumed by plots/labels).
    Str(String),
    /// Opaque display color (`color.*` lowers here; never arithmetic).
    ColorHandle(ColorHandle),
    /// Opaque enum member (`UTIL.Smoothings.WMA` lowers here).
    EnumHandle(EnumHandle),
    /// Pine array: ordered elements with absence per slot. Collection
    /// operators return new arrays (functional); nothing mutates in place.
    Array(Vec<Option<PineValue>>),
    /// Pine `map<int, int>`: the scheduler-state shape. Missing keys read
    /// as absence at the call site, never as a default.
    IntMap(std::collections::HashMap<i64, i64>),
    /// Owned user-type instance (`type` declarations with `method`
    /// dispatch). Fields address by name; instances never read as numbers,
    /// strings, or bools.
    Object(UserInstance),
}

/// Opaque display-color token. The inner packing is decided by the lowering
/// stage that mints it; comparison and display never inspect it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColorHandle(u32);

impl ColorHandle {
    /// Mint a handle from the lowering stage's packed representation.
    #[must_use]
    pub fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// The packed representation, for the lowering stage only.
    #[must_use]
    pub fn raw(self) -> u32 {
        self.0
    }
}

/// Opaque enum-member token: which enum and which member, as declared.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EnumHandle {
    /// Declaring enum (`Smoothings`).
    pub enum_name: String,
    /// Member name (`WMA`).
    pub member: String,
}

impl EnumHandle {
    /// Name one enum member.
    #[must_use]
    pub fn new(enum_name: &str, member: &str) -> Self {
        Self {
            enum_name: enum_name.to_owned(),
            member: member.to_owned(),
        }
    }
}

impl fmt::Display for EnumHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.enum_name, self.member)
    }
}

/// Owned user-type instance: which declared type plus its field bindings
/// (`None` fields are Pine `na`). Methods receive the instance by mutable
/// reference through the executor; the value itself stays a plain cloneable
/// datum like every other variant.
#[derive(Debug, Clone, PartialEq)]
pub struct UserInstance {
    /// Declared type name (`Scheduler`).
    pub type_name: String,
    /// Field bindings in no particular order.
    pub fields: std::collections::HashMap<String, Option<PineValue>>,
}

impl UserInstance {
    /// Build an instance from evaluated field bindings.
    #[must_use]
    pub fn new(
        type_name: &str,
        fields: std::collections::HashMap<String, Option<PineValue>>,
    ) -> Self {
        Self {
            type_name: type_name.to_owned(),
            fields,
        }
    }
}

impl PineValue {
    /// Read as a float, applying Pine's int-to-float auto-cast.
    #[must_use]
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Self::Float(v) => Some(*v),
            Self::Int(v) => Some(*v as f64),
            Self::Bool(_)
            | Self::Str(_)
            | Self::ColorHandle(_)
            | Self::EnumHandle(_)
            | Self::Array(_)
            | Self::IntMap(_)
            | Self::Object(_) => None,
        }
    }

    /// Read as an int. Floats never implicitly truncate.
    #[must_use]
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(v) => Some(*v),
            Self::Float(_)
            | Self::Bool(_)
            | Self::Str(_)
            | Self::ColorHandle(_)
            | Self::EnumHandle(_)
            | Self::Array(_)
            | Self::IntMap(_)
            | Self::Object(_) => None,
        }
    }

    /// Read as a bool. Numbers are never truthy by accident.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(v) => Some(*v),
            Self::Float(_)
            | Self::Int(_)
            | Self::Str(_)
            | Self::ColorHandle(_)
            | Self::EnumHandle(_)
            | Self::Array(_)
            | Self::IntMap(_)
            | Self::Object(_) => None,
        }
    }
}

/// Pine `nz(value, default)`: absence (`None`) becomes `default`, presence
/// passes through untouched.
#[must_use]
pub fn nz(value: Option<PineValue>, default: PineValue) -> PineValue {
    value.unwrap_or(default)
}

/// Pine `fixnan`: forward-fill absence with the last present value. A `NaN`
/// leg entry counts as absent (legs use `NaN` for `na`). Leading absence —
/// the warmup prefix — stays absent; nothing is resurrected.
#[must_use]
pub fn fixnan(series: &[Option<f64>]) -> Vec<Option<f64>> {
    let mut last: Option<f64> = None;
    series
        .iter()
        .map(|slot| match slot {
            Some(v) if !v.is_nan() => {
                last = Some(*v);
                Some(*v)
            }
            _ => last,
        })
        .collect()
}

impl fmt::Display for PineValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Float(v) => write!(f, "{v}"),
            Self::Int(v) => write!(f, "{v}"),
            Self::Bool(v) => write!(f, "{v}"),
            Self::Str(v) => write!(f, "{v}"),
            Self::ColorHandle(v) => write!(f, "color({})", v.raw()),
            Self::EnumHandle(v) => write!(f, "{v}"),
            Self::Object(instance) => write!(f, "{} instance", instance.type_name),
            Self::Array(items) => {
                let mut first = true;
                write!(f, "[")?;
                for item in items {
                    if !first {
                        write!(f, ", ")?;
                    }
                    first = false;
                    match item {
                        Some(value) => write!(f, "{value}")?,
                        None => write!(f, "na")?,
                    }
                }
                write!(f, "]")
            }
            Self::IntMap(entries) => {
                let mut keys: Vec<i64> = entries.keys().copied().collect();
                keys.sort_unstable();
                let mut first = true;
                write!(f, "{{")?;
                for key in keys {
                    if !first {
                        write!(f, ", ")?;
                    }
                    first = false;
                    write!(f, "{key}: {}", entries[&key])?;
                }
                write!(f, "}}")
            }
        }
    }
}

#[cfg(test)]
#[path = "../tests/value.rs"]
mod tests;
