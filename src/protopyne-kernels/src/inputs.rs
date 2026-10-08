//! Validated per-run inputs: declared bounds, typed reads, checked writes.
//!
//! Every input is declared with its kind, default, and admissible range up
//! front. Declaration rejects inverted ranges, out-of-range defaults, and
//! non-finite float bounds; assignment rejects unknown names, kind
//! mismatches, and out-of-range values. Nothing here panics.

use std::collections::HashMap;

use crate::error::KernelError;
use crate::value::PineValue;

/// An input declaration with its admissible range.
#[derive(Debug, Clone, PartialEq)]
pub enum InputDef {
    /// Float input with inclusive `[min, max]`.
    Float {
        /// Value used until overwritten.
        default: f64,
        /// Inclusive lower bound (must be finite).
        min: f64,
        /// Inclusive upper bound (must be finite, `>= min`).
        max: f64,
    },
    /// Int input with inclusive `[min, max]`.
    Int {
        /// Value used until overwritten.
        default: i64,
        /// Inclusive lower bound.
        min: i64,
        /// Inclusive upper bound (`>= min`).
        max: i64,
    },
    /// Bool input (no range).
    Bool {
        /// Value used until overwritten.
        default: bool,
    },
}

impl InputDef {
    fn default_value(&self) -> PineValue {
        match *self {
            Self::Float { default, .. } => PineValue::Float(default),
            Self::Int { default, .. } => PineValue::Int(default),
            Self::Bool { default } => PineValue::Bool(default),
        }
    }

    fn check(&self, value: PineValue) -> Result<(), KernelError> {
        match self {
            Self::Float { min, max, .. } => match value {
                PineValue::Float(v) if v.is_finite() && v >= *min && v <= *max => Ok(()),
                PineValue::Float(v) => Err(KernelError::InvalidInput(format!(
                    "float {v} outside [{min}, {max}]"
                ))),
                got => Err(KernelError::InvalidInput(format!(
                    "kind mismatch: float input got {got}"
                ))),
            },
            Self::Int { min, max, .. } => match value {
                PineValue::Int(v) if v >= *min && v <= *max => Ok(()),
                PineValue::Int(v) => Err(KernelError::InvalidInput(format!(
                    "int {v} outside [{min}, {max}]"
                ))),
                got => Err(KernelError::InvalidInput(format!(
                    "kind mismatch: int input got {got}"
                ))),
            },
            Self::Bool { .. } => match value {
                PineValue::Bool(_) => Ok(()),
                got => Err(KernelError::InvalidInput(format!(
                    "kind mismatch: bool input got {got}"
                ))),
            },
        }
    }

    fn validate(&self) -> Result<(), KernelError> {
        match *self {
            Self::Float { default, min, max } => {
                if !default.is_finite() || !min.is_finite() || !max.is_finite() {
                    return Err(KernelError::InvalidInput(
                        "float input bounds and default must be finite".to_owned(),
                    ));
                }
                if min > max {
                    return Err(KernelError::InvalidInput(format!(
                        "float input min {min} exceeds max {max}"
                    )));
                }
                if default < min || default > max {
                    return Err(KernelError::InvalidInput(format!(
                        "float default {default} outside [{min}, {max}]"
                    )));
                }
                Ok(())
            }
            Self::Int { default, min, max } => {
                if min > max {
                    return Err(KernelError::InvalidInput(format!(
                        "int input min {min} exceeds max {max}"
                    )));
                }
                if default < min || default > max {
                    return Err(KernelError::InvalidInput(format!(
                        "int default {default} outside [{min}, {max}]"
                    )));
                }
                Ok(())
            }
            Self::Bool { .. } => Ok(()),
        }
    }
}

/// Declared inputs and their current per-run values.
#[derive(Debug, Clone, Default)]
pub struct InputSet {
    defs: HashMap<String, InputDef>,
    values: HashMap<String, PineValue>,
}

impl InputSet {
    /// Create an empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare an input and seed it with its default. Duplicate names and
    /// invalid bounds are errors.
    pub fn declare(&mut self, name: impl Into<String>, def: InputDef) -> Result<(), KernelError> {
        def.validate()?;
        let name = name.into();
        if self.defs.contains_key(&name) {
            return Err(KernelError::InvalidInput(format!(
                "duplicate input declaration: {name:?}"
            )));
        }
        self.values.insert(name.clone(), def.default_value());
        self.defs.insert(name, def);
        Ok(())
    }

    /// Read the current value; undeclared names are `None`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<PineValue> {
        self.values.get(name).copied()
    }

    /// Read a float input; wrong kinds and undeclared names are `None`.
    #[must_use]
    pub fn get_float(&self, name: &str) -> Option<f64> {
        self.get(name).and_then(PineValue::as_float)
    }

    /// Read an int input; wrong kinds and undeclared names are `None`.
    #[must_use]
    pub fn get_int(&self, name: &str) -> Option<i64> {
        self.get(name).and_then(PineValue::as_int)
    }

    /// Read a bool input; wrong kinds and undeclared names are `None`.
    #[must_use]
    pub fn get_bool(&self, name: &str) -> Option<bool> {
        self.get(name).and_then(PineValue::as_bool)
    }

    /// Assign an input after bounds and kind checks.
    pub fn set(&mut self, name: &str, value: PineValue) -> Result<(), KernelError> {
        let def = self.defs.get(name).ok_or_else(|| {
            KernelError::InvalidInput(format!("undeclared input: {name:?}"))
        })?;
        def.check(value)?;
        self.values.insert(name.to_owned(), value);
        Ok(())
    }
}

#[cfg(test)]
#[path = "../tests/inputs.rs"]
mod tests;
