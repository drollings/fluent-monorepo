//! Candle timeframe vocabulary shared by ingest, kernels, and venues.
//!
//! Every variant is a fixed millisecond span: the [`Timeframe::duration_ms`]
//! table is the single authoritative span map (mirroring the Pine
//! `getTimeframeMS` values entry for entry), and bucket math elsewhere calls
//! it instead of keeping a second table. Calendar spans (month, quarter,
//! year) are not representable here by design — their length varies — so
//! every constructor refuses them fail-closed instead of approximating.
//! [`Timeframe::from_pine_abbrev`] is the single Pine↔Rust map, covering
//! exactly the abbreviations the Pine `getTimeframeAbbrev` emits for the
//! fixed-ms set.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::error::TypesError;

/// Fold anchor for every sub-weekly spacing: the Unix epoch itself.
pub const FOLD_ORIGIN_MS: i64 = 0;

/// Fold anchor for weekly bars: 1969-12-29 00:00:00 UTC, the Monday of the
/// epoch's week. The epoch itself is a Thursday, and a Thursday-anchored
/// weekly bar is confusing to hand an operator.
pub const WEEK_FOLD_ORIGIN_MS: i64 = -259_200_000;

/// Closed-bar timeframe. Covers the fixed-ms intraday-to-weekly grid the
/// prototype ingests and evaluates, seconds included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Timeframe {
    /// 5-second bars.
    S5,
    /// 10-second bars.
    S10,
    /// 15-second bars.
    S15,
    /// 30-second bars.
    S30,
    /// 1-minute bars.
    M1,
    /// 3-minute bars.
    M3,
    /// 5-minute bars.
    M5,
    /// 10-minute bars.
    M10,
    /// 15-minute bars.
    M15,
    /// 30-minute bars.
    M30,
    /// 1-hour bars.
    H1,
    /// 2-hour bars.
    H2,
    /// 4-hour bars.
    H4,
    /// Daily bars.
    D1,
    /// Weekly bars (7 fixed days).
    W1,
}

impl Timeframe {
    /// Bar span in whole minutes, truncating: sub-minute spans read 0 by
    /// construction. Exact spans live in [`duration_ms`](Self::duration_ms)
    /// and [`total_seconds`](Self::total_seconds); use those, not this, for
    /// bucket math.
    #[must_use]
    pub fn minutes(self) -> u32 {
        match self {
            Self::S5 | Self::S10 | Self::S15 | Self::S30 => 0,
            Self::M1 => 1,
            Self::M3 => 3,
            Self::M5 => 5,
            Self::M10 => 10,
            Self::M15 => 15,
            Self::M30 => 30,
            Self::H1 => 60,
            Self::H2 => 120,
            Self::H4 => 240,
            Self::D1 => 1440,
            Self::W1 => 10_080,
        }
    }

    /// Bar span in milliseconds (the spacing windows and fetch planning
    /// check gaps against). The authoritative span table: every value equals
    /// the Pine `getTimeframeMS` entry for the same timeframe,
    /// millisecond for millisecond.
    #[must_use]
    pub fn duration_ms(self) -> i64 {
        match self {
            Self::S5 => 5_000,
            Self::S10 => 10_000,
            Self::S15 => 15_000,
            Self::S30 => 30_000,
            Self::M1 => 60_000,
            Self::M3 => 180_000,
            Self::M5 => 300_000,
            Self::M10 => 600_000,
            Self::M15 => 900_000,
            Self::M30 => 1_800_000,
            Self::H1 => 3_600_000,
            Self::H2 => 7_200_000,
            Self::H4 => 14_400_000,
            Self::D1 => 86_400_000,
            Self::W1 => 604_800_000,
        }
    }

    /// Bar span as a time span.
    #[must_use]
    pub fn duration(self) -> chrono::Duration {
        chrono::Duration::milliseconds(self.duration_ms())
    }

    /// The fold anchor for a spacing. Only `W1` is special; any future
    /// fixed-ms spacing anchors at the epoch with no code change. This is
    /// the one home for the anchor rule: bucket math everywhere (the store
    /// fold, the gateway, the workspace) composes this, never a second
    /// table — except the kernels streaming projection, which stays
    /// epoch-anchored by construction and pins that divergence in its own
    /// tests.
    #[must_use]
    pub fn fold_origin_ms(self) -> i64 {
        match self {
            Self::W1 => WEEK_FOLD_ORIGIN_MS,
            _ => FOLD_ORIGIN_MS,
        }
    }

    /// The bucket a stamp belongs to: the greatest anchor-aligned grid line
    /// at or before the stamp.
    ///
    /// Pure Euclidean floor division on UTC milliseconds — negative stamps
    /// floor toward the earlier bucket, never toward zero — saturating
    /// instead of overflowing past the representable range. Idempotent:
    /// feeding a bucket start back in returns it unchanged, which is what
    /// makes incremental folding over a live tape converge instead of drift.
    #[must_use]
    pub fn bucket_start_ms(self, ts_ms: i64) -> i64 {
        let origin = self.fold_origin_ms();
        let step = self.duration_ms();
        origin.saturating_add(
            ts_ms
                .saturating_sub(origin)
                .div_euclid(step)
                .saturating_mul(step),
        )
    }

    /// Bar span in whole seconds. Every span is a whole number of seconds,
    /// so this is exact (unlike [`minutes`](Self::minutes), which truncates
    /// sub-minute spans).
    #[must_use]
    pub fn total_seconds(self) -> u64 {
        match self {
            Self::S5 => 5,
            Self::S10 => 10,
            Self::S15 => 15,
            Self::S30 => 30,
            Self::M1 => 60,
            Self::M3 => 180,
            Self::M5 => 300,
            Self::M10 => 600,
            Self::M15 => 900,
            Self::M30 => 1_800,
            Self::H1 => 3_600,
            Self::H2 => 7_200,
            Self::H4 => 14_400,
            Self::D1 => 86_400,
            Self::W1 => 604_800,
        }
    }

    /// Whether this span is a fixed millisecond grid. True for every
    /// constructible variant: calendar spans are refused at the boundary
    /// (see [`from_pine_abbrev`](Self::from_pine_abbrev)), never built and
    /// approximated. The per-variant arms force a deliberate decision if a
    /// non-fixed span is ever added.
    #[must_use]
    pub fn is_fixed_ms(self) -> bool {
        match self {
            Self::S5
            | Self::S10
            | Self::S15
            | Self::S30
            | Self::M1
            | Self::M3
            | Self::M5
            | Self::M10
            | Self::M15
            | Self::M30
            | Self::H1
            | Self::H2
            | Self::H4
            | Self::D1
            | Self::W1 => true,
        }
    }

    /// Whether `self` spans a whole number of `base` bars with no remainder:
    /// the one whole-multiple check behind load-time folding, read planning,
    /// and toolbar group math. Equal spacings are whole multiples of each
    /// other; anything finer or non-multiple is not.
    #[must_use]
    pub fn is_whole_multiple_of(self, base: Self) -> bool {
        self.duration_ms() % base.duration_ms() == 0
    }

    /// Parse a toolbar label (`1M`/`5M`/`15M`/`1H`/`4H`/`1D`): the one
    /// toolbar-label bridge. Other chart spellings ride
    /// [`from_resolution`](Self::from_resolution); anything else is an
    /// unknown label, not a guess. The `1M`-style codes read as months on a
    /// data chart, so they are toolbar labels first and never resolve as
    /// months here.
    #[must_use]
    pub fn from_chart_label(label: &str) -> Option<Self> {
        match label {
            "1M" => Some(Self::M1),
            "5M" => Some(Self::M5),
            "15M" => Some(Self::M15),
            "1H" => Some(Self::H1),
            "4H" => Some(Self::H4),
            "1D" => Some(Self::D1),
            other => Self::from_resolution(other),
        }
    }

    /// Parse a Pine abbreviation from `getTimeframeAbbrev`
    /// (`5S`/`10S`/`15S`/`30S`/`1`/`3`/`5`/`10`/`15`/`30`/`60`/`120`/`240`/
    /// `D`/`W`). The single Pine↔Rust map: no second abbrev table may be
    /// introduced elsewhere. Anything else — calendar abbrevs (`1M`/`3M`/
    /// `12M`), chart sentinels (`C`, `-`), unknown strings — refuses via
    /// [`TypesError::UnknownTimeframe`], never approximates.
    pub fn from_pine_abbrev(abbrev: &str) -> Result<Self, TypesError> {
        match abbrev {
            "5S" => Ok(Self::S5),
            "10S" => Ok(Self::S10),
            "15S" => Ok(Self::S15),
            "30S" => Ok(Self::S30),
            "1" => Ok(Self::M1),
            "3" => Ok(Self::M3),
            "5" => Ok(Self::M5),
            "10" => Ok(Self::M10),
            "15" => Ok(Self::M15),
            "30" => Ok(Self::M30),
            "60" => Ok(Self::H1),
            "120" => Ok(Self::H2),
            "240" => Ok(Self::H4),
            "D" => Ok(Self::D1),
            "W" => Ok(Self::W1),
            _ => Err(TypesError::UnknownTimeframe(abbrev.to_owned())),
        }
    }

    /// Parse a charting resolution code (`5S`/`15`/`3`/`10`/`120`/`W`,
    /// plus the `1H`/`2H`/`4H`/`1D`/`D`/`1W` slang). Anything else is an
    /// unknown timeframe, not a guess. Month-suffixed codes (`1M`, `3M`)
    /// stay refused: on a chart they mean months, which this grid cannot
    /// represent.
    #[must_use]
    pub fn from_resolution(resolution: &str) -> Option<Self> {
        match resolution {
            "1H" => Some(Self::H1),
            "2H" => Some(Self::H2),
            "4H" => Some(Self::H4),
            "1D" | "D" => Some(Self::D1),
            "1W" => Some(Self::W1),
            _ => Self::from_pine_abbrev(resolution).ok(),
        }
    }

    /// Canonical short code (`M1`, `S5`, `H2`, `W1`, …).
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::S5 => "S5",
            Self::S10 => "S10",
            Self::S15 => "S15",
            Self::S30 => "S30",
            Self::M1 => "M1",
            Self::M3 => "M3",
            Self::M5 => "M5",
            Self::M10 => "M10",
            Self::M15 => "M15",
            Self::M30 => "M30",
            Self::H1 => "H1",
            Self::H2 => "H2",
            Self::H4 => "H4",
            Self::D1 => "D1",
            Self::W1 => "W1",
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
            "S5" => Ok(Self::S5),
            "S10" => Ok(Self::S10),
            "S15" => Ok(Self::S15),
            "S30" => Ok(Self::S30),
            "M1" => Ok(Self::M1),
            "M3" => Ok(Self::M3),
            "M5" => Ok(Self::M5),
            "M10" => Ok(Self::M10),
            "M15" => Ok(Self::M15),
            "M30" => Ok(Self::M30),
            "H1" => Ok(Self::H1),
            "H2" => Ok(Self::H2),
            "H4" => Ok(Self::H4),
            "D1" => Ok(Self::D1),
            "W1" => Ok(Self::W1),
            _ => Err(TypesError::UnknownTimeframe(s.to_owned())),
        }
    }
}

#[cfg(test)]
#[path = "../tests/timeframe.rs"]
mod tests;
