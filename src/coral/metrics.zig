/// metrics.zig — Coral Latency Histograms and Resolution Counters (M8.1)
///
/// Per-tier latency histograms for L1–L5 cache resolution and misses.
/// Thread-safe via atomic increment for all counters and bucket slots.
///
/// Bucket boundaries (milliseconds): 1, 5, 10, 25, 50, 100, 250, 500, 1000,
/// 2500, 5000 (matches Prometheus histogram standard boundaries for HTTP SLOs).
///
/// Prometheus text format is emitted by `formatPrometheus()`.
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

/// A single-tier latency histogram with atomic counters.
/// All fields are public so they can be reset atomically in tests.
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
};

// ---------------------------------------------------------------------------
// CoralMetrics — per-tier histogram collection + resolution counters
// ---------------------------------------------------------------------------

/// Tracks coral health metrics with a fixed-size buffer; managed via ownership model; ensures data integrity.
pub const CoralMetrics = struct {
    l1_hit: LatencyHistogram = LatencyHistogram.init(),
    l2_hit: LatencyHistogram = LatencyHistogram.init(),
    l3_hit: LatencyHistogram = LatencyHistogram.init(),
    l4_hit: LatencyHistogram = LatencyHistogram.init(),
    l5_hit: LatencyHistogram = LatencyHistogram.init(),
    miss: LatencyHistogram = LatencyHistogram.init(),

    /// Resolution counters — incremented by observe() on each histogram.
    pub fn l1Hits(self: *const CoralMetrics) u64 {
        return self.l1_hit.count.load(.monotonic);
    }
    pub fn l2Hits(self: *const CoralMetrics) u64 {
        return self.l2_hit.count.load(.monotonic);
    }
    pub fn l3Hits(self: *const CoralMetrics) u64 {
        return self.l3_hit.count.load(.monotonic);
    }
    pub fn l4Hits(self: *const CoralMetrics) u64 {
        return self.l4_hit.count.load(.monotonic);
    }
    pub fn l5Hits(self: *const CoralMetrics) u64 {
        return self.l5_hit.count.load(.monotonic);
    }
    pub fn misses(self: *const CoralMetrics) u64 {
        return self.miss.count.load(.monotonic);
    }

    /// Deterministic resolution rate: (L1+L2+L3+L4) / total observations.
    /// Returns 0 when no observations have been made.
    pub fn deterministicRate(self: *const CoralMetrics) f64 {
        const det = self.l1Hits() + self.l2Hits() + self.l3Hits() + self.l4Hits();
        const total = det + self.l5Hits() + self.misses();
        if (total == 0) return 0;
        return @as(f64, @floatFromInt(det)) / @as(f64, @floatFromInt(total));
    }

    // ── Prometheus text format ─────────────────────────────────────────────

    /// Write Prometheus-compatible metric lines for all tiers to `writer`.
    ///
    /// Example output:
    ///   coral_l1_hit_latency_bucket{le="1"} 42
    ///   coral_l1_hit_latency_bucket{le="5"} 128
    ///   ...
    ///   coral_deterministic_rate 0.95
    pub fn formatPrometheus(self: *const CoralMetrics, writer: anytype) !void {
        const tiers = [_]struct { name: []const u8, hist: *const LatencyHistogram }{
            .{ .name = "l1_hit", .hist = &self.l1_hit },
            .{ .name = "l2_hit", .hist = &self.l2_hit },
            .{ .name = "l3_hit", .hist = &self.l3_hit },
            .{ .name = "l4_hit", .hist = &self.l4_hit },
            .{ .name = "l5_hit", .hist = &self.l5_hit },
            .{ .name = "miss", .hist = &self.miss },
        };

        for (tiers) |tier| {
            // Emit cumulative bucket lines.
            for (BUCKET_MS, 0..) |bound, i| {
                try writer.print(
                    "coral_{s}_latency_bucket{{le=\"{d}\"}} {d}\n",
                    .{ tier.name, bound, tier.hist.cumulativeBucket(i) },
                );
            }
            // +Inf bucket.
            try writer.print(
                "coral_{s}_latency_bucket{{le=\"+Inf\"}} {d}\n",
                .{ tier.name, tier.hist.count.load(.monotonic) },
            );
            try writer.print(
                "coral_{s}_latency_count {d}\n",
                .{ tier.name, tier.hist.count.load(.monotonic) },
            );
            try writer.print(
                "coral_{s}_latency_sum {d}\n",
                .{ tier.name, tier.hist.sum_ms.load(.monotonic) },
            );
        }

        try writer.print(
            "coral_deterministic_rate {d:.4}\n",
            .{self.deterministicRate()},
        );
    }
};

// =============================================================================
// Tests — M8.1
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

test "CoralMetrics: deterministicRate zero when empty" {
    const m = CoralMetrics{};
    try testing.expectEqual(@as(f64, 0), m.deterministicRate());
}

test "CoralMetrics: deterministicRate 1.0 when only L1 hits" {
    var m = CoralMetrics{};
    m.l1_hit.observe(1);
    m.l1_hit.observe(2);
    try testing.expectApproxEqAbs(@as(f64, 1.0), m.deterministicRate(), 0.001);
}

test "CoralMetrics: deterministicRate 0.5 when equal L1 and L5" {
    var m = CoralMetrics{};
    m.l1_hit.observe(1);
    m.l5_hit.observe(1);
    try testing.expectApproxEqAbs(@as(f64, 0.5), m.deterministicRate(), 0.001);
}

test "CoralMetrics: formatPrometheus emits deterministic_rate line" {
    var m = CoralMetrics{};
    m.l1_hit.observe(5);
    m.l5_hit.observe(5);

    var buf: [4096]u8 = undefined;
    var fbs = std.io.fixedBufferStream(&buf);
    try m.formatPrometheus(fbs.writer());
    const out = fbs.getWritten();
    try testing.expect(std.mem.indexOf(u8, out, "coral_deterministic_rate") != null);
    try testing.expect(std.mem.indexOf(u8, out, "coral_l1_hit_latency_bucket") != null);
}
