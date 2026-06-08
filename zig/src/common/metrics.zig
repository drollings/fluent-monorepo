/// metrics.zig — Generic latency histogram primitive (M8.1)
///
/// Thread-safe Prometheus-style histogram for latency measurement.
/// Used by coral (cache tier resolution), vector (HNSW/DB queries),
/// and llm (HTTP call duration) subsystems.
///
/// Bucket boundaries (milliseconds): 1, 5, 10, 25, 50, 100, 250, 500, 1000,
/// 2500, 5000 — matches Prometheus histogram standard boundaries for HTTP SLOs.
const std = @import("std");

// ---------------------------------------------------------------------------
// Histogram bucket boundaries
// ---------------------------------------------------------------------------

/// Upper-bound millisecond thresholds for each finite bucket.
/// The last implicit bucket is +Inf (all observations).
pub const BUCKET_MS = [_]u64{ 1, 5, 10, 25, 50, 100, 250, 500, 1_000, 2_500, 5_000 };
pub const BUCKET_COUNT: usize = BUCKET_MS.len + 1; // +1 for the +Inf bucket

// ---------------------------------------------------------------------------
// LatencyHistogram
// ---------------------------------------------------------------------------

/// Thread-safe latency histogram with atomic bucket counters.
/// Tracks latency distributions; managed by owner; key invariant is accurate measurement.
pub const LatencyHistogram = struct {
    /// Bucket counts: index i accumulates observations where
    /// BUCKET_MS[i-1] < latency_ms ≤ BUCKET_MS[i].
    /// Index BUCKET_COUNT-1 is the +Inf bucket.
    buckets: [BUCKET_COUNT]std.atomic.Value(u64) = undefined,
    /// Total count of all observations.
    count: std.atomic.Value(u64) = std.atomic.Value(u64).init(0),
    /// Sum of all observed latencies in milliseconds.
    sum_ms: std.atomic.Value(u64) = std.atomic.Value(u64).init(0),

    pub fn init() LatencyHistogram {
        var h = LatencyHistogram{};
        for (&h.buckets) |*b| b.* = std.atomic.Value(u64).init(0);
        return h;
    }

    /// Record one observation of `latency_ms` milliseconds.
    pub fn observe(self: *LatencyHistogram, latency_ms: u64) void {
        _ = self.count.fetchAdd(1, .monotonic);
        _ = self.sum_ms.fetchAdd(latency_ms, .monotonic);
        // Find the first bucket whose upper bound ≥ latency_ms.
        for (BUCKET_MS, 0..) |bound, i| {
            if (latency_ms <= bound) {
                _ = self.buckets[i].fetchAdd(1, .monotonic);
                return;
            }
        }
        // Falls into the +Inf bucket.
        _ = self.buckets[BUCKET_COUNT - 1].fetchAdd(1, .monotonic);
    }

    /// Return the cumulative count for bucket `idx` (Prometheus-style:
    /// bucket[i] counts all observations ≤ BUCKET_MS[i]).
    pub fn cumulativeBucket(self: *const LatencyHistogram, idx: usize) u64 {
        var cum: u64 = 0;
        var i: usize = 0;
        while (i <= idx) : (i += 1) {
            cum += self.buckets[i].load(.monotonic);
        }
        return cum;
    }

    /// Estimate the `p`-th percentile latency (0.0–1.0) from histogram buckets.
    /// Returns the upper bound of the first bucket whose cumulative count
    /// reaches or exceeds `p * total`.  Returns 0 when no observations.
    pub fn estimatePercentile(self: *const LatencyHistogram, p: f64) u64 {
        const total = self.count.load(.monotonic);
        if (total == 0) return 0;
        const target: u64 = @intFromFloat(@ceil(p * @as(f64, @floatFromInt(total))));
        var cum: u64 = 0;
        for (BUCKET_MS, 0..) |bound, i| {
            cum += self.buckets[i].load(.monotonic);
            if (cum >= target) return bound;
        }
        // All observations fall in the +Inf bucket.
        return std.math.maxInt(u64);
    }
};

// =============================================================================
// Tests
// =============================================================================

const testing = std.testing;

test "LatencyHistogram: observe increments count and sum" {
    var h = LatencyHistogram.init();
    h.observe(10);
    h.observe(20);
    try testing.expectEqual(@as(u64, 2), h.count.load(.monotonic));
    try testing.expectEqual(@as(u64, 30), h.sum_ms.load(.monotonic));
}

test "LatencyHistogram: observe routes to correct bucket" {
    var h = LatencyHistogram.init();
    h.observe(1); // bucket 0 (≤1ms)
    h.observe(5); // bucket 1 (≤5ms)
    h.observe(6); // bucket 2 (≤10ms)
    try testing.expectEqual(@as(u64, 1), h.buckets[0].load(.monotonic));
    try testing.expectEqual(@as(u64, 1), h.buckets[1].load(.monotonic));
    try testing.expectEqual(@as(u64, 1), h.buckets[2].load(.monotonic));
}

test "LatencyHistogram: large value goes to +Inf bucket" {
    var h = LatencyHistogram.init();
    h.observe(100_000);
    try testing.expectEqual(@as(u64, 1), h.buckets[BUCKET_COUNT - 1].load(.monotonic));
}

test "LatencyHistogram: cumulative bucket includes earlier buckets" {
    var h = LatencyHistogram.init();
    h.observe(1); // bucket 0
    h.observe(5); // bucket 1
    // cumulativeBucket(1) = bucket[0] + bucket[1] = 2
    try testing.expectEqual(@as(u64, 2), h.cumulativeBucket(1));
}

test "LatencyHistogram: estimatePercentile returns 0 when empty" {
    const h = LatencyHistogram.init();
    try testing.expectEqual(@as(u64, 0), h.estimatePercentile(0.5));
}

test "LatencyHistogram: estimatePercentile p50 with known observations" {
    var h = LatencyHistogram.init();
    h.observe(1);
    h.observe(1);
    h.observe(10);
    h.observe(10);
    try testing.expectEqual(@as(u64, 1), h.estimatePercentile(0.5));
    try testing.expectEqual(@as(u64, 10), h.estimatePercentile(0.75));
}

test "LatencyHistogram: thread-safe concurrent observe" {
    var h = LatencyHistogram.init();
    const N_THREADS = 4;
    const N_OBS = 100;

    const S = struct {
        fn worker(hist: *LatencyHistogram) void {
            for (0..N_OBS) |_| hist.observe(5);
        }
    };

    var threads: [N_THREADS]std.Thread = undefined;
    for (&threads) |*t| t.* = try std.Thread.spawn(.{}, S.worker, .{&h});
    for (&threads) |*t| t.join();

    try testing.expectEqual(@as(u64, N_THREADS * N_OBS), h.count.load(.monotonic));
}
