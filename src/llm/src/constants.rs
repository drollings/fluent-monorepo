//! LLM-domain constants — the single owner.
//!
//! Moved verbatim from `common_core::constants`: `MAX_EMBEDDING_DIMENSIONS`
//! (the embedding width every provider and HNSW store agrees on) and the
//! three request-budget defaults (`DEFAULT_TOTAL_TIMEOUT_MS`,
//! `DEFAULT_IDLE_TIMEOUT_MS`, `DEFAULT_RETRY_INTERVAL_S`) that every
//! router serde default and dispatch call site reads. Generic caps
//! (`MAX_VALUE_LEN`, `MAX_FILE_SIZE`, `MAX_JSON_DEPTH`,
//! `MAX_MCP_REQUEST_SIZE`, `default_true`) stay in `common-core`; the HNSW
//! vocabulary (`MAX_KNN_CANDIDATES`, `DEFAULT_HNSW_THRESHOLD`, `HnswParams`)
//! is canonically owned by `search_vector::hnsw` with value-identical
//! `common_core::constants` shims. ONNX tuning
//! (`DEFAULT_ONNX_LIMITER_CAP`, `DEFAULT_ONNX_THREADS`) lives in the router.
//!
//! The locked values in `tests/constants.rs` are the lasting contract.
//!
//! These are task-value budgets (context width, wall-clock budgets, retry
//! cadence), not producer confidence. Never silently retune the values.

/// Maximum embedding dimensions every provider and HNSW store agrees on.
pub const MAX_EMBEDDING_DIMENSIONS: usize = 4_096;

/// Canonical per-dispatch wall-clock budget for a full LLM request (ms).
/// `RoutingTarget` (serde) and `ModelEntry` (serde) both read this constant.
pub const DEFAULT_TOTAL_TIMEOUT_MS: u64 = 300_000;
/// Canonical per-chunk idle budget for a streaming LLM response (ms).
pub const DEFAULT_IDLE_TIMEOUT_MS: u64 = 30_000;
/// Canonical base interval between retries (seconds).
pub const DEFAULT_RETRY_INTERVAL_S: u64 = 1;
