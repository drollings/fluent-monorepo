//! Tests for plugin.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const plugin_mod = @import("plugin.zig");

test "LanguagePlugin interface is callable" {
    // Verify the struct layout compiles and function pointers are non-null.
    const dummy_parse = struct {
        fn parse(
            arena: std.mem.Allocator,
            source: [:0]const u8,
            file_path: []const u8,
        ) anyerror!plugin_mod.ParsedFile {
            _ = arena;
            _ = source;
            return plugin_mod.ParsedFile{
                .module = file_path,
                .source = file_path,
                .language = "test",
                .module_comment = null,
                .members = &.{},
            };
        }
    }.parse;

    const dummy_imports = struct {
        fn extract(
            arena: std.mem.Allocator,
            source: [:0]const u8,
        ) anyerror![]const []const u8 {
            _ = arena;
            _ = source;
            return &.{};
        }
    }.extract;

    const plugin = plugin_mod.LanguagePlugin{
        .name = "test",
        .extensions = &.{".test"},
        .parseFn = dummy_parse,
        .extractImportsFn = dummy_imports,
    };

    try std.testing.expectEqualStrings("test", plugin.name);
    try std.testing.expectEqual(@as(usize, 1), plugin.extensions.len);
}
