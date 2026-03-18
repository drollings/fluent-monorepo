//! Vector operations — cosine similarity, normalization, hybrid merge.
//!
//! Used by guidance's LanceDB-style vector search backend.

const std = @import("std");

// ── Cosine similarity ─────────────────────────────────────────────

/// Cosine similarity between two vectors. Returns 0.0–1.0.
/// Returns 0.0 for empty, mismatched, or degenerate inputs.
pub fn cosineSimilarity(a: []const f32, b: []const f32) f32 {
    if (a.len != b.len or a.len == 0) return 0.0;

    var dot: f64 = 0.0;
    var norm_a: f64 = 0.0;
    var norm_b: f64 = 0.0;

    for (a, b) |x_raw, y_raw| {
        const x: f64 = @floatCast(x_raw);
        const y: f64 = @floatCast(y_raw);
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }

    const denom = @sqrt(norm_a) * @sqrt(norm_b);
    if (!std.math.isFinite(denom) or denom < std.math.floatEps(f64)) {
        return 0.0;
    }

    const raw = dot / denom;
    if (!std.math.isFinite(raw)) {
        return 0.0;
    }

    // Clamp to [0, 1] — embeddings are typically positive
    const clamped = @max(0.0, @min(1.0, raw));
    return @floatCast(clamped);
}

// ── Serialization ─────────────────────────────────────────────────

/// Serialize f32 vector to bytes (little-endian). Caller owns result.
pub fn vecToBytes(allocator: std.mem.Allocator, v: []const f32) ![]u8 {
    const bytes = try allocator.alloc(u8, v.len * 4);
    for (v, 0..) |f, i| {
        const le: [4]u8 = @bitCast(f);
        @memcpy(bytes[i * 4 ..][0..4], &le);
    }
    return bytes;
}

/// Deserialize bytes to f32 vector (little-endian). Caller owns result.
pub fn bytesToVec(allocator: std.mem.Allocator, bytes: []const u8) ![]f32 {
    const count = bytes.len / 4;
    const result = try allocator.alloc(f32, count);
    for (0..count) |i| {
        const chunk = bytes[i * 4 ..][0..4];
        result[i] = @bitCast(chunk.*);
    }
    return result;
}

// ── Scored result ─────────────────────────────────────────────────

pub const ScoredResult = struct {
    id: i64,
    vector_score: ?f32 = null,
    keyword_score: ?f32 = null,
    final_score: f32 = 0.0,
};

// ── Hybrid merge ──────────────────────────────────────────────────

pub const IdScore = struct {
    id: i64,
    score: f32,
};

/// Hybrid merge: combine vector and keyword results with weighted fusion.
///
/// Normalizes keyword scores to [0, 1]. Vector scores (cosine similarity)
/// are assumed to already be in [0, 1].
///
/// final_score = vector_weight * vector_score + keyword_weight * keyword_score
///
/// Deduplicates by id. Results sorted by final_score descending.
/// Caller owns the returned slice and must free it.
pub fn hybridMerge(
    allocator: std.mem.Allocator,
    vector_results: []const IdScore,
    keyword_results: []const IdScore,
    vector_weight: f32,
    keyword_weight: f32,
    limit: usize,
) ![]ScoredResult {
    var ids: std.ArrayList(i64) = .empty;
    defer ids.deinit(allocator);

    var vec_scores = std.AutoHashMap(i64, f32).init(allocator);
    defer vec_scores.deinit();

    var kw_scores = std.AutoHashMap(i64, f32).init(allocator);
    defer kw_scores.deinit();

    for (vector_results) |vr| {
        const entry = try vec_scores.getOrPut(vr.id);
        if (!entry.found_existing) {
            entry.value_ptr.* = vr.score;
            try ids.append(allocator, vr.id);
        } else {
            entry.value_ptr.* = @max(entry.value_ptr.*, vr.score);
        }
    }

    // Normalize keyword scores
    var max_kw: f32 = 0.0;
    for (keyword_results) |kr| {
        max_kw = @max(max_kw, kr.score);
    }
    if (max_kw < std.math.floatEps(f32)) max_kw = 1.0;

    for (keyword_results) |kr| {
        const normalized = kr.score / max_kw;
        const entry = try kw_scores.getOrPut(kr.id);
        if (!entry.found_existing) {
            entry.value_ptr.* = normalized;
            if (!vec_scores.contains(kr.id)) {
                try ids.append(allocator, kr.id);
            }
        } else {
            entry.value_ptr.* = @max(entry.value_ptr.*, normalized);
        }
    }

    // Build scored results
    var results: std.ArrayList(ScoredResult) = .empty;
    defer results.deinit(allocator);

    for (ids.items) |id| {
        const vs = vec_scores.get(id);
        const ks = kw_scores.get(id);
        const vs_val = vs orelse 0.0;
        const ks_val = ks orelse 0.0;
        const final = vector_weight * vs_val + keyword_weight * ks_val;
        try results.append(allocator, .{
            .id = id,
            .vector_score = vs,
            .keyword_score = ks,
            .final_score = final,
        });
    }

    // Sort by final_score descending
    std.mem.sortUnstable(ScoredResult, results.items, {}, struct {
        fn lessThan(_: void, lhs: ScoredResult, rhs: ScoredResult) bool {
            return lhs.final_score > rhs.final_score;
        }
    }.lessThan);

    const actual_limit = @min(limit, results.items.len);
    return allocator.dupe(ScoredResult, results.items[0..actual_limit]);
}

// ── Tests ─────────────────────────────────────────────────────────

test "cosine identical vectors" {
    const v = [_]f32{ 1.0, 2.0, 3.0 };
    const sim = cosineSimilarity(&v, &v);
    try std.testing.expect(@abs(sim - 1.0) < 0.001);
}

test "cosine orthogonal vectors" {
    const a = [_]f32{ 1.0, 0.0, 0.0 };
    const b = [_]f32{ 0.0, 1.0, 0.0 };
    const sim = cosineSimilarity(&a, &b);
    try std.testing.expect(@abs(sim) < 0.001);
}

test "cosine empty returns zero" {
    const empty: []const f32 = &.{};
    try std.testing.expectEqual(@as(f32, 0.0), cosineSimilarity(empty, empty));
}

test "vec bytes roundtrip" {
    const original = [_]f32{ 1.0, -2.5, 3.14, 0.0 };
    const bytes = try vecToBytes(std.testing.allocator, &original);
    defer std.testing.allocator.free(bytes);

    const restored = try bytesToVec(std.testing.allocator, bytes);
    defer std.testing.allocator.free(restored);

    try std.testing.expectEqual(@as(usize, 4), restored.len);
    for (original, restored) |a, b_val| {
        try std.testing.expect(@abs(a - b_val) < std.math.floatEps(f32));
    }
}

test "hybrid merge deduplicates" {
    const vec_results = [_]IdScore{.{ .id = 1, .score = 0.9 }};
    const kw_results = [_]IdScore{.{ .id = 1, .score = 10.0 }};
    const merged = try hybridMerge(std.testing.allocator, &vec_results, &kw_results, 0.7, 0.3, 10);
    defer std.testing.allocator.free(merged);

    try std.testing.expectEqual(@as(usize, 1), merged.len);
    try std.testing.expectEqual(@as(i64, 1), merged[0].id);
    try std.testing.expect(merged[0].vector_score != null);
    try std.testing.expect(merged[0].keyword_score != null);
}

test "hybrid merge respects limit" {
    var vec_results: [20]IdScore = undefined;
    for (0..20) |i| {
        vec_results[i] = .{ .id = @intCast(i), .score = 1.0 - @as(f32, @floatFromInt(i)) * 0.05 };
    }
    const merged = try hybridMerge(std.testing.allocator, &vec_results, &.{}, 1.0, 0.0, 5);
    defer std.testing.allocator.free(merged);
    try std.testing.expectEqual(@as(usize, 5), merged.len);
}
