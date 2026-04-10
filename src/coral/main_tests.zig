//! Tests for main.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const coral_db = @import("coral_db");
const main_mod = @import("main.zig");

test "coral: core module imports compile" {
    // db.zig and schema.zig are exposed via the `coral_db` named module.
    // They cannot be imported relatively when coral_db is a build dep (module conflict).
    _ = coral_db; // exercises db.zig + schema.zig via named module
    _ = @import("yago_ingest.zig"); // yago_ingest.zig pulls in coral_batch + ontology
    _ = @import("token_budget.zig"); // M7.1 TokenEstimator
    _ = @import("metrics.zig"); // M8.1 LatencyHistogram
    _ = @import("http_transport.zig"); // M4.1 HTTP/SSE transport
    // quantized_embedding moved to src/vector/quantized_embedding.zig (P5.2 int8 embeddings)
}
