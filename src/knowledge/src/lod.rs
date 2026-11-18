//! Level of detail, 0..=5 (VISION table).
//!
//! Moved verbatim from `fluent-router`'s `views.rs` (M10.2): the level
//! identifier is LOD mechanics, owned here; the `u8` level constants stay in
//! `fluent-types` (pure data, next to `LOD_COUNT`) and are re-exported below
//! for call sites that already import the level from this module.

pub use fluent_types::{LAZY_LOD_RANGE, LOD0_FULL_TEXT, LOD5_LABEL};

/// Level of detail, 0..=5 (VISION table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Lod(u8);

impl Lod {
    /// Full text.
    pub const LOD0: Lod = Lod(0);
    /// Compressed but complete.
    pub const LOD1: Lod = Lod(1);
    /// Short summary ≤ 1000 ch.
    pub const LOD2: Lod = Lod(2);
    /// Compact summary ≤ 280 ch.
    pub const LOD3: Lod = Lod(3);
    /// Single line ≤ 80 ch.
    pub const LOD4: Lod = Lod(4);
    /// Name / label.
    pub const LOD5: Lod = Lod(5);

    /// The tier as a `u8` (for store `lod_text` calls).
    pub fn as_u8(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for Lod {
    type Error = u8;

    fn try_from(level: u8) -> Result<Self, Self::Error> {
        match level {
            0..=5 => Ok(Lod(level)),
            other => Err(other),
        }
    }
}

impl std::fmt::Display for Lod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LOD{}", self.0)
    }
}

#[cfg(test)]
#[path = "../tests/lod.rs"]
mod tests;
