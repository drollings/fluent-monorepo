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

/// Transforms the provided Zig code snippet into a LanguagePlugin instance.
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

/// Converts a Zig source file into a parsed Zig data structure, handling errors gracefully.
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

/// Extracts Zig import addresses from a source string, returning an array of byte slices.
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

/// Derives a Zig module slice from a given file path and allocator.
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

test "deriveModule strips extension and converts slashes" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const m = try deriveModule(arena.allocator(), "src/foo/bar.zig");
    try std.testing.expectEqualStrings("src.foo.bar", m);

    const m2 = try deriveModule(arena.allocator(), "./src/main.zig");
    try std.testing.expectEqualStrings("src.main", m2);
}
