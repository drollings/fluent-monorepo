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

pub mod append_log;
#[cfg(feature = "parquet")]
pub(crate) mod arrow_decode;
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
pub mod registry;
pub mod repair;
pub mod snapshot;
pub mod store;
pub mod tape;

pub use append_log::{AppendLog, AppendRecord};
pub use capability::StoreStorage;
pub use catalog::{AssetClass, Market, PriceScale, DEFAULT_MAX_MARKETS};
pub use commit::{Arrival, ClosedCounts, ClosedEmitter, KeyedSnapshot, SealReason};
pub use coverage::{
    classify, Coverage, FreshnessPolicy, GapClass, Watermark, DEFAULT_MAX_LAG_BARS,
};
pub use error::{PlanError, SnapshotError, StoreError};
pub use fold::{
    bucket_start, fold_bars, fold_bars_edges, fold_origin_ms, FoldEdges, FoldPolicy,
    FOLD_ORIGIN_MS, WEEK_FOLD_ORIGIN_MS,
};
pub use health::StoreHealth;
pub use health_edge::{health_edge, next_health, HealthEdge};
pub use history::{
    BoxFuture, FetchPage, FetchRequest, FileHistory, HistoryError, HistoryProvider, NetworkStub,
    ParseFn, DEFAULT_PAGE_BARS,
};
pub use identity::VerifiedId;
pub use key::{SymbolId, TapeKey};
pub use manifest::{SealManifest, SnapshotRecord};
pub use market_parquet::{
    read_ohlcv_parquet, read_ohlcv_parquet_lenient, LenientRead, OhlcvRead, SkippedRow,
};
pub use plan::{ReadPlan, SpanOrigin, SpanPlan};
pub use range::TimeRange;
pub use registry::{MarketRegistry, CATALOG_FILE};
pub use repair::{
    classify_history_error, classify_store_error, retry_delay_ms, spans_for_want,
    RecoveryCoordinator, RecoveryPlan, RepairConfig, RepairReason, RepairStatus, RepairWorker,
    SymbolRepair,
};
pub use snapshot::{
    read_snapshot, reconcile_orphans, seal, ReconcileReport, SealOptions, SealedSnapshot,
    SnapshotWriter, DEFAULT_MIN_PART_ROWS,
};
pub use store::{
    AppendOutcome, CandleSeries, CandleStore, ReadRequest, WritePolicy, DEFAULT_APPEND_CHUNK,
};
pub use tape::{MarketTape, TapeOptions, DEFAULT_MAX_BARS, SEALS_DIR, TAPES_DIR};
