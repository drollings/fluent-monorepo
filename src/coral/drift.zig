//! drift.zig — Re-exports BitSet DRIFT from src/common/drift.zig.
//!
//! The canonical implementation lives in `src/common/drift.zig` so it can be
//! shared with guidance. Coral code continues to `@import("drift")` unchanged.

const common_drift = @import("common").drift;

pub const BitSetDrift = common_drift.BitSetDrift;
