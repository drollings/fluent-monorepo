//! Tests for header.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const types = @import("../types.zig");
const header_mod = @import("header.zig");

test "sourceHasModuleDoc - with header" {
    const source = "//! My module.\nconst x = 1;\n";
    try std.testing.expect(header_mod.sourceHasModuleDoc(source));
}
test "sourceHasModuleDoc - without header" {
    const source = "const x = 1;\n";
    try std.testing.expect(!header_mod.sourceHasModuleDoc(source));
}
test "generateFileHeader - returns null when header exists" {
    const allocator = std.testing.allocator;
    const source = "//! Already has header.\nconst x = 1;\n";
    const result = try header_mod.generateFileHeader(allocator, "src/foo.zig", &.{}, source);
    try std.testing.expect(result == null);
}
test "generateFileHeader - generates header for new file" {
    const allocator = std.testing.allocator;
    const members = [_]types.Member{
        .{ .type = .fn_decl, .name = "doSomething", .is_pub = true },
        .{ .type = .fn_private, .name = "helper", .is_pub = false },
    };
    const result = try header_mod.generateFileHeader(allocator, "src/foo.zig", &members, "const x = 1;\n");
    defer if (result) |r| allocator.free(r);

    try std.testing.expect(result != null);
    const h = result.?;
    try std.testing.expect(std.mem.indexOf(u8, h, "//!") != null);
    try std.testing.expect(std.mem.indexOf(u8, h, "doSomething") != null);
}
test "insertFileHeader - prepends header" {
    const allocator = std.testing.allocator;
    const source = "const x = 1;\n";
    const header = "//! My module.\n";
    const result = try header_mod.insertFileHeader(allocator, source, header);
    defer allocator.free(result);
    try std.testing.expectEqualStrings("//! My module.\nconst x = 1;\n", result);
}
