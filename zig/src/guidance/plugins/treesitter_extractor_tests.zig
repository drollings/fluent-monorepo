//! Tests for treesitter_extractor.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const treesitter_extractor_mod = @import("treesitter_extractor.zig");

test "member extractor" {
    const allocator = std.testing.allocator;

    // Test Python extraction
    const py_source =
        \\def foo():
        \\    pass
        \\
        \\class Bar:
        \\    pass
        \\
    ;

    var extractor = treesitter_extractor_mod.MemberExtractor.init(allocator, "python", py_source);
    defer extractor.deinit();

    // Note: This test requires actual tree-sitter parser to be set up
    // Full integration testing will be done in plugin_tests.zig
}
