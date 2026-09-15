//! Poison-safe locking for `std::sync` primitives — a re-export surface,
//! not a reimplementation.
//!
//! Canonical home is `common_core::sync` (this crate's own `pool`, `queue`,
//! and `affinity` modules compose it through this path). Recover from
//! poisoning via `into_inner` instead of panicking: a panic while a lock is
//! held must not wedge the holder permanently.

pub use common_core::sync::{lock, lock_read, lock_write};
