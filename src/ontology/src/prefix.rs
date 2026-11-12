//! Well-known CURIE prefixes behind a closed enum.
//!
//! `yago` and `schema` are the two closed namespaces the loader, the view,
//! and the plausibility fallback all branch on. Every other prefix (arbitrary
//! TTL `@prefix` declarations, operator vocabularies) stays open by
//! necessity: it is carried verbatim in the `Other` variant so no caller
//! silently drops an unknown namespace.

#![forbid(unsafe_code)]

use std::convert::Infallible;
use std::fmt;
use std::str::FromStr;

/// IRI base for the schema.org namespace.
pub const SCHEMA_IRI_BASE: &str = "http://schema.org/";
/// IRI base for the YaGO resource namespace.
pub const YAGO_IRI_BASE: &str = "http://yago-knowledge.org/resource/";

/// Closed prefix set: the two well-known namespaces plus an open fallback
/// carrying the original prefix text for genuinely open key sets (TTL
/// `@prefix` declarations, operator vocabularies).
#[derive(Debug, Clone, PartialEq, Eq)]
/// Closed prefix set: the two well-known namespaces plus an open fallback
/// carrying the original prefix text for genuinely open key sets (TTL
/// `@prefix` declarations, operator vocabularies).
pub enum CuriePrefix {
    /// The `yago:` namespace.
    Yago,
    /// The `schema:` namespace.
    Schema,
    /// Any other prefix, carried verbatim.
    Other(String),
}

impl CuriePrefix {
    /// The canonical lowercase prefix text (`yago`, `schema`, or the stored
    /// fallback).
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            CuriePrefix::Yago => "yago",
            CuriePrefix::Schema => "schema",
            CuriePrefix::Other(text) => text,
        }
    }

    /// Look up the closed variant for a lowercase prefix, if it is one of
    /// the well-known namespaces.
    fn closed(prefix: &str) -> Option<CuriePrefix> {
        match prefix {
            "yago" => Some(CuriePrefix::Yago),
            "schema" => Some(CuriePrefix::Schema),
            _ => None,
        }
    }
}

impl fmt::Display for CuriePrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for CuriePrefix {
    type Err = Infallible;

    /// Never fails: unknown text maps to the open fallback, lowercased so
    /// `YAGO` and `yago` name the same closed variant and unknown prefixes
    /// compare consistently.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let lowered = text.to_lowercase();
        Ok(CuriePrefix::closed(&lowered).unwrap_or(CuriePrefix::Other(lowered)))
    }
}

/// Split a CURIE at the first `:` into its prefix and remainder without
/// allocating the remainder.
///
/// Returns `None` when the input contains no `:`. The prefix half is mapped
/// through the closed set (`yago`/`schema`, case-insensitive); anything else
/// becomes the open fallback carrying the lowercased prefix text.
#[must_use]
/// Split a CURIE at the first `:` into its prefix and remainder without
/// allocating the remainder.
/// 
/// Returns `None` when the input contains no `:`. The prefix half is mapped
/// through the closed set (`yago`/`schema`, case-insensitive); anything else
/// becomes the open fallback carrying the lowercased prefix text.
pub fn split_prefix(curie: &str) -> Option<(CuriePrefix, &str)> {
    let (raw_prefix, rest) = curie.split_once(':')?;
    let lowered = raw_prefix.to_lowercase();
    let prefix = CuriePrefix::closed(&lowered).unwrap_or(CuriePrefix::Other(lowered));
    Some((prefix, rest))
}

#[cfg(test)]
#[path = "../tests/prefix.rs"]
mod tests;