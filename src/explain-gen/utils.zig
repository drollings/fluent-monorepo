// Utility functions for explain-gen, adapted from ast-guidance (MIT License)
// Copyright (c) 2024 Daniel Richards
// See: https://github.com/anomalyco/ast-guidance

const std = @import("std");

/// Extract up to `max_lines` source lines starting at `start_line` (1-based),
/// stopping at the next top-level pub/fn declaration or 80 lines, whichever comes first.
pub fn extractSourceExcerpt(
    allocator: std.mem.Allocator,
    src_content: []const u8,
    start_line: usize,
    max_lines: usize,
) []const u8 {
    var lines_iter = std.mem.splitScalar(u8, src_content, '\n');
    var line_idx: usize = 0;
    // Advance to start_line (1-based → 0-based index = start_line - 1).
    const target_start = if (start_line > 0) start_line - 1 else 0;
    while (line_idx < target_start) : (line_idx += 1) {
        _ = lines_iter.next() orelse return allocator.dupe(u8, "");
    }

    var buf: std.ArrayList(u8) = .{};
    var captured: usize = 0;
    var first = true;
    var saw_close_brace = false; // tracks whether we've emitted a col-0 closing }
    while (lines_iter.next()) |line| {
        if (captured >= max_lines) break;
        const trimmed_line = std.mem.trimRight(u8, line, "\r");
        // Stop at next top-level declaration at column 0 (not on the very first line).
        if (!first and trimmed_line.len > 0 and trimmed_line[0] != ' ' and trimmed_line[0] != '\t') {
            if (std.mem.startsWith(u8, trimmed_line, "pub ") or
                std.mem.startsWith(u8, trimmed_line, "fn ") or
                std.mem.startsWith(u8, trimmed_line, "const ") or
                std.mem.startsWith(u8, trimmed_line, "var ") or
                std.mem.startsWith(u8, trimmed_line, "// =") or
                std.mem.startsWith(u8, trimmed_line, "// -") or
                // A col-0 doc-comment or plain comment after we've seen the closing brace
                (saw_close_brace and std.mem.startsWith(u8, trimmed_line, "//")) or
                (saw_close_brace and trimmed_line.len == 0))
            {
                break;
            }
        }
        // Track col-0 closing brace (end of top-level block).
        if (!first and std.mem.eql(u8, std.mem.trim(u8, trimmed_line, " \t"), "};")) {
            saw_close_brace = true;
        }
        // Skip separator banners.
        if (std.mem.startsWith(u8, std.mem.trimLeft(u8, trimmed_line, " \t"), "// ---")) {
            captured += 1;
            first = false;
            continue;
        }
        try buf.appendSlice(allocator, line);
        try buf.append(allocator, '\n');
        captured += 1;
        first = false;
    }

    // Strip trailing blank lines.
    const raw = buf.toOwnedSlice(allocator) catch return allocator.dupe(u8, "");
    const trimmed = std.mem.trimRight(u8, raw, " \t\r\n");
    defer allocator.free(raw);
    return allocator.dupe(u8, trimmed);
}

/// Grep a file for any of the search terms (case-insensitive substring).
/// Returns up to `max_results` matches, skipping pure comment lines.
pub fn grepFile(
    allocator: std.mem.Allocator,
    file_path: []const u8,
    terms: [][]const u8,
    max_results: usize,
) []struct {
    line_no: usize,
    line: []const u8,
} {
    const file = std.fs.openFileAbsolute(file_path, .{}) catch return &.{};
    defer file.close();
    const content = file.readToEndAlloc(allocator, 10 * 1024 * 1024) catch return &.{};
    defer allocator.free(content);

    var results: std.ArrayList(struct {
        line_no: usize,
        line: []const u8,
    }) = .{};
    var lines_iter = std.mem.splitScalar(u8, content, '\n');
    var line_no: usize = 0;
    while (lines_iter.next()) |line| {
        line_no += 1;
        if (results.items.len >= max_results) break;
        const trimmed = std.mem.trimLeft(u8, line, " \t");
        // Skip pure comment lines.
        if (std.mem.startsWith(u8, trimmed, "//") or std.mem.startsWith(u8, trimmed, "#")) continue;
        const line_lower = try std.ascii.allocLowerString(allocator, line);
        defer allocator.free(line_lower);
        for (terms) |term| {
            if (std.mem.indexOf(u8, line_lower, term) != null) {
                try results.append(allocator, .{
                    .line_no = line_no,
                    .line = try allocator.dupe(u8, std.mem.trimRight(u8, line, "\r")),
                });
                break;
            }
        }
    }
    return results.toOwnedSlice(allocator);
}
