/// header_generator.zig — File header comment generation for guidance.
///
/// Generates `//!`-style module doc comments for Zig source files.
/// These replace the `detail` field in JSON files as the authoritative
/// source of module-level documentation.
const std = @import("std");
const types = @import("types.zig");

/// Generates a Zig file header slice using provided allocator, path, members, and preview data.
pub fn generateFileHeader(
    allocator: std.mem.Allocator,
    rel_path: []const u8,
    members: []const types.Member,
    source_preview: []const u8,
) !?[]const u8 {
    // Do not generate a header when the file already has //! comments.
    if (sourceHasModuleDoc(source_preview)) return null;

    // Build a minimal header from available information.
    var out: std.ArrayList(u8) = .{};
    errdefer out.deinit(allocator);
    const w = out.writer(allocator);

    // Module identifier from path.
    const basename = std.fs.path.basename(rel_path);
    const stem = if (std.mem.lastIndexOfScalar(u8, basename, '.')) |dot|
        basename[0..dot]
    else
        basename;

    try w.print("//! {s} — ", .{stem});

    // Count public members to give a brief description.
    var pub_fns: usize = 0;
    var pub_structs: usize = 0;
    for (members) |m| {
        if (!m.is_pub) continue;
        switch (m.type) {
            .fn_decl => pub_fns += 1,
            .@"struct", .@"enum", .@"union" => pub_structs += 1,
            else => {},
        }
    }

    if (pub_fns > 0 and pub_structs > 0) {
        try w.print("{} public function(s), {} public type(s).", .{ pub_fns, pub_structs });
    } else if (pub_fns > 0) {
        try w.print("{} public function(s).", .{pub_fns});
    } else if (pub_structs > 0) {
        try w.print("{} public type(s).", .{pub_structs});
    } else {
        try w.writeAll("module.");
    }
    try w.writeByte('\n');

    // List public member names for discoverability.
    var listed: usize = 0;
    for (members) |m| {
        if (!m.is_pub) continue;
        if (listed == 0) try w.writeAll("//!\n//! Public API:\n");
        try w.print("//!   {s}\n", .{m.name});
        listed += 1;
        if (listed >= 10) {
            try w.writeAll("//!   ...\n");
            break;
        }
    }

    return @as(?[]const u8, try out.toOwnedSlice(allocator));
}

/// Inserts a file header into a Zig source file using provided allocator and data.
pub fn insertFileHeader(
    allocator: std.mem.Allocator,
    source: []const u8,
    header: []const u8,
) ![]const u8 {
    var out: std.ArrayList(u8) = .{};
    errdefer out.deinit(allocator);
    try out.appendSlice(allocator, header);
    if (!std.mem.endsWith(u8, header, "\n")) try out.append(allocator, '\n');
    try out.appendSlice(allocator, source);
    return out.toOwnedSlice(allocator);
}

/// Checks if a Zig source contains a module documentation string and returns a boolean.
pub fn sourceHasModuleDoc(source: []const u8) bool {
    var iter = std.mem.splitScalar(u8, source, '\n');
    while (iter.next()) |line| {
        const trimmed = std.mem.trim(u8, line, " \t");
        if (trimmed.len == 0) continue;
        return std.mem.startsWith(u8, trimmed, "//!");
    }
    return false;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test "sourceHasModuleDoc - with header" {
    const source = "//! My module.\nconst x = 1;\n";
    try std.testing.expect(sourceHasModuleDoc(source));
}

test "sourceHasModuleDoc - without header" {
    const source = "const x = 1;\n";
    try std.testing.expect(!sourceHasModuleDoc(source));
}

test "generateFileHeader - returns null when header exists" {
    const allocator = std.testing.allocator;
    const source = "//! Already has header.\nconst x = 1;\n";
    const result = try generateFileHeader(allocator, "src/foo.zig", &.{}, source);
    try std.testing.expect(result == null);
}

test "generateFileHeader - generates header for new file" {
    const allocator = std.testing.allocator;
    const members = [_]types.Member{
        .{ .type = .fn_decl, .name = "doSomething", .is_pub = true },
        .{ .type = .fn_private, .name = "helper", .is_pub = false },
    };
    const result = try generateFileHeader(allocator, "src/foo.zig", &members, "const x = 1;\n");
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
    const result = try insertFileHeader(allocator, source, header);
    defer allocator.free(result);
    try std.testing.expectEqualStrings("//! My module.\nconst x = 1;\n", result);
}
