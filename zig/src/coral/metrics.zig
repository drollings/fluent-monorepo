/// metrics.zig — Coral Latency Histograms and Resolution Counters (M8.1)
///
/// Per-tier latency histograms for L1–L5 cache resolution and misses.
/// LatencyHistogram is defined in src/common/metrics.zig and re-exported here.
/// CoralMetrics aggregates six histograms with coral-specific tier semantics.
///
/// Prometheus text format is emitted by `formatPrometheus()`.
const std = @import("std");
const common = @import("common");

pub const LatencyHistogram = common.metrics.LatencyHistogram;
pub const BUCKET_MS = common.metrics.BUCKET_MS;
pub const BUCKET_COUNT = common.metrics.BUCKET_COUNT;

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

// LatencyHistogram unit tests live in src/common/metrics.zig.

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

    var aw: std.Io.Writer.Allocating = .init(testing.allocator);
    defer aw.deinit();
    try m.formatPrometheus(&aw.writer);
    const out = aw.written();
    try testing.expect(std.mem.indexOf(u8, out, "coral_deterministic_rate") != null);
    try testing.expect(std.mem.indexOf(u8, out, "coral_l1_hit_latency_bucket") != null);
}

test "CoralMetrics: thread-safe concurrent observe" {
    var m = CoralMetrics{};
    const N_THREADS = 4;
    const N_OBS = 100;

    const S = struct {
        fn worker(metrics: *CoralMetrics) void {
            for (0..N_OBS) |_| {
                metrics.l1_hit.observe(5);
            }
        }
    };

    var threads: [N_THREADS]std.Thread = undefined;
    for (&threads) |*t| {
        t.* = try std.Thread.spawn(.{}, S.worker, .{&m});
    }
    for (&threads) |*t| t.join();

    try testing.expectEqual(@as(u64, N_THREADS * N_OBS), m.l1Hits());
}
