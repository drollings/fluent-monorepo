//! Tests for document_indexer.zig.
const std = @import("std");
const types = @import("types.zig");
const document_indexer_mod = @import("document_indexer.zig");

test "GuidanceJsonIndexer: docType returns guidance_json" {
    const allocator = std.testing.allocator;
    const doc = types.GuidanceDoc{ .arena = undefined,
        .meta = .{ .module = "test", .source = "src/test.zig" },
        .comment = "Test module",
        .detail = "A comprehensive test module with many features.",
        .keywords = &.{ "test", "unit" },
    };
    const indexer = try document_indexer_mod.createIndexer(allocator, &doc, "/tmp");
    defer indexer.deinit();

    try std.testing.expectEqualStrings("guidance_json", indexer.docType());
    try std.testing.expectEqualStrings("src/test.zig", indexer.docId());
}

test "GuidanceJsonIndexer: produceStages includes detail prose" {
    const allocator = std.testing.allocator;
    const doc = types.GuidanceDoc{ .arena = undefined,
        .meta = .{ .module = "test", .source = "src/test.zig" },
        .detail = "A comprehensive test module that does many interesting things and has lots of content.",
        .keywords = &.{"test"},
    };
    const indexer = try document_indexer_mod.createIndexer(allocator, &doc, "/tmp");
    defer indexer.deinit();

    const stages = try indexer.produceStages(allocator, "test", 1.0);
    defer {
        types.freeStages(allocator, stages);
        allocator.free(stages);
    }

    try std.testing.expect(stages.len >= 1);
    try std.testing.expectEqual(types.StageKind.prose, stages[0].kind);
}
