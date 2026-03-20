/// source.zig — Source code excerpt extraction helpers
///
/// Centralises the repeated pattern of extracting relevant source code
/// excerpts from files for LLM context, documentation, and error messages.
const std = @import("std");

/// Default maximum lines for excerpt extraction.
pub const DEFAULT_MAX_LINES: usize = 200;

/// Node type classification for excerpt extraction.
pub const NodeType = enum {
    fn_decl,
    fn_private,
    method,
    method_private,
    struct_decl,
    enum_decl,
    union_decl,
    test_decl,
    enum_field,
    other,

    pub fn isFunction(self: NodeType) bool {
        return self == .fn_decl or self == .fn_private or
            self == .method or self == .method_private;
    }

    pub fn isContainer(self: NodeType) bool {
        return self == .struct_decl or self == .enum_decl or self == .union_decl;
    }

    pub fn fromString(node_type: []const u8) NodeType {
        if (std.mem.eql(u8, node_type, "fn_decl")) return .fn_decl;
        if (std.mem.eql(u8, node_type, "fn_private")) return .fn_private;
        if (std.mem.eql(u8, node_type, "method")) return .method;
        if (std.mem.eql(u8, node_type, "method_private")) return .method_private;
        if (std.mem.eql(u8, node_type, "struct")) return .struct_decl;
        if (std.mem.eql(u8, node_type, "enum")) return .enum_decl;
        if (std.mem.eql(u8, node_type, "union")) return .union_decl;
        if (std.mem.eql(u8, node_type, "test_decl")) return .test_decl;
        if (std.mem.eql(u8, node_type, "enum_field")) return .enum_field;
        return .other;
    }
};

/// Extract a source code excerpt from `src` starting at `start_line` (1-based).
///
/// For functions: extracts the entire function body using brace matching.
/// For containers (struct/enum/union): shows declaration and signatures only.
/// For other types: extracts up to `max_lines` lines (default: DEFAULT_MAX_LINES).
///
/// Returns an owned allocation; caller must free.
pub fn extractExcerpt(
    allocator: std.mem.Allocator,
    src: []const u8,
    start_line: u32,
    node_type: NodeType,
    max_lines: usize,
) ![]const u8 {
    var lines: std.ArrayList([]const u8) = .{};
    defer lines.deinit(allocator);

    var iter = std.mem.splitScalar(u8, src, '\n');
    var line_no: u32 = 0;
    var brace_depth: isize = 0;
    var started_scope: bool = false;
    var scope_start_depth: isize = 0;

    while (iter.next()) |raw| {
        line_no += 1;
        if (line_no < start_line) continue;

        const trimmed = std.mem.trimRight(u8, raw, "\r");
        const is_first = line_no == start_line;

        // Count braces in this line
        var line_brace_delta: isize = 0;
        var found_open = false;
        for (trimmed) |ch| {
            if (ch == '{') {
                line_brace_delta += 1;
                found_open = true;
            } else if (ch == '}') {
                line_brace_delta -= 1;
            }
        }

        // Start tracking when we first see an opening brace
        if (!started_scope and found_open) {
            started_scope = true;
            scope_start_depth = brace_depth + 1;
        }

        // Skip separator comments
        if (std.mem.startsWith(u8, std.mem.trimLeft(u8, trimmed, " \t"), "// ---")) continue;

        if (node_type.isContainer() and started_scope and brace_depth > scope_start_depth) {
            // Inside nested container - skip body content
            const stripped = std.mem.trimLeft(u8, trimmed, " \t");
            if (stripped.len > 0 and stripped[0] != '/' and stripped[0] != '*' and
                !std.mem.startsWith(u8, stripped, "pub ") and
                !std.mem.startsWith(u8, stripped, "fn ") and
                !std.mem.startsWith(u8, stripped, "const ") and
                !std.mem.startsWith(u8, stripped, "var ") and
                !std.mem.startsWith(u8, stripped, "//") and
                !std.mem.startsWith(u8, stripped, "///") and
                !std.mem.eql(u8, stripped, "},") and
                !std.mem.eql(u8, stripped, "}"))
            {
                brace_depth += line_brace_delta;
                continue;
            }
        }

        try lines.append(allocator, trimmed);
        brace_depth += line_brace_delta;

        // If we were in a scope and just closed back to where we started, we're done
        if (started_scope and brace_depth < scope_start_depth) {
            break;
        }

        // Stop at next top-level declaration (col 0) if we haven't started a scope yet
        if (!started_scope and !is_first and trimmed.len > 0 and trimmed[0] != ' ' and trimmed[0] != '\t') {
            if (std.mem.startsWith(u8, trimmed, "pub ") or
                std.mem.startsWith(u8, trimmed, "fn ") or
                std.mem.startsWith(u8, trimmed, "const ") or
                std.mem.startsWith(u8, trimmed, "var ") or
                std.mem.startsWith(u8, trimmed, "test ") or
                std.mem.startsWith(u8, trimmed, "///"))
            {
                _ = lines.pop();
                break;
            }
        }

        // For non-functions/containers, apply line limit
        if (!node_type.isFunction() and !node_type.isContainer() and lines.items.len >= max_lines) {
            break;
        }
    }

    // Prune trailing blank/comment-only lines
    while (lines.items.len > 0) {
        const last = lines.items[lines.items.len - 1];
        const trimmed_last = std.mem.trim(u8, last, " \t\r");
        if (trimmed_last.len == 0 or std.mem.startsWith(u8, trimmed_last, "//")) {
            _ = lines.pop();
        } else {
            break;
        }
    }

    if (lines.items.len == 0) return allocator.dupe(u8, "");

    var buf: std.ArrayList(u8) = .{};
    errdefer buf.deinit(allocator);
    for (lines.items, 0..) |line, idx| {
        if (idx > 0) try buf.append(allocator, '\n');
        try buf.appendSlice(allocator, line);
    }
    return buf.toOwnedSlice(allocator);
}

/// Extract up to `max_lines` source lines starting at `start_line` (1-based).
/// Stops at the next top-level pub/fn/const/var declaration or at `max_lines`.
/// This is a simpler extraction for quick lookups without brace-awareness.
/// Returns an owned allocation; caller must free.
pub fn extractSimpleExcerpt(
    allocator: std.mem.Allocator,
    src_content: []const u8,
    start_line: usize,
    max_lines: usize,
) []const u8 {
    var lines_iter = std.mem.splitScalar(u8, src_content, '\n');
    var line_idx: usize = 0;
    const target_start = if (start_line > 0) start_line - 1 else 0;
    while (line_idx < target_start) : (line_idx += 1) {
        _ = lines_iter.next() orelse return allocator.dupe(u8, "") catch return "";
    }

    var buf: std.ArrayList(u8) = .{};
    var captured: usize = 0;
    var first = true;
    var saw_close_brace = false;
    while (lines_iter.next()) |line| {
        if (captured >= max_lines) break;
        const trimmed_line = std.mem.trimRight(u8, line, "\r");
        if (!first and trimmed_line.len > 0 and trimmed_line[0] != ' ' and trimmed_line[0] != '\t') {
            if (std.mem.startsWith(u8, trimmed_line, "pub ") or
                std.mem.startsWith(u8, trimmed_line, "fn ") or
                std.mem.startsWith(u8, trimmed_line, "const ") or
                std.mem.startsWith(u8, trimmed_line, "var ") or
                std.mem.startsWith(u8, trimmed_line, "// =") or
                std.mem.startsWith(u8, trimmed_line, "// -") or
                (saw_close_brace and std.mem.startsWith(u8, trimmed_line, "//")) or
                (saw_close_brace and trimmed_line.len == 0))
            {
                break;
            }
        }
        if (!first and std.mem.eql(u8, std.mem.trim(u8, trimmed_line, " \t"), "};")) {
            saw_close_brace = true;
        }
        if (std.mem.startsWith(u8, std.mem.trimLeft(u8, trimmed_line, " \t"), "// ---")) {
            captured += 1;
            first = false;
            continue;
        }
        buf.appendSlice(allocator, line) catch return "";
        buf.append(allocator, '\n') catch return "";
        captured += 1;
        first = false;
    }

    const raw = buf.toOwnedSlice(allocator) catch return "";
    const trimmed = std.mem.trimRight(u8, raw, " \t\r\n");
    defer allocator.free(raw);
    return allocator.dupe(u8, trimmed) catch "";
}

// =============================================================================
// Tests
// =============================================================================

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

    const result = try extractExcerpt(std.testing.allocator, src, 3, .fn_decl, 80);
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

    const result = try extractExcerpt(std.testing.allocator, src, 3, .struct_decl, 80);
    defer std.testing.allocator.free(result);

    try std.testing.expect(std.mem.indexOf(u8, result, "pub const Point = struct {") != null);
}

test "extractSimpleExcerpt respects line limit" {
    const src = "line1\nline2\nline3\nline4\nline5";
    const result = extractSimpleExcerpt(std.testing.allocator, src, 1, 3);
    defer std.testing.allocator.free(result);

    try std.testing.expectEqualStrings("line1\nline2\nline3", result);
}

test "NodeType.fromString" {
    try std.testing.expect(NodeType.fromString("fn_decl") == .fn_decl);
    try std.testing.expect(NodeType.fromString("struct") == .struct_decl);
    try std.testing.expect(NodeType.fromString("unknown") == .other);
}

test "NodeType.isFunction and isContainer" {
    try std.testing.expect(NodeType.fn_decl.isFunction());
    try std.testing.expect(!NodeType.fn_decl.isContainer());
    try std.testing.expect(NodeType.struct_decl.isContainer());
    try std.testing.expect(!NodeType.struct_decl.isFunction());
}
