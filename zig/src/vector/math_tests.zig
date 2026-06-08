//! Tests for math.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const math_mod = @import("math.zig");

test "cosine identical vectors" {
    const v = [_]f32{ 1.0, 2.0, 3.0 };
    const sim = math_mod.cosineSimilarity(&v, &v);
    try std.testing.expect(@abs(sim - 1.0) < 0.001);
}
test "cosine orthogonal vectors" {
    const a = [_]f32{ 1.0, 0.0, 0.0 };
    const b = [_]f32{ 0.0, 1.0, 0.0 };
    const sim = math_mod.cosineSimilarity(&a, &b);
    try std.testing.expect(@abs(sim) < 0.001);
}
test "cosine empty returns zero" {
    const empty: []const f32 = &.{};
    try std.testing.expectEqual(@as(f32, 0.0), math_mod.cosineSimilarity(empty, empty));
}
test "vec bytes roundtrip" {
    const original = [_]f32{ 1.0, -2.5, 3.14, 0.0 };
    const bytes = try math_mod.vecToBytes(std.testing.allocator, &original);
    defer std.testing.allocator.free(bytes);

    const restored = try math_mod.bytesToVec(std.testing.allocator, bytes);
    defer std.testing.allocator.free(restored);

    try std.testing.expectEqual(@as(usize, 4), restored.len);
    for (original, restored) |a, b_val| {
        try std.testing.expect(@abs(a - b_val) < std.math.floatEps(f32));
    }
}
test "hybrid merge deduplicates" {
    const vec_results = [_]math_mod.IdScore{.{ .id = 1, .score = 0.9 }};
    const kw_results = [_]math_mod.IdScore{.{ .id = 1, .score = 10.0 }};
    const merged = try math_mod.hybridMerge(std.testing.allocator, &vec_results, &kw_results, 0.7, 0.3, 10);
    defer std.testing.allocator.free(merged);

    try std.testing.expectEqual(@as(usize, 1), merged.len);
    try std.testing.expectEqual(@as(i64, 1), merged[0].id);
    try std.testing.expect(merged[0].vector_score != null);
    try std.testing.expect(merged[0].keyword_score != null);
}
test "hybrid merge respects limit" {
    var vec_results: [20]math_mod.IdScore = undefined;
    for (0..20) |i| {
        vec_results[i] = .{ .id = @intCast(i), .score = 1.0 - @as(f32, @floatFromInt(i)) * 0.05 };
    }
    const merged = try math_mod.hybridMerge(std.testing.allocator, &vec_results, &.{}, 1.0, 0.0, 5);
    defer std.testing.allocator.free(merged);
    try std.testing.expectEqual(@as(usize, 5), merged.len);
}
test "hybrid merge three-way combines all scores" {
    const vec_results = [_]math_mod.IdScore{.{ .id = 1, .score = 0.8 }};
    const kw_results = [_]math_mod.IdScore{.{ .id = 1, .score = 5.0 }};
    const cap_results = [_]math_mod.IdScore{.{ .id = 1, .score = 2.0 }};
    const merged = try math_mod.hybridMergeThree(std.testing.allocator, &vec_results, &kw_results, &cap_results, 0.6, 0.25, 0.15, 10);
    defer std.testing.allocator.free(merged);

    try std.testing.expectEqual(@as(usize, 1), merged.len);
    try std.testing.expectEqual(@as(i64, 1), merged[0].id);
    try std.testing.expect(merged[0].vector_score != null);
    try std.testing.expect(merged[0].keyword_score != null);
    try std.testing.expect(merged[0].capability_score != null);
    try std.testing.expect(@abs(merged[0].vector_score.? - 0.8) < 0.001);
    try std.testing.expect(@abs(merged[0].keyword_score.? - 5.0) < 0.001);
    try std.testing.expect(@abs(merged[0].capability_score.? - 2.0) < 0.001);
    // RRF: at rank 0 with RRF_K=60: 0.6/60 + 0.25/60 + 0.15/60 = 0.01 + 0.004167 + 0.0025 ≈ 0.016667
    try std.testing.expect(@abs(merged[0].final_score - 0.016667) < 0.001);
}
