//! core/excerpt.zig — Unified source excerpt extraction.
//!
//! Consolidates:
//!   - query_engine.zig:explainExtractExcerpt()
//!   - query_engine.zig:explainGrepFile()
//!   - staged.zig:extractExcerptFromSource()
//!   - staged.zig:extractSourceExcerpt()
//!   - staged.zig:extractSourceExcerptVerified()
//!
//! All returned slices are allocator-owned; callers must free.

const std = @import("std");
const common = @import("common");
const types_mod = @import("../types.zig");
const line_verify = @import("../sync/line_verify.zig");

/// Extract a code excerpt from a source string.
/// Returns an owned mutable slice; caller frees.
/// Returns empty string if the excerpt is empty.
pub fn extractFromSource(
    allocator: std.mem.Allocator,
    src: []const u8,
    start_line: u32,
    node_type: []const u8,
    max_lines: usize,
) ![]u8 {
    const node_type_enum = common.NodeType.fromString(node_type);
    const result = try common.extractExcerpt(allocator, src, start_line, node_type_enum, max_lines);
    return @constCast(result);
}

/// Extract a code excerpt from a file path relative to workspace.
/// Returns an owned mutable slice; caller frees.
/// When member_name is non-null, runs line-number verification first.
pub fn extractFromPath(
    allocator: std.mem.Allocator,
    workspace: []const u8,
    rel_source: []const u8,
    start_line: u32,
    node_type: []const u8,
    member_name: ?[]const u8,
) ![]u8 {
    const abs_path = try std.fs.path.join(allocator, &.{ workspace, rel_source });
    defer allocator.free(abs_path);

    const src = common.readFileAlloc(allocator, abs_path, 10 * 1024 * 1024) orelse
        return allocator.dupe(u8, "");
    defer allocator.free(src);

    var effective_line = start_line;

    if (member_name) |name| {
        const member_type = memberTypeFromNodeType(node_type);
        const member = types_mod.Member{
            .type = member_type,
            .name = name,
            .line = start_line,
        };
        const vr = try line_verify.verifyMemberLine(allocator, src, member);
        defer vr.deinit(allocator);
        if (!vr.verified) {
            if (vr.corrected_line) |cl| {
                std.log.debug("[excerpt] stale line for {s}:{s} — was {}, corrected to {}", .{ rel_source, name, start_line, cl });
                effective_line = cl;
            }
        }
    }

    return extractFromSource(allocator, src, effective_line, node_type, common.DEFAULT_MAX_LINES);
}

/// Legacy 4-arg wrapper matching explainExtractExcerptPub's signature (max_lines=80).
pub fn extractFromSource_legacy(
    allocator: std.mem.Allocator,
    src: []const u8,
    start_line: u32,
    node_type: []const u8,
) ![]const u8 {
    return extractFromSource(allocator, src, start_line, node_type, 80);
}

/// Search a file for lines containing any of the given terms.
/// Returns a slice of 1-based line numbers; caller frees.
/// Skips comment lines (// and #).
pub fn grepFile(
    allocator: std.mem.Allocator,
    file_path: []const u8,
    terms: []const []const u8,
    max_results: usize,
) ![]usize {
    const io = std.Io.Threaded.global_single_threaded.io();
    const content = std.Io.Dir.cwd().readFileAlloc(io, file_path, allocator, .limited(10 * 1024 * 1024)) catch return &.{};
    defer allocator.free(content);

    var line_numbers: std.ArrayList(usize) = .empty;
    errdefer line_numbers.deinit(allocator);
    var it = std.mem.splitScalar(u8, content, '\n');
    var line_no: usize = 0;
    while (it.next()) |line| {
        line_no += 1;
        if (line_numbers.items.len >= max_results) break;
        const trimmed = common.trimLeft(u8, line, " \t");
        if (std.mem.startsWith(u8, trimmed, "//") or std.mem.startsWith(u8, trimmed, "#")) continue;
        const lower = try std.ascii.allocLowerString(allocator, line);
        defer allocator.free(lower);
        for (terms) |term| {
            if (std.mem.indexOf(u8, lower, term) != null) {
                try line_numbers.append(allocator, line_no);
                break;
            }
        }
    }
    return line_numbers.toOwnedSlice(allocator);
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn memberTypeFromNodeType(node_type: []const u8) types_mod.MemberType {
    if (std.mem.eql(u8, node_type, "fn_decl")) return .fn_decl;
    if (std.mem.eql(u8, node_type, "fn_private")) return .fn_private;
    if (std.mem.eql(u8, node_type, "method")) return .method;
    if (std.mem.eql(u8, node_type, "method_private")) return .method_private;
    if (std.mem.eql(u8, node_type, "struct")) return .@"struct";
    if (std.mem.eql(u8, node_type, "enum")) return .@"enum";
    if (std.mem.eql(u8, node_type, "union")) return .@"union";
    if (std.mem.eql(u8, node_type, "test_decl")) return .test_decl;
    if (std.mem.eql(u8, node_type, "enum_field")) return .enum_field;
    return .fn_decl; // fallback
}
