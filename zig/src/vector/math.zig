//! Vector operations — cosine similarity, normalization, hybrid merge.
//!
//! Used by guidance's SQLite vector search backend.

const std = @import("std");

// ── Cosine similarity ─────────────────────────────────────────────

/// Calculates cosine similarity between two arrays of floating-point values.
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

/// Converts a vector of f32 to a byte array using an allocator.
pub fn vecToBytes(allocator: std.mem.Allocator, v: []const f32) ![]u8 {
    const bytes = try allocator.alloc(u8, v.len * 4);
    for (v, 0..) |f, i| {
        const le: [4]u8 = @bitCast(f);
        @memcpy(bytes[i * 4 ..][0..4], &le);
    }
    return bytes;
}

/// Converts a null-terminated byte slice to a vector of f32 values.
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

/// Represents a scored result with fixed-size buffers; managed via ownership model; ensures data integrity across operations.
pub const ScoredResult = struct {
    id: i64,
    vector_score: ?f32 = null,
    keyword_score: ?f32 = null,
    capability_score: ?f32 = null,
    final_score: f32 = 0.0,
};

// ── Hybrid merge ──────────────────────────────────────────────────

pub const IdScore = struct {
    id: i64,
    score: f32,
};

/// RRF (Reciprocal Rank Fusion) constant — standard smoothing parameter.
/// Lower values (e.g., 20) favor top-ranked items more aggressively.
/// Higher values (e.g., 80) give more weight to lower-ranked items.
pub const RRF_K: f32 = 60.0;

/// Combines vector results with weights, merging scores into a unified output.
pub fn hybridMerge(
    allocator: std.mem.Allocator,
    vector_results: []const IdScore,
    keyword_results: []const IdScore,
    vector_weight: f32,
    keyword_weight: f32,
    limit: usize,
) ![]ScoredResult {
    // Maps to accumulate RRF scores and track component scores
    var rrf_scores = std.AutoHashMap(i64, f32).init(allocator);
    defer rrf_scores.deinit();
    var vec_scores = std.AutoHashMap(i64, f32).init(allocator);
    defer vec_scores.deinit();
    var kw_scores = std.AutoHashMap(i64, f32).init(allocator);
    defer kw_scores.deinit();

    // Process vector results with RRF
    for (vector_results, 0..) |vr, rank| {
        const rrf_score = vector_weight * (1.0 / (RRF_K + @as(f32, @floatFromInt(rank))));
        const entry = try rrf_scores.getOrPut(vr.id);
        if (!entry.found_existing) {
            entry.value_ptr.* = rrf_score;
        } else {
            entry.value_ptr.* += rrf_score;
        }

        // Store best vector score for metadata
        const vs_entry = try vec_scores.getOrPut(vr.id);
        if (!vs_entry.found_existing or vr.score > vs_entry.value_ptr.*) {
            vs_entry.value_ptr.* = vr.score;
        }
    }

    // Process keyword results with RRF
    for (keyword_results, 0..) |kr, rank| {
        const rrf_score = keyword_weight * (1.0 / (RRF_K + @as(f32, @floatFromInt(rank))));
        const entry = try rrf_scores.getOrPut(kr.id);
        if (!entry.found_existing) {
            entry.value_ptr.* = rrf_score;
        } else {
            entry.value_ptr.* += rrf_score;
        }

        // Store best keyword score for metadata
        const ks_entry = try kw_scores.getOrPut(kr.id);
        if (!ks_entry.found_existing or kr.score > ks_entry.value_ptr.*) {
            ks_entry.value_ptr.* = kr.score;
        }
    }

    // Build scored results from RRF scores
    var results: std.ArrayList(ScoredResult) = .empty;
    defer results.deinit(allocator);

    var rrf_it = rrf_scores.iterator();
    while (rrf_it.next()) |entry| {
        const id = entry.key_ptr.*;
        const rrf_score = entry.value_ptr.*;
        const vs = vec_scores.get(id);
        const ks = kw_scores.get(id);

        try results.append(allocator, .{
            .id = id,
            .vector_score = vs,
            .keyword_score = ks,
            .final_score = rrf_score,
        });
    }

    // Sort by RRF score descending
    std.mem.sortUnstable(ScoredResult, results.items, {}, struct {
        fn lessThan(_: void, lhs: ScoredResult, rhs: ScoredResult) bool {
            return lhs.final_score > rhs.final_score;
        }
    }.lessThan);

    const actual_limit = @min(limit, results.items.len);
    return allocator.dupe(ScoredResult, results.items[0..actual_limit]);
}

/// Merges three score vectors into a single result using allocator and parameters.
pub fn hybridMergeThree(
    allocator: std.mem.Allocator,
    vector_results: []const IdScore,
    keyword_results: []const IdScore,
    capability_results: []const IdScore,
    cosine_w: f32,
    keyword_w: f32,
    cap_w: f32,
    limit: usize,
) ![]ScoredResult {
    // Maps to accumulate RRF scores and track component scores
    var rrf_scores = std.AutoHashMap(i64, f32).init(allocator);
    defer rrf_scores.deinit();
    var vec_scores = std.AutoHashMap(i64, f32).init(allocator);
    defer vec_scores.deinit();
    var kw_scores = std.AutoHashMap(i64, f32).init(allocator);
    defer kw_scores.deinit();
    var cap_scores = std.AutoHashMap(i64, f32).init(allocator);
    defer cap_scores.deinit();

    // Process vector results with RRF
    for (vector_results, 0..) |vr, rank| {
        const rrf_score = cosine_w * (1.0 / (RRF_K + @as(f32, @floatFromInt(rank))));
        const entry = try rrf_scores.getOrPut(vr.id);
        if (!entry.found_existing) {
            entry.value_ptr.* = rrf_score;
        } else {
            entry.value_ptr.* += rrf_score;
        }

        const vs_entry = try vec_scores.getOrPut(vr.id);
        if (!vs_entry.found_existing or vr.score > vs_entry.value_ptr.*) {
            vs_entry.value_ptr.* = vr.score;
        }
    }

    // Process keyword results with RRF
    for (keyword_results, 0..) |kr, rank| {
        const rrf_score = keyword_w * (1.0 / (RRF_K + @as(f32, @floatFromInt(rank))));
        const entry = try rrf_scores.getOrPut(kr.id);
        if (!entry.found_existing) {
            entry.value_ptr.* = rrf_score;
        } else {
            entry.value_ptr.* += rrf_score;
        }

        const ks_entry = try kw_scores.getOrPut(kr.id);
        if (!ks_entry.found_existing or kr.score > ks_entry.value_ptr.*) {
            ks_entry.value_ptr.* = kr.score;
        }
    }

    // Process capability results with RRF
    for (capability_results, 0..) |cr, rank| {
        const rrf_score = cap_w * (1.0 / (RRF_K + @as(f32, @floatFromInt(rank))));
        const entry = try rrf_scores.getOrPut(cr.id);
        if (!entry.found_existing) {
            entry.value_ptr.* = rrf_score;
        } else {
            entry.value_ptr.* += rrf_score;
        }

        const cs_entry = try cap_scores.getOrPut(cr.id);
        if (!cs_entry.found_existing or cr.score > cs_entry.value_ptr.*) {
            cs_entry.value_ptr.* = cr.score;
        }
    }

    // Build scored results from RRF scores
    var results: std.ArrayList(ScoredResult) = .empty;
    defer results.deinit(allocator);

    var rrf_it = rrf_scores.iterator();
    while (rrf_it.next()) |entry| {
        const id = entry.key_ptr.*;
        const rrf_score = entry.value_ptr.*;
        const vs = vec_scores.get(id);
        const ks = kw_scores.get(id);
        const cs = cap_scores.get(id);

        try results.append(allocator, .{
            .id = id,
            .vector_score = vs,
            .keyword_score = ks,
            .capability_score = cs,
            .final_score = rrf_score,
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
