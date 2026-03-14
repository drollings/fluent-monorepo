//! ZigPlugin — wraps ast_parser.zig as a LanguagePlugin.
//!
//! Handles .zig and .zon files.  Parses with Zig's stdlib AST, extracting
//! top-level declarations and imports.

const std = @import("std");
const types = @import("../types.zig");
const ast_parser = @import("../ast_parser.zig");
const plugin_mod = @import("../plugin.zig");

const LanguagePlugin = plugin_mod.LanguagePlugin;
const ParsedFile = plugin_mod.ParsedFile;

/// File extensions handled by this plugin.
const EXTENSIONS = [_][]const u8{ ".zig", ".zon" };

/// Returns the singleton ZigPlugin descriptor.
pub fn plugin() LanguagePlugin {
    return .{
        .name = "zig",
        .extensions = &EXTENSIONS,
        .parseFn = parseZig,
        .extractImportsFn = extractZigImports,
    };
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

fn parseZig(
    arena: std.mem.Allocator,
    source: [:0]const u8,
    file_path: []const u8,
) anyerror!ParsedFile {
    var parser = try ast_parser.AstParser.init(arena, source);
    defer parser.deinit();

    // Module name: convert path separators to dots, strip extension.
    const module = try deriveModule(arena, file_path);

    // Source-relative path: strip leading "./" if present.
    const source_path = if (std.mem.startsWith(u8, file_path, "./"))
        file_path[2..]
    else
        file_path;

    const members = try parser.extractMembers();

    return ParsedFile{
        .module = module,
        .source = source_path,
        .language = "zig",
        .module_comment = null, // doc-comment extraction left to SyncProcessor
        .members = members,
    };
}

fn extractZigImports(
    arena: std.mem.Allocator,
    source: [:0]const u8,
) anyerror![]const []const u8 {
    var parser = try ast_parser.AstParser.init(arena, source);
    defer parser.deinit();

    const raw = try parser.extractImports();
    // extractImports returns [][]u8; return as []const []const u8.
    return @as([]const []const u8, raw);
}

/// Convert a file path to a dot-separated module name.
/// "src/foo/bar.zig" → "src.foo.bar"
fn deriveModule(arena: std.mem.Allocator, file_path: []const u8) ![]const u8 {
    // Strip leading "./"
    var path = file_path;
    if (std.mem.startsWith(u8, path, "./")) path = path[2..];

    // Strip extension.
    if (std.mem.lastIndexOfScalar(u8, path, '.')) |dot| {
        path = path[0..dot];
    }

    // Replace '/' and '\' with '.'.
    const out = try arena.dupe(u8, path);
    for (out) |*ch| {
        if (ch.* == '/' or ch.* == std.fs.path.sep) ch.* = '.';
    }
    return out;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test "ZigPlugin handles .zig extension" {
    const p = plugin();
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
    const p = plugin();
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
    const p = plugin();
    const imports = try p.extractImports(arena.allocator(), src);
    try std.testing.expect(imports.len >= 1);

    var found_std = false;
    for (imports) |imp| {
        if (std.mem.eql(u8, imp, "std")) found_std = true;
    }
    try std.testing.expect(found_std);
}

test "deriveModule strips extension and converts slashes" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const m = try deriveModule(arena.allocator(), "src/foo/bar.zig");
    try std.testing.expectEqualStrings("src.foo.bar", m);

    const m2 = try deriveModule(arena.allocator(), "./src/main.zig");
    try std.testing.expectEqualStrings("src.main", m2);
}
