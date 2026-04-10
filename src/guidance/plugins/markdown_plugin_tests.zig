//! Tests for markdown_plugin.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const types = @import("../types.zig");
const markdown_plugin_mod = @import("markdown_plugin.zig");

test "MarkdownPlugin handles .md extension" {
    const p = markdown_plugin_mod.plugin();
    try std.testing.expectEqualStrings("markdown", p.name);
    var found = false;
    for (p.extensions) |ext| if (std.mem.eql(u8, ext, ".md")) {
        found = true;
    };
    try std.testing.expect(found);
}
test "MarkdownPlugin.parse extracts headings as members" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    // Build the source string explicitly to avoid multiline literal edge cases.
    const src_str = "# My Project\n\nA short description of the project.\n\n## Installation\n\nSteps to install.\n\n## Usage\n\nHow to use it.\n\n### Advanced\n\nAdvanced topics.\n";
    const src = try std.fmt.allocPrintSentinel(arena.allocator(), "{s}", .{src_str}, 0);

    const p = markdown_plugin_mod.plugin();
    const result = try p.parse(arena.allocator(), src, "README.md");

    try std.testing.expectEqualStrings("markdown", result.language);
    try std.testing.expectEqualStrings("README", result.module);
    // First paragraph after h1 heading.
    try std.testing.expect(result.module_comment != null);
    try std.testing.expect(std.mem.indexOf(u8, result.module_comment.?, "description") != null);

    // Members: Installation, Usage, Advanced (h1 is skipped as module title).
    try std.testing.expect(result.members.len >= 3);
    try std.testing.expectEqualStrings("Installation", result.members[0].name);
    try std.testing.expectEqualStrings("Usage", result.members[1].name);
    try std.testing.expectEqualStrings("Advanced", result.members[2].name);
}
test "MarkdownPlugin.parse code blocks are skipped" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const src: [:0]const u8 =
        \\# Title
        \\
        \\```
        \\## Not a heading
        \\```
        \\
        \\## Real heading
    ;
    const p = markdown_plugin_mod.plugin();
    const result = try p.parse(arena.allocator(), src, "doc.md");
    // Only "Real heading" should be a member.
    try std.testing.expectEqual(@as(usize, 1), result.members.len);
    try std.testing.expectEqualStrings("Real heading", result.members[0].name);
}
test "MarkdownPlugin.extractImports finds local links" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const src: [:0]const u8 =
        \\See [installation](docs/install.md) and [API](api/reference.md).
        \\Also [external](https://example.com) which should be skipped.
    ;
    const p = markdown_plugin_mod.plugin();
    const links = try p.extractImports(arena.allocator(), src);

    try std.testing.expectEqual(@as(usize, 2), links.len);
    try std.testing.expectEqualStrings("docs/install.md", links[0]);
    try std.testing.expectEqualStrings("api/reference.md", links[1]);
}
test "FileType.fromExtension" {
    try std.testing.expectEqual(types.FileType.source, types.FileType.fromExtension(".zig"));
    try std.testing.expectEqual(types.FileType.source, types.FileType.fromExtension(".py"));
    try std.testing.expectEqual(types.FileType.markdown, types.FileType.fromExtension(".md"));
    try std.testing.expectEqual(types.FileType.config, types.FileType.fromExtension(".toml"));
    try std.testing.expectEqual(types.FileType.unknown, types.FileType.fromExtension(".xyz"));
}
