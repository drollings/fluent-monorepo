//! Candle timeframe vocabulary shared by ingest, kernels, and venues.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::error::TypesError;

/// Closed-bar timeframe. Covers the intraday-to-daily grid the prototype
/// ingests and evaluates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Timeframe {
    /// 1-minute bars.
    M1,
    /// 5-minute bars.
    M5,
    /// 15-minute bars.
    M15,
    /// 30-minute bars.
    M30,
    /// 1-hour bars.
    H1,
    /// 4-hour bars.
    H4,
    /// Daily bars.
    D1,
}

impl Timeframe {
    /// Bar span in minutes.
    #[must_use]
    pub fn minutes(self) -> u32 {
        match self {
            Self::M1 => 1,
            Self::M5 => 5,
            Self::M15 => 15,
            Self::M30 => 30,
            Self::H1 => 60,
            Self::H4 => 240,
            Self::D1 => 1440,
        }
    }

    /// Canonical short code (`M1`, `H1`, …).
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::M1 => "M1",
            Self::M5 => "M5",
            Self::M15 => "M15",
            Self::M30 => "M30",
            Self::H1 => "H1",
            Self::H4 => "H4",
            Self::D1 => "D1",
        }
    }
}

impl fmt::Display for Timeframe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl FromStr for Timeframe {
    type Err = TypesError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "M1" => Ok(Self::M1),
            "M5" => Ok(Self::M5),
            "M15" => Ok(Self::M15),
            "M30" => Ok(Self::M30),
            "H1" => Ok(Self::H1),
            "H4" => Ok(Self::H4),
            "D1" => Ok(Self::D1),
            _ => Err(TypesError::UnknownTimeframe(s.to_owned())),
        }
    }
}

#[cfg(test)]
#[path = "../tests/timeframe.rs"]
mod tests;
