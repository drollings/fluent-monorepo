//! Option-mark basis: where a position's per-unit mark came from.
//!
//! A foundation vocabulary (strategy names it in its mark context, the
//! server names it in its reports), so it lives here rather than in either
//! consumer. The wire tags are frozen — reports and view rows serialize the
//! variant names — so variants are never renamed or reordered.

/// Where a position's per-unit mark came from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MarkBasis {
    /// Every leg answered with a point-in-time chain mid.
    ChainMid,
    /// The position's own executed prices (the chain was dark or absent).
    #[default]
    FillPrice,
    /// No mark source answered: unrealized is absent, never zero.
    Absent,
}

impl MarkBasis {
    /// The stable wire tag carried in reports and view rows.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChainMid => "ChainMid",
            Self::FillPrice => "FillPrice",
            Self::Absent => "Absent",
        }
    }

    /// `true` when no mark exists, so unrealized must report `None`.
    #[must_use]
    pub const fn is_absent(self) -> bool {
        matches!(self, Self::Absent)
    }

    /// Preference rank: lower is preferred by the `ChainMid → FillPrice →
    /// Absent` order.
    #[must_use]
    const fn rank(self) -> u8 {
        match self {
            Self::ChainMid => 0,
            Self::FillPrice => 1,
            Self::Absent => 2,
        }
    }

    /// The more preferred of two bases.
    #[must_use]
    pub const fn preferred(self, other: Self) -> Self {
        if self.rank() <= other.rank() {
            self
        } else {
            other
        }
    }
}
