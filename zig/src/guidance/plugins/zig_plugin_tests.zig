//! Tests for zig_plugin.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const zig_plugin_mod = @import("zig_plugin.zig");

test "ZigPlugin handles .zig extension" {
    const p = zig_plugin_mod.plugin();
    try std.testing.expectEqualStrings("zig", p.name);
    var found = false;
    for (p.extensions) |ext| {
        if (std.mem.eql(u8, ext, ".zig")) found = true;
    }
    try std.testing.expect(found);
}
test "ZigPlugin.parse extracts members from simple source" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const src: [:0]const u8 =
        \\pub fn add(a: u32, b: u32) u32 {
        \\    return a + b;
        \\}
        \\pub const LIMIT: u32 = 100;
    ;
    const p = zig_plugin_mod.plugin();
    const result = try p.parse(arena.allocator(), src, "src/math.zig");

    try std.testing.expectEqualStrings("zig", result.language);
    try std.testing.expectEqualStrings("src.math", result.module);
    try std.testing.expect(result.members.len >= 1);

    // Find the `add` function.
    var found_add = false;
    for (result.members) |m| {
        if (std.mem.eql(u8, m.name, "add")) found_add = true;
    }
    try std.testing.expect(found_add);
}
test "ZigPlugin.extractImports returns import paths" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const src: [:0]const u8 =
        \\const std = @import("std");
        \\const foo = @import("foo.zig");
    ;
    const p = zig_plugin_mod.plugin();
    const imports = try p.extractImports(arena.allocator(), src);
    try std.testing.expect(imports.len >= 1);

    var found_std = false;
    for (imports) |imp| {
        if (std.mem.eql(u8, imp, "std")) found_std = true;
    }
    try std.testing.expect(found_std);
}
