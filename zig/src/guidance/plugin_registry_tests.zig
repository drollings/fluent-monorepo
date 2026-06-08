//! Tests for plugin_registry.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const plugin_mod = @import("plugin.zig");
const plugin_registry_mod = @import("plugin_registry.zig");

test "PluginRegistry init registers Zig plugin" {
    var reg = plugin_registry_mod.PluginRegistry.init(std.testing.allocator);
    defer reg.deinit();

    const p = reg.getForExtension(".zig");
    try std.testing.expect(p != null);
    try std.testing.expectEqualStrings("zig", p.?.name);
}
test "PluginRegistry getForPath" {
    var reg = plugin_registry_mod.PluginRegistry.init(std.testing.allocator);
    defer reg.deinit();

    const p = reg.getForPath("src/foo/bar.zig");
    try std.testing.expect(p != null);
    try std.testing.expectEqualStrings("zig", p.?.name);

    // Markdown is registered.
    const md = reg.getForPath("README.md");
    try std.testing.expect(md != null);
    try std.testing.expectEqualStrings("markdown", md.?.name);

    // Unknown extension returns null.
    const none = reg.getForPath("archive.tar");
    try std.testing.expect(none == null);
}
test "PluginRegistry registeredLanguages" {
    var reg = plugin_registry_mod.PluginRegistry.init(std.testing.allocator);
    defer reg.deinit();

    const langs = try reg.registeredLanguages(std.testing.allocator);
    defer std.testing.allocator.free(langs);

    try std.testing.expect(langs.len >= 1);
    var found_zig = false;
    for (langs) |l| if (std.mem.eql(u8, l, "zig")) {
        found_zig = true;
    };
    try std.testing.expect(found_zig);
}
test "PluginRegistry register custom plugin" {
    var reg = plugin_registry_mod.PluginRegistry.init(std.testing.allocator);
    defer reg.deinit();

    const dummy_parse = struct {
        fn f(arena: std.mem.Allocator, src: [:0]const u8, path: []const u8) anyerror!plugin_mod.ParsedFile {
            _ = arena;
            _ = src;
            return .{ .module = path, .source = path, .language = "lua", .module_comment = null, .members = &.{} };
        }
    }.f;
    const dummy_imports = struct {
        fn f(arena: std.mem.Allocator, src: [:0]const u8) anyerror![]const []const u8 {
            _ = arena;
            _ = src;
            return &.{};
        }
    }.f;

    const ext = [_][]const u8{".lua"};
    try reg.register(std.testing.allocator, .{
        .name = "lua",
        .extensions = &ext,
        .parseFn = dummy_parse,
        .extractImportsFn = dummy_imports,
    });

    const p = reg.getForExtension(".lua");
    try std.testing.expect(p != null);
    try std.testing.expectEqualStrings("lua", p.?.name);
}
