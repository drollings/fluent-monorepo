//! Tests for stage_builder.zig.
const std = @import("std");
const types = @import("types.zig");
const stage_builder_mod = @import("stage_builder.zig");

test "GuidanceJsonStageBuilder: stageCount returns non-zero for doc with detail" {
    const allocator = std.testing.allocator;
    const doc = types.GuidanceDoc{
        .meta = .{ .module = "test", .source = "src/test.zig" },
        .detail = "A comprehensive module with many features and functions.",
        .comment = "Short comment.",
        .keywords = &.{"test"},
    };
    const builder = try stage_builder_mod.createStageBuilder(allocator, &doc, "/tmp");
    defer builder.deinit();

    try std.testing.expect(builder.stageCount() >= 1);
}

test "GuidanceJsonStageBuilder: buildStages produces prose stage from detail" {
    const allocator = std.testing.allocator;
    const doc = types.GuidanceDoc{
        .meta = .{ .module = "test", .source = "src/test.zig" },
        .detail = "A comprehensive module that provides many interesting features for testing.",
        .keywords = &.{"test"},
    };
    const builder = try stage_builder_mod.createStageBuilder(allocator, &doc, "/tmp");
    defer builder.deinit();

    const stages = try builder.buildStages(allocator);
    defer {
        types.freeStages(allocator, stages);
        allocator.free(stages);
    }

    try std.testing.expect(stages.len >= 1);
    try std.testing.expectEqual(types.StageKind.prose, stages[0].kind);
}

test "GuidanceJsonStageBuilder: isRelevant matches source path token" {
    const allocator = std.testing.allocator;
    const doc = types.GuidanceDoc{
        .meta = .{ .module = "src.vector.hnsw", .source = "src/vector/hnsw.zig" },
    };
    const builder = try stage_builder_mod.createStageBuilder(allocator, &doc, "/tmp");
    defer builder.deinit();

    const tokens = [_][]const u8{"hnsw"};
    try std.testing.expect(builder.isRelevant(&tokens));

    const no_tokens = [_][]const u8{"triage"};
    try std.testing.expect(!builder.isRelevant(&no_tokens));
}
