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

    /// Bar span in milliseconds (the spacing windows and fetch planning
    /// check gaps against).
    #[must_use]
    pub fn duration_ms(self) -> i64 {
        i64::from(self.minutes()) * 60_000
    }

    /// Bar span as a time span.
    #[must_use]
    pub fn duration(self) -> chrono::Duration {
        chrono::Duration::milliseconds(self.duration_ms())
    }

    /// Bar span in whole seconds.
    #[must_use]
    pub fn total_seconds(self) -> u64 {
        u64::from(self.minutes()) * 60
    }

    /// Parse a charting resolution code (`1`, `5`, `15`, `30`, `60`/`1H`,
    /// `240`/`4H`, `1D`/`D`). Anything else is an unknown timeframe, not a
    /// guess.
    #[must_use]
    pub fn from_resolution(resolution: &str) -> Option<Self> {
        match resolution {
            "1" => Some(Self::M1),
            "5" => Some(Self::M5),
            "15" => Some(Self::M15),
            "30" => Some(Self::M30),
            "60" | "1H" => Some(Self::H1),
            "240" | "4H" => Some(Self::H4),
            "1D" | "D" => Some(Self::D1),
            _ => None,
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
