//! Deterministic ordering: the single canonical spelling for the
//! workspace-wide "collect → sort → dedup/snapshot" discipline.
//!
//! All three helpers sort owned vectors and return them, so call sites read
//! as a pipeline (`let ordered = sorted_vec(collected)`) instead of a
//! mutable `collect`/`sort`/`dedup` stanza. Sorts are stable (`slice::sort`
//! family) — equal keys keep insertion order, pinned by test.
//!
//! Score-ordered sorts (float rank + id tiebreak, `partial_cmp` fallback to
//! `Equal`) are deliberately NOT enshrined here: NaN/epsilon policy belongs
//! to the scoring crate (`search-vector::fusion`), not a generic helper.
//! The documented recipe lives in `tests/sort.rs`
//! (`sort_by_score_then_id`); call sites keep their own closures.
//!
//! # Examples
//!
//! ```
//! use common_core::sort::{dedup_sorted, sorted_by_vec, sorted_vec};
//!
//! assert_eq!(sorted_vec(vec![3, 1, 2]), vec![1, 2, 3]);
//! assert_eq!(dedup_sorted(vec![2, 1, 2]), vec![1, 2]);
//! assert_eq!(
//!     sorted_by_vec(vec!["bb", "a", "ccc"], |x, y| x.len().cmp(&y.len())),
//!     vec!["a", "bb", "ccc"]
//! );
//! ```

use std::cmp::Ordering;

/// Sort an owned vector in ascending order and return it.
#[must_use]
pub fn sorted_vec<T: Ord>(mut values: Vec<T>) -> Vec<T> {
    values.sort();
    values
}

/// Sort an owned vector with a custom comparator and return it (stable:
/// equal keys keep insertion order).
#[must_use]
pub fn sorted_by_vec<T>(
    mut values: Vec<T>,
    compare: impl FnMut(&T, &T) -> Ordering,
) -> Vec<T> {
    values.sort_by(compare);
    values
}

/// Sort an owned vector, drop duplicates, and return it.
#[must_use]
pub fn dedup_sorted<T: Ord>(mut values: Vec<T>) -> Vec<T> {
    values.sort();
    values.dedup();
    values
}
