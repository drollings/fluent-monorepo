/// constants.zig — Shared resource-limit constants
///
/// Single source of truth for all size/count caps used across the codebase.
/// Import from any module that has `common` as a dependency:
///   const constants = @import("common").constants;
///
/// For modules without the `common` dependency (e.g. coral_batch), reference
/// the value directly and add a comment pointing to this file.
/// Maximum file size accepted by the batch ingestor (100 MB).
/// Prevents OOM when a caller passes an unexpectedly large Turtle file.
pub const MAX_FILE_SIZE: usize = 100 * 1024 * 1024;

/// Maximum MCP request body (10 MB).
/// The MCP server returns HTTP 413 for requests that exceed this limit.
pub const MAX_MCP_REQUEST_SIZE: usize = 10 * 1024 * 1024;

/// Maximum candidate nodes considered during a KNN cosine-similarity scan.
/// Caps full-table-scan cost when the node count grows to millions.
pub const MAX_KNN_CANDIDATES: usize = 100_000;

/// Maximum number of embedding dimensions accepted from external sources.
/// Guards against malformed responses that claim absurdly large vectors.
pub const MAX_EMBEDDING_DIMENSIONS: usize = 4_096;

/// Maximum JSON nesting depth for recursive parsers.
pub const MAX_JSON_DEPTH: usize = 100;

/// Maximum WASM host-function calls per plugin invocation.
/// Rate-limits tight-loop DoS from malicious WASM modules.
pub const MAX_WASM_HOST_CALLS: u32 = 10_000;
