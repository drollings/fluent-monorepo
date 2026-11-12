//! Typed store failures with an explicit retry taxonomy.
//!
//! Every failure a caller can act on gets its own variant, because the
//! three-way distinction the platform depends on — *this span is not
//! available*, *try again*, *this input is wrong* — collapses the moment it
//! is flattened into one error string. A refused read is permanent, a locked
//! database is retryable, and a malformed bar is a caller bug: none of the
//! three is improved by trying again.
//!
//! Nothing here panics or substitutes a value. A refusal always carries the
//! reason it refused.

use common_core::error::IoError;
use fluent_wvr::WorkError;
use protopyne_types::Timeframe;
use thiserror::Error;

use crate::range::TimeRange;

/// Failures from reading, writing, and planning against the tape.
#[derive(Error, Debug)]
/// Failures from reading, writing, and planning against the tape.
pub enum StoreError {
    /// The underlying file system refused the operation.
    #[error("store I/O failure: {0}")]
    Io(#[from] IoError),
    /// The store was locked past its busy timeout. The only variant worth
    /// retrying: contention elsewhere, never a fact about the data.
    #[error("store is busy: {0}")]
    Busy(String),
    /// A bar failed the shared structural validation, or a stored row could
    /// not be materialized into a valid bar (corrupt decimal text, negative
    /// volume, out-of-range stamp). Permanent: retrying reproduces it exactly.
    #[error("invalid bar: {0}")]
    InvalidBar(String),
    /// A price the market's scale cannot represent. Carries the symbol, the
    /// offending price, and the scale it was checked against, so the caller
    /// can route to repair instead of guessing. Never rounded, never
    /// truncated: an unrepresentable row is refused, and the refusal is
    /// counted, not silent. Permanent: retrying reproduces it exactly.
    #[error("price {value} not representable at scale {scale} for {symbol}")]
    PriceScale {
        /// The market the price was written for.
        symbol: String,
        /// The offending price, as carried.
        value: rust_decimal::Decimal,
        /// The scale it was checked against.
        scale: u32,
    },
    /// The calling task was never granted this store, or holds a permit
    /// bound to a different store file. Denied, not defaulted; permanent.
    #[error("store access denied: {0}")]
    Denied(String),
    /// The store configuration is incoherent (an empty path, a zero-sized
    /// pool, no read or write permits). Permanent: the configuration must
    /// change, and no retry of the same configuration can succeed.
    #[error("invalid store configuration: {0}")]
    Config(String),
    /// A market is already registered for this `(symbol, venue)` pair.
    /// Permanent: registering again changes nothing, so it refuses.
    #[error("duplicate market: {0}")]
    Duplicate(String),
    /// The catalog reached its market bound. Refusing rather than evicting:
    /// a durable identity must never be recycled under a live tape.
    #[error("market catalog full: {0}")]
    CatalogFull(String),
    /// The tape reached its bar-capacity bound. Refusing rather than
    /// growing: the bound measures producer memory capacity (the whole
    /// store replays into memory at boot), never the trustworthiness of
    /// the data. Permanent: the configuration must change, and no retry
    /// of the same configuration can succeed.
    #[error("tape capacity exceeded: {0}")]
    Capacity(String),
    /// A read could not be resolved into a plan.
    #[error("read plan refused: {0}")]
    Plan(#[from] PlanError),
    /// A history provider failed on a fill the store requested: absent data,
    /// unusable bytes, or a timed-out fetch. Terminal reporting for a
    /// single-shot fill, which never retries — the repair loop's own retry
    /// taxonomy lives on the repair verdicts, not here.
    #[error("history failure: {0}")]
    History(String),
    /// A market-data Parquet file could not be ingested: a schema mismatch,
    /// an undecodable or null value, a fractional volume, or a mixed-ticker
    /// file read without a ticker name. Permanent: the same bytes reproduce
    /// it exactly, so retrying changes nothing.
    #[error("market parquet ingest failed: {reason}")]
    Ingest {
        /// The refusal, with the row or column that produced it.
        reason: String,
    },
    /// Two readings of one instant could not be reconciled, or a snapshot
    /// artifact is inconsistent with its manifest row.
    #[error("snapshot failure: {0}")]
    Snapshot(#[from] SnapshotError),
    /// A requested operation needs a build feature this binary was not
    /// compiled with. Permanent: the feature must be enabled and the binary
    /// rebuilt. Reported rather than failing to compile, so the tape
    /// surface stays fully usable in a lean build.
    #[error("store feature disabled: {0}")]
    FeatureDisabled(String),
}

impl StoreError {
    /// Whether the operation may be attempted again.
    ///
    /// Only contention is transient. An unreachable database, a refused plan,
    /// and a malformed bar are all facts about the world that a retry
    /// reproduces exactly.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Busy(_))
    }
}

impl From<StoreError> for WorkError {
    fn from(err: StoreError) -> Self {
        Self::Execution(err.to_string())
    }
}

common_core::impl_from_io_error!(StoreError);

/// Why a requested read could not be resolved into a plan.
///
/// Every variant is a **task-value** refusal: the request cannot be answered
/// correctly, so it is not answered. None is a confidence reading.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
/// Why a requested read could not be resolved into a plan.
/// 
/// Every variant is a **task-value** refusal: the request cannot be answered
/// correctly, so it is not answered. None is a confidence reading.
pub enum PlanError {
    /// The request named no symbol.
    #[error("symbol must be nonempty")]
    BlankSymbol,
    /// The catalog holds no such symbol.
    #[error("unknown symbol: {0}")]
    UnknownSymbol(String),
    /// The symbol is known but cannot be read at this bar spacing.
    #[error("unsupported timeframe {timeframe:?} for {symbol}")]
    UnsupportedTimeframe {
        /// The symbol that was asked for.
        symbol: String,
        /// The bar spacing that cannot be served.
        timeframe: Timeframe,
    },
    /// The requested window ends before it begins.
    #[error("inverted range: start {start} is not before end {end}")]
    InvertedRange {
        /// Requested lower bound.
        start: i64,
        /// Requested upper bound.
        end: i64,
    },
    /// An open-ended read carried no bar limit, so it would scan everything.
    #[error("unbounded read with no bar limit: refusing to scan the whole tape")]
    UnboundedUnlimited,
    /// The wanted window is torn by an interior hole, or the tape holds no
    /// rows to plan against at all. A tail past the tape's end is staleness,
    /// not a hole, and resolves; this fires only when bars the request
    /// covers are missing from inside the covered extent.
    #[error("uncovered range {span:?} at {want:?}: refusing a read whose coverage has holes")]
    UncoveredRange {
        /// The uncovered sub-span: the first hole overlapping the request,
        /// or the whole wanted window when the tape is empty.
        span: TimeRange,
        /// The requested bar spacing.
        want: Timeframe,
    },
}

/// Why two readings of one instant could not be reconciled.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
/// Why two readings of one instant could not be reconciled.
pub enum SnapshotError {
    /// The merged reading violates a structural invariant the shared bar
    /// validator already owns. The reason is that validator's text, carried
    /// verbatim rather than restated.
    #[error("inconsistent snapshot: {reason}")]
    Inconsistent {
        /// The shared validator's reason.
        reason: String,
    },
    /// Two different instants were offered as one key.
    #[error("key conflict: {held} then {newer}")]
    KeyConflict {
        /// The stamp already held.
        held: String,
        /// The stamp offered against it.
        newer: String,
    },
    /// A sealed artifact could not be written or read: an I/O or encoding
    /// failure at the Arrow/Parquet boundary, reported with its reason
    /// rather than a lossy summary. Permanent: the same input reproduces it.
    #[error("snapshot artifact failure: {reason}")]
    Artifact {
        /// The underlying failure, carried verbatim.
        reason: String,
    },
    /// A manifest row names a file that is not present. Loud, never a silent
    /// empty read: a caller learns the artifact it was promised is gone.
    #[error("snapshot manifest names a missing file: {path}")]
    MissingArtifact {
        /// The path the manifest row expected.
        path: String,
    },
    /// A snapshot file has no manifest row: it was never sealed, or the
    /// seal's manifest write did not commit. Loud, never a silent empty read.
    #[error("snapshot file has no manifest row: {path}")]
    Unmanifested {
        /// The path that has no manifest row.
        path: String,
    },
}

#[cfg(test)]
#[path = "../tests/error.rs"]
mod tests;