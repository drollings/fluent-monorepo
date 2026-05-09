//! Tests for extractor.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const extractor_mod = @import("extractor.zig");

test "CallExtractor: empty file produces no calls" {
    const allocator = std.testing.allocator;
    const source: [:0]const u8 = "";
    var extractor = try extractor_mod.CallExtractor.init(allocator, "test.zig", source);
    defer extractor.deinit();

    const calls = try extractor.extractAllCalls();
    defer {
        for (calls) |cs| {
            allocator.free(cs.caller_fn);
            allocator.free(cs.callee_name);
        }
        allocator.free(calls);
    }
    try std.testing.expectEqual(@as(usize, 0), calls.len);
}
test "CallExtractor: buildImportMap finds @import aliases" {
    const allocator = std.testing.allocator;
    const source: [:0]const u8 =
        \\const bar = @import("bar.zig");
        \\const std = @import("std");
        \\pub fn foo() void {}
    ;
    var extractor = try extractor_mod.CallExtractor.init(allocator, "foo.zig", source);
    defer extractor.deinit();

    try std.testing.expect(extractor.imports.contains("bar"));
    try std.testing.expect(extractor.imports.contains("std"));
}
test "CallExtractor: direct function calls extracted with high confidence" {
    const allocator = std.testing.allocator;
    const source: [:0]const u8 =
        \\pub fn outer() void {
        \\    inner();
        \\}
        \\fn inner() void {}
    ;
    var extractor = try extractor_mod.CallExtractor.init(allocator, "t.zig", source);
    defer extractor.deinit();

    const calls = try extractor.extractAllCalls();
    defer {
        for (calls) |cs| {
            allocator.free(cs.caller_fn);
            allocator.free(cs.callee_name);
        }
        allocator.free(calls);
    }

    // Should have found `inner()` call with high confidence.
    var found = false;
    for (calls) |cs| {
        if (std.mem.eql(u8, cs.callee_name, "inner")) {
            try std.testing.expectEqual(@TypeOf(cs.confidence).high, cs.confidence);
            found = true;
        }
    }
    try std.testing.expect(found);
}
