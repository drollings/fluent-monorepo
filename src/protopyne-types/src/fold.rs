//! The complete-bucket rule, one home.
//!
//! A bucket is whole when its children tile the bucket grid contiguously:
//! the first child opens the bucket, every child gap is the same span, and
//! the children count out the full bucket span. Anything else — a short
//! leading or trailing edge, a torn interior bucket, a lone child that cannot
//! evidence a full bucket — is not whole. [`FoldEdges`] names the policy;
//! [`BucketAccum`] carries the O(1) per-bucket completeness state behind it,
//! so bucketing never allocates a per-bucket stamp vector on any policy.
//! Decision code folds [`Bar`](crate::Bar) values through the store;
//! display code folds display rows through the same rule in the terminal.

/// How a fold treats bucket edges. The policy rides the fold call itself:
/// a range-free fold takes no window, so edge treatment is the caller's
/// stated choice, not an ambient default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
/// The complete-bucket rule, one home.
/// 
/// A bucket is whole when its children tile the bucket grid contiguously:
/// the first child opens the bucket, every child gap is the same span, and
/// the children count out the full bucket span. Anything else — a short
/// leading or trailing edge, a torn interior bucket, a lone child that cannot
/// evidence a full bucket — is not whole. [`FoldEdges`] names the policy;
/// [`BucketAccum`] carries the O(1) per-bucket completeness state behind it,
/// so bucketing never allocates a per-bucket stamp vector on any policy.
/// Decision code folds [`Bar`](crate::Bar) values through the store;
/// display code folds display rows through the same rule in the terminal.
/// How a fold treats bucket edges. The policy rides the fold call itself:
/// a range-free fold takes no window, so edge treatment is the caller's
/// stated choice, not an ambient default.
pub enum FoldEdges {
    /// Every bucket holding at least one child emits, partial edges
    /// included. The legacy behavior; the default, so existing callers keep
    /// their exact output.
    #[default]
    KeepPartial,
    /// A bucket emits only when its children tile the whole bucket grid
    /// contiguously: the first child opens the bucket, every child gap is
    /// the same span, and the children count out the full bucket span.
    /// Anything else — a short leading or trailing edge, a torn interior
    /// bucket, a lone child that cannot evidence a full bucket — is
    /// omitted, never emitted torn or partial.
    CompleteOnly,
}

/// O(1) completeness state for one bucket under construction: the bucket
/// start, the first and last child stamps, the child count, and the uniform
/// child gap (or a diverged flag once two gaps disagree). The gap is read
/// off the children, never assumed from a spacing the fold does not take:
/// any uniform tiling of the bucket counts as complete, whatever the source
/// spacing was.
#[derive(Debug, Clone, Copy)]
/// O(1) completeness state for one bucket under construction: the bucket
/// start, the first and last child stamps, the child count, and the uniform
/// child gap (or a diverged flag once two gaps disagree). The gap is read
/// off the children, never assumed from a spacing the fold does not take:
/// any uniform tiling of the bucket counts as complete, whatever the source
/// spacing was.
pub struct BucketAccum {
    start_ms: i64,
    count: u64,
    first_ms: i64,
    last_ms: i64,
    gap_ms: i64,
    uniform: bool,
}

impl BucketAccum {
    /// Open the completeness state for the bucket starting at `start_ms`.
    /// No children have been seen; the bucket is not complete.
    #[must_use]
    pub fn new(start_ms: i64) -> Self {
        Self {
            start_ms,
            count: 0,
            first_ms: start_ms,
            last_ms: start_ms,
            gap_ms: 0,
            uniform: true,
        }
    }

    /// Absorb one ascending child stamp. Strictly ascending input is the
    /// caller's contract (the folds validate it); a non-positive gap simply
    /// reads as non-uniform, never as a complete tiling.
    pub fn absorb(&mut self, stamp_ms: i64) {
        if self.count == 0 {
            self.first_ms = stamp_ms;
            self.last_ms = stamp_ms;
            self.count = 1;
            return;
        }
        let gap = stamp_ms - self.last_ms;
        if self.count == 1 {
            self.gap_ms = gap;
        } else if gap != self.gap_ms {
            self.uniform = false;
        }
        self.last_ms = stamp_ms;
        self.count = self.count.saturating_add(1);
    }

    /// Whether the absorbed children tile the `step_ms` bucket whole: at
    /// least two children, opening on the bucket start, with one uniform
    /// positive gap counting out the full span.
    #[must_use]
    pub fn is_complete(&self, step_ms: i64) -> bool {
        if self.count < 2 || !self.uniform {
            return false;
        }
        if self.first_ms != self.start_ms {
            return false;
        }
        if self.gap_ms <= 0 {
            return false;
        }
        (self.count as i64).checked_mul(self.gap_ms) == Some(step_ms)
    }
}

#[cfg(test)]
#[path = "../tests/fold.rs"]
mod tests;