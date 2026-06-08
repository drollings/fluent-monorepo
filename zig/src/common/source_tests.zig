//! Tests for source.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const source_mod = @import("source.zig");

test "extractExcerpt extracts function body" {
    const src =
        \\const std = @import("std");
        \\
        \\pub fn hello() void {
        \\    std.debug.print("Hello!", .{});
        \\}
        \\
        \\pub fn other() void {}
    ;

    const result = try source_mod.extractExcerpt(std.testing.allocator, src, 3, .fn_decl, 80);
    defer std.testing.allocator.free(result);

    try std.testing.expect(std.mem.indexOf(u8, result, "pub fn hello() void {") != null);
    try std.testing.expect(std.mem.indexOf(u8, result, "}") != null);
    try std.testing.expect(std.mem.indexOf(u8, result, "other") == null);
}
test "extractExcerpt extracts container declaration" {
    const src =
        \\const std = @import("std");
        \\
        \\pub const Point = struct {
        \\    x: f32,
        \\    y: f32,
        \\
        \\    pub fn new(x: f32, y: f32) Point {
        \\        return .{ .x = x, .y = y };
        \\    }
        \\};
    ;

    const result = try source_mod.extractExcerpt(std.testing.allocator, src, 3, .struct_decl, 80);
    defer std.testing.allocator.free(result);

    try std.testing.expect(std.mem.indexOf(u8, result, "pub const Point = struct {") != null);
}
test "extractSimpleExcerpt respects line limit" {
    const src = "line1\nline2\nline3\nline4\nline5";
    const result = source_mod.extractSimpleExcerpt(std.testing.allocator, src, 1, 3);
    defer std.testing.allocator.free(result);

    try std.testing.expectEqualStrings("line1\nline2\nline3", result);
}
test "NodeType.fromString" {
    try std.testing.expect(source_mod.NodeType.fromString("fn_decl") == .fn_decl);
    try std.testing.expect(source_mod.NodeType.fromString("struct") == .struct_decl);
    try std.testing.expect(source_mod.NodeType.fromString("unknown") == .other);
}
test "NodeType.isFunction and isContainer" {
    try std.testing.expect(source_mod.NodeType.fn_decl.isFunction());
    try std.testing.expect(!source_mod.NodeType.fn_decl.isContainer());
    try std.testing.expect(source_mod.NodeType.struct_decl.isContainer());
    try std.testing.expect(!source_mod.NodeType.struct_decl.isFunction());
}
