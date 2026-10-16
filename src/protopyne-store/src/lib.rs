#![forbid(unsafe_code)]

//! Durable price substrate: instrument identity, a committed-bar lifecycle,
//! coverage, and a reproducible read path.
//!
//! # Role
//!
//! A store answers one question — *what did the tape actually hold over this
//! span, and where is it incomplete?* — and answers it with evidence. Every
//! bar it will ever hand out is a **committed** bar; a still-forming bar never
//! crosses this boundary, so nothing downstream has to ask whether the bar it
//! is looking at is finished.
//!
//! # Boundaries
//!
//! This crate is a **substrate, not a pipeline layer**. It sits beside
//! `types → kernels → strategy → venue → server` and is depended on *by* the
//! layers that need durable prices; it never depends on them.
//!
//! - **No `egui` / `eframe`.** The store is headless by construction. Only
//!   `fluent-egui` and `protopyne-egui` may touch the GUI stack, in either
//!   direction.
//! - **No `venue`.** The store is not an execution venue, journals nothing,
//!   and is unreachable from a submission path.
//! - **No `server`.** It serves no routes and holds no request state.
//! - **No `strategy`, no `kernels`** at runtime. Indicator kernels are a
//!   development dependency only, for the shared canonical-encoding idiom.
//!
//! Its one runtime dependency outside the workspace shared libraries is
//! `protopyne-types`, for the bar vocabulary, the ET session clock, and the
//! fail-closed validation trait.
//!
//! # Standing rule
//!
//! A stored bar carries **no confidence, quality, or staleness field**. A bar
//! is either committed or it is not. Uncertainty at write time is resolved by
//! not writing.

#[cfg(feature = "parquet")]
pub(crate) mod arrow_decode;
pub mod append_log;
pub mod capability;
pub mod catalog;
pub mod commit;
pub mod coverage;
pub mod error;
pub mod fold;
pub mod health;
pub mod health_edge;
pub mod history;
pub mod identity;
pub mod key;
pub mod manifest;
pub mod market_parquet;
pub mod plan;
pub mod range;
pub mod repair;
pub mod registry;
pub mod snapshot;
pub mod store;
pub mod tape;

pub use append_log::{AppendLog, AppendRecord};
pub use capability::StoreStorage;
pub use catalog::{AssetClass, DEFAULT_MAX_MARKETS, Market, PriceScale};
pub use commit::{Arrival, ClosedCounts, ClosedEmitter, KeyedSnapshot, SealReason};
pub use coverage::{
    Coverage, DEFAULT_MAX_LAG_BARS, FreshnessPolicy, GapClass, Watermark, classify,
};
pub use error::{PlanError, SnapshotError, StoreError};
pub use fold::{
    FOLD_ORIGIN_MS, FoldEdges, FoldPolicy, WEEK_FOLD_ORIGIN_MS, bucket_start, fold_bars,
    fold_bars_edges, fold_origin_ms,
};
pub use health::StoreHealth;
pub use health_edge::{HealthEdge, health_edge, next_health};
pub use history::{
    BoxFuture, DEFAULT_PAGE_BARS, FetchPage, FetchRequest, FileHistory, HistoryError,
    HistoryProvider, NetworkStub, ParseFn,
};
pub use identity::VerifiedId;
pub use key::{SymbolId, TapeKey};
pub use manifest::{SealManifest, SnapshotRecord};
pub use market_parquet::{OhlcvRead, read_ohlcv_parquet};
pub use plan::{ReadPlan, SpanOrigin, SpanPlan};
pub use range::TimeRange;
pub use repair::{
    RecoveryCoordinator, RecoveryPlan, RepairConfig, RepairReason, RepairStatus, RepairWorker,
    SymbolRepair, classify_history_error, classify_store_error, retry_delay_ms, spans_for_want,
};
pub use registry::{CATALOG_FILE, MarketRegistry};
pub use snapshot::{
    DEFAULT_MIN_PART_ROWS, ReconcileReport, SealOptions, SealedSnapshot, SnapshotWriter,
    read_snapshot, reconcile_orphans, seal,
};
pub use store::{
    AppendOutcome, CandleSeries, CandleStore, DEFAULT_APPEND_CHUNK, ReadRequest, WritePolicy,
};
pub use tape::{DEFAULT_MAX_BARS, SEALS_DIR, TAPES_DIR, MarketTape, TapeOptions};
