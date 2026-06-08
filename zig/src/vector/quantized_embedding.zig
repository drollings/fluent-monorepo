//! quantized_embedding.zig — int8 Quantized Embeddings for Memory Efficiency
//!
//! Provides compact storage for embeddings by quantizing float32 vectors to int8.
//! Reduces memory footprint by 4× (4 bytes per float → 1 byte per int8) while
//! preserving cosine similarity ordering for approximate nearest neighbor search.
//!
//! §Use case:
//!   - Edge devices with limited RAM (RaspberryPi, mobile)
//!   - Large embeddings databases (>1M vectors)
//!   - Preliminary filtering before full-precision reranking
//!
//! §Quantization scheme:
//!   - Scale factor per vector: max(|x|) across all dimensions
//!   - Quantize: q[i] = clamp(round(x[i] / scale), -128, 127)
//!   - Dequantize: x_hat[i] = q[i] * scale
//!   - Cosine similarity: computed directly on int8 using dot product + normalization
//!
//! §Hamming distance for binary comparison:
//!   - Convert int8 to binary sign (+1/-1)
//!   - XOR population count gives Hamming distance
//!   - Useful for approximate similarity without float operations

const std = @import("std");
const Allocator = std.mem.Allocator;

/// Manages quantized embedding state with fixed-size buffers; ownership model is per-instance; key invariant is stable key structure.
pub const QuantizedEmbedding = struct {
    /// Quantized values in [-128, 127].
    data: []i8,
    /// Scale factor for dequantization.  max(|x|) of the original vector.
    scale: f32,
    /// Dimensionality (len(data) == dim).
    dim: u32,

    const Self = @This();

    /// Quantize a float32 embedding to int8.
    /// The returned struct owns `data`; caller must call `deinit(allocator)`.
    pub fn fromF32(allocator: Allocator, f32_emb: []const f32) !Self {
        if (f32_emb.len == 0) return Self{ .data = &.{}, .scale = 1.0, .dim = 0 };
        var max_abs: f32 = 0.0;
        for (f32_emb) |v| {
            const abs_v = @abs(v);
            if (abs_v > max_abs) max_abs = abs_v;
        }

        // Avoid division by zero
        const scale = if (max_abs > 0) max_abs / 127.0 else 1.0;

        const data = try allocator.alloc(i8, f32_emb.len);
        errdefer allocator.free(data);

        for (f32_emb, 0..) |v, i| {
            const scaled = v / scale;
            const rounded = @round(scaled);
            // Clamp to int8 range
            data[i] = @intCast(std.math.clamp(@as(i32, @intFromFloat(rounded)), -128, 127));
        }

        return Self{
            .data = data,
            .scale = scale,
            .dim = @intCast(f32_emb.len),
        };
    }

    /// Free owned data.
    pub fn deinit(self: *Self, allocator: Allocator) void {
        allocator.free(self.data);
        self.* = .{ .data = &.{}, .scale = 1.0, .dim = 0 };
    }

    /// Dequantize back to float32 (approximate).
    /// Caller owns the returned slice.
    pub fn toF32(self: Self, allocator: Allocator) ![]f32 {
        const result = try allocator.alloc(f32, self.dim);
        for (self.data, 0..) |q, i| {
            result[i] = @as(f32, @floatFromInt(q)) * self.scale;
        }
        return result;
    }

    /// Compute cosine similarity between two quantized embeddings.
    /// Uses int32 arithmetic for the dot product to avoid overflow.
    /// Result is in [-1, 1] but may be approximate due to quantization error.
    pub fn cosineSimilarity(self: Self, other: Self) f32 {
        if (self.dim != other.dim or self.dim == 0) return 0.0;

        var dot: i64 = 0;
        var norm_a: i64 = 0;
        var norm_b: i64 = 0;

        for (self.data, other.data) |a, b| {
            dot += @as(i64, a) * @as(i64, b);
            norm_a += @as(i64, a) * @as(i64, a);
            norm_b += @as(i64, b) * @as(i64, b);
        }

        const norm_a_f = @sqrt(@as(f64, @floatFromInt(norm_a)));
        const norm_b_f = @sqrt(@as(f64, @floatFromInt(norm_b)));
        if (norm_a_f == 0 or norm_b_f == 0) return 0.0;

        return @floatCast(@as(f64, @floatFromInt(dot)) / (norm_a_f * norm_b_f));
    }

    /// Compute dot product between two quantized embeddings.
    /// Returns the raw int64 dot product (useful for ranking without sqrt).
    pub fn dotProduct(self: Self, other: Self) i64 {
        if (self.dim != other.dim) return 0;
        var dot: i64 = 0;
        for (self.data, other.data) |a, b| {
            dot += @as(i64, a) * @as(i64, b);
        }
        return dot;
    }

    /// Compute Hamming distance between binary sign representations.
    /// Each dimension is converted to its sign bit, then XOR population.
    /// Returns the number of dimensions where signs differ.
    pub fn hammingDistance(self: Self, other: Self) u32 {
        if (self.dim != other.dim) return @intCast(self.dim);
        var dist: u32 = 0;
        for (self.data, other.data) |a, b| {
            // Sign bit: 0 for positive/zero, 1 for negative
            const sign_a: u8 = if (a < 0) 1 else 0;
            const sign_b: u8 = if (b < 0) 1 else 0;
            dist += sign_a ^ sign_b;
        }
        return dist;
    }

    /// Compute L2 distance squared (approximate) between quantized vectors.
    /// Returns f32 for compatibility with similarity thresholds.
    pub fn l2DistanceSquared(self: Self, other: Self) f32 {
        if (self.dim != other.dim) return std.math.floatMax(f32);
        var sum: i64 = 0;
        for (self.data, other.data) |a, b| {
            const diff = @as(i64, a) - @as(i64, b);
            sum += diff * diff;
        }
        // Scale back by both scales
        return @as(f32, @floatFromInt(@as(i32, @intCast(sum)))) * self.scale * other.scale;
    }

    /// Serialize to a byte buffer for storage.
    /// Format: [dim: u32LE][scale: f32LE][data: dim × i8]
    /// Caller owns the returned slice.
    pub fn serialize(self: Self, allocator: Allocator) ![]u8 {
        const size = 4 + 4 + self.dim; // u32 + f32 + data
        const buf = try allocator.alloc(u8, size);

        std.mem.writeInt(u32, buf[0..4], self.dim, .little);
        std.mem.writeInt(u32, buf[4..8], @bitCast(self.scale), .little);
        @memcpy(buf[8 .. 8 + self.dim], @as([*]const u8, @ptrCast(self.data.ptr))[0..self.dim]);

        return buf;
    }

    /// Deserialize from a byte buffer.
    /// The returned struct does NOT own `buf` — caller must copy if needed.
    pub fn deserialize(buf: []const u8) ?Self {
        if (buf.len < 8) return null;
        const dim = std.mem.readInt(u32, buf[0..4], .little);
        if (buf.len < 8 + dim) return null;
        const scale: f32 = @bitCast(std.mem.readInt(u32, buf[4..8], .little));
        const data_ptr: [*]const i8 = @ptrCast(@alignCast(buf[8..].ptr));

        return Self{
            .data = @constCast(data_ptr[0..dim]),
            .scale = scale,
            .dim = dim,
        };
    }

    /// Deserialize and copy data into allocator-owned buffer.
    pub fn deserializeCopy(allocator: Allocator, buf: []const u8) !Self {
        const result = deserialize(buf) orelse return error.InvalidBuffer;
        const data_copy = try allocator.dupe(i8, result.data);
        return Self{
            .data = data_copy,
            .scale = result.scale,
            .dim = result.dim,
        };
    }
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "QuantizedEmbedding.fromF32: basic round-trip" {
    const allocator = testing.allocator;
    const f32_emb = [_]f32{ 0.5, -0.3, 0.8, -0.1, 0.0 };

    var qe = try QuantizedEmbedding.fromF32(allocator, &f32_emb);
    defer qe.deinit(allocator);

    try testing.expectEqual(@as(u32, 5), qe.dim);
    try testing.expect(qe.scale > 0);

    const f32_roundtrip = try qe.toF32(allocator);
    defer allocator.free(f32_roundtrip);

    // Check approximate reconstruction
    for (f32_emb, f32_roundtrip) |orig, recon| {
        try testing.expectApproxEqAbs(orig, recon, qe.scale);
    }
}

test "QuantizedEmbedding.cosineSimilarity: identical vectors" {
    const allocator = testing.allocator;
    const f32_emb = [_]f32{ 1.0, 2.0, 3.0, 4.0 };

    var qe = try QuantizedEmbedding.fromF32(allocator, &f32_emb);
    defer qe.deinit(allocator);

    const sim = qe.cosineSimilarity(qe);
    try testing.expectApproxEqAbs(@as(f32, 1.0), sim, 0.01);
}

test "QuantizedEmbedding.cosineSimilarity: orthogonal vectors" {
    const allocator = testing.allocator;

    const a = [_]f32{ 1.0, 0.0 };
    const b = [_]f32{ 0.0, 1.0 };

    var qe_a = try QuantizedEmbedding.fromF32(allocator, &a);
    defer qe_a.deinit(allocator);
    var qe_b = try QuantizedEmbedding.fromF32(allocator, &b);
    defer qe_b.deinit(allocator);

    const sim = qe_a.cosineSimilarity(qe_b);
    try testing.expectApproxEqAbs(@as(f32, 0.0), sim, 0.01);
}

test "QuantizedEmbedding.hammingDistance: opposite signs" {
    const allocator = testing.allocator;

    const a = [_]f32{ 1.0, -1.0, 1.0, -1.0 };
    const b = [_]f32{ -1.0, 1.0, -1.0, 1.0 };

    var qe_a = try QuantizedEmbedding.fromF32(allocator, &a);
    defer qe_a.deinit(allocator);
    var qe_b = try QuantizedEmbedding.fromF32(allocator, &b);
    defer qe_b.deinit(allocator);

    const dist = qe_a.hammingDistance(qe_b);
    try testing.expectEqual(@as(u32, 4), dist); // All signs differ
}

test "QuantizedEmbedding.serialize/deserialize" {
    const allocator = testing.allocator;
    const f32_emb = [_]f32{ 0.5, -0.3, 0.8, -0.1, 0.0 };

    var qe = try QuantizedEmbedding.fromF32(allocator, &f32_emb);
    defer qe.deinit(allocator);

    const buf = try qe.serialize(allocator);
    defer allocator.free(buf);

    var qe2 = try QuantizedEmbedding.deserializeCopy(allocator, buf);
    defer qe2.deinit(allocator);

    try testing.expectEqual(qe.dim, qe2.dim);
    try testing.expectApproxEqAbs(qe.scale, qe2.scale, 0.0001);
    for (qe.data, qe2.data) |a, b| {
        try testing.expectEqual(a, b);
    }
}
