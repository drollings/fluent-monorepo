//! core/format.zig — Unified markdown formatting for explain output.
//!
//! Consolidates:
//!   - staged.zig:formatStaged()
//!   - query_engine.zig:renderExplainOutput()  (exposed as formatLegacy)
//!
//! Public types (SkillExcerpt, ExcerptEntry, FileMatchItem) are defined here
//! so query_engine.zig can import them instead of redeclaring locally.

const std = @import("std");
const common = @import("common");
const types_mod = @import("../types.zig");
const core_ranking = @import("ranking.zig");
const core_metadata = @import("metadata.zig");

pub const SearchResult = core_ranking.SearchResult;

// =============================================================================
// Shared types (moved from query_engine.zig)
// =============================================================================

pub const SkillExcerpt = struct { name: []const u8, excerpt: []const u8 };

/// Structured excerpt entry for legacy explain rendering.
pub const ExcerptEntry = struct {
    file_path: []const u8, // borrowed from SearchResult
    label: []const u8, // owned: "src/foo.zig:42"
    code: []const u8, // owned: pruned source block
    lang: []const u8, // borrowed constant
};

/// File match metadata with path, count, and line numbers.
pub const FileMatchItem = struct { path: []const u8, count: usize, lines: []usize };

// =============================================================================
// formatStaged — render []Stage to markdown (moved from staged.zig)
// =============================================================================

/// Writes deduplicated CODE stages as fenced code blocks. writer must be std.ArrayList writer.
fn formatCodeStages(allocator: std.mem.Allocator, writer: anytype, stages: []const types_mod.Stage) !void {
    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();
    const aa = arena.allocator();

    var seen_code: std.StringHashMapUnmanaged(void) = .empty;
    defer seen_code.deinit(aa);
    for (stages) |s| {
        if (s.kind != .code) continue;
        const dedup_key = if (s.line) |ln|
            try std.fmt.allocPrint(aa, "{s}:{d}", .{ s.source, ln })
        else
            try aa.dupe(u8, s.source);
        if (seen_code.contains(dedup_key)) continue;
        try seen_code.put(aa, dedup_key, {});
        const lang = common.langFromPath(s.source);
        if (s.line) |ln| {
            const trimmed_content = std.mem.trimRight(u8, s.content, " \t\n\r");
            const end_ln = ln + std.mem.count(u8, trimmed_content, "\n");
            try writer.print("## Source location: `{s}:{d}-{d}`\n\n```{s}\n", .{ s.source, ln, end_ln, lang });
        } else {
            try writer.print("## Source location: `{s}`\n\n```{s}\n", .{ s.source, lang });
        }
        try writer.print("{s}", .{s.content});
        try writer.writeAll("\n```\n\n");
    }
}

/// Writes capability_doc and skill_doc stages.
fn formatCapabilitySection(writer: anytype, stages: []const types_mod.Stage) !void {
    for (stages) |s| {
        if (s.kind != .capability_doc) continue;
        try writer.print("## Capability: {s}\n\n", .{s.source});
        try writer.print("{s}\n", .{std.mem.trim(u8, s.content, "\t\n\r")});
        try writer.writeByte('\n');
    }
    var skill_header_written = false;
    for (stages) |s| {
        if (s.kind != .skill_doc) continue;
        if (!skill_header_written) {
            try writer.writeAll("## Knowledge Base\n\n**READ BEFORE IMPLEMENTING**\n\n");
            skill_header_written = true;
        }
        const excerpt = std.mem.trim(u8, s.content, "\t\n\r");
        const first_nl = std.mem.indexOfScalar(u8, excerpt, '\n') orelse excerpt.len;
        try writer.print("- **{s}**: {s}\n", .{ s.source, excerpt[0..@min(first_nl, 200)] });
    }
    if (skill_header_written) try writer.writeByte('\n');
}

/// Writes the ## References section from metadata stages.
fn formatMetadataSection(allocator: std.mem.Allocator, writer: anytype, query: []const u8, stages: []const types_mod.Stage) !void {
    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();
    const aa = arena.allocator();

    var ref_header_written = false;
    var all_keywords: std.ArrayList(u8) = .empty;
    defer all_keywords.deinit(aa);
    var keyword_list: std.ArrayList([]const u8) = .empty;
    defer keyword_list.deinit(aa);
    var all_see_also: std.ArrayList(u8) = .empty;
    defer all_see_also.deinit(aa);
    var all_skills: std.ArrayList(u8) = .empty;
    defer all_skills.deinit(aa);
    var all_capabilities: std.ArrayList(u8) = .empty;
    defer all_capabilities.deinit(aa);
    var all_matched_caps: std.ArrayList(u8) = .empty;
    defer all_matched_caps.deinit(aa);

    var seen_kw: std.StringHashMapUnmanaged(void) = .empty;
    defer seen_kw.deinit(aa);
    var seen_see_also: std.StringHashMapUnmanaged(void) = .empty;
    defer seen_see_also.deinit(aa);
    var seen_skills_ref: std.StringHashMapUnmanaged(void) = .empty;
    defer seen_skills_ref.deinit(aa);
    var seen_caps_ref: std.StringHashMapUnmanaged(void) = .empty;
    defer seen_caps_ref.deinit(aa);
    var seen_matched_caps: std.StringHashMapUnmanaged(void) = .empty;
    defer seen_matched_caps.deinit(aa);

    for (stages) |s| {
        if (s.kind != .metadata) continue;
        var lines = std.mem.splitScalar(u8, s.content, '\n');
        while (lines.next()) |line| {
            if (std.mem.startsWith(u8, line, "keywords: ")) {
                var parts = std.mem.splitSequence(u8, line["keywords: ".len..], ", ");
                while (parts.next()) |part| {
                    const p = std.mem.trim(u8, part, " \t");
                    if (p.len == 0 or seen_kw.contains(p) or std.ascii.eqlIgnoreCase(p, query) or seen_kw.count() >= 10) continue;
                    try seen_kw.put(aa, p, {});
                    try keyword_list.append(aa, p);
                }
            } else if (std.mem.startsWith(u8, line, "used_by: ")) {
                var parts = std.mem.splitSequence(u8, line["used_by: ".len..], ", ");
                while (parts.next()) |part| {
                    const p = std.mem.trim(u8, part, " \t");
                    if (p.len == 0 or seen_see_also.contains(p)) continue;
                    try seen_see_also.put(aa, p, {});
                    if (all_see_also.items.len > 0) try all_see_also.appendSlice(aa, ", ");
                    try all_see_also.append(aa, '`');
                    try all_see_also.appendSlice(aa, p);
                    try all_see_also.append(aa, '`');
                }
            } else if (std.mem.startsWith(u8, line, "skills: ")) {
                var parts = std.mem.splitSequence(u8, line["skills: ".len..], ", ");
                while (parts.next()) |part| {
                    const p = std.mem.trim(u8, part, " \t");
                    if (p.len == 0 or seen_skills_ref.contains(p)) continue;
                    try seen_skills_ref.put(aa, p, {});
                    if (all_skills.items.len > 0) try all_skills.appendSlice(aa, ", ");
                    try all_skills.append(aa, '`');
                    try all_skills.appendSlice(aa, p);
                    try all_skills.append(aa, '`');
                }
            } else if (std.mem.startsWith(u8, line, "capabilities: ")) {
                var parts = std.mem.splitSequence(u8, line["capabilities: ".len..], ", ");
                while (parts.next()) |part| {
                    const p = std.mem.trim(u8, part, " \t");
                    if (p.len == 0 or seen_caps_ref.contains(p)) continue;
                    try seen_caps_ref.put(aa, p, {});
                    if (all_capabilities.items.len > 0) try all_capabilities.appendSlice(aa, "\x00");
                    try all_capabilities.appendSlice(aa, p);
                }
            } else if (std.mem.startsWith(u8, line, "matched_capabilities: ")) {
                var parts = std.mem.splitSequence(u8, line["matched_capabilities: ".len..], ", ");
                while (parts.next()) |part| {
                    const p = std.mem.trim(u8, part, " \t");
                    if (p.len == 0 or seen_matched_caps.contains(p)) continue;
                    try seen_matched_caps.put(aa, p, {});
                    if (all_matched_caps.items.len > 0) try all_matched_caps.appendSlice(aa, ", ");
                    try all_matched_caps.append(aa, '`');
                    try all_matched_caps.appendSlice(aa, p);
                    try all_matched_caps.append(aa, '`');
                }
            }
        }
    }

    if (keyword_list.items.len > 0) {
        for (keyword_list.items[1..]) |kw| {
            if (all_keywords.items.len > 0) try all_keywords.appendSlice(aa, ", ");
            try all_keywords.append(aa, '`');
            try all_keywords.appendSlice(aa, kw);
            try all_keywords.append(aa, '`');
        }
        if (!ref_header_written) {
            try writer.writeAll("## References\n\n");
            ref_header_written = true;
        }
        try writer.print("- **Recommended search command**: `guidance explain \"{s}\"`\n", .{keyword_list.items[0]});
        if (all_keywords.items.len > 0) try writer.print("- **Other terms to search**: {s}\n", .{all_keywords.items});
    }

    if (all_see_also.items.len > 0 or all_skills.items.len > 0 or all_capabilities.items.len > 0 or all_matched_caps.items.len > 0) {
        if (!ref_header_written) {
            try writer.writeAll("## References\n\n");
            ref_header_written = true;
        }
        if (all_matched_caps.items.len > 0) try writer.print("- **Matched capabilities**: {s}\n", .{all_matched_caps.items});
        if (all_see_also.items.len > 0) try writer.print("- **Files used most in**: {s}\n", .{all_see_also.items});
        if (all_skills.items.len > 0) try writer.print("- **Skills**: {s}\n", .{all_skills.items});
        if (all_capabilities.items.len > 0) {
            try writer.writeAll("- **Capabilities**: ");
            var cap_it = std.mem.splitScalar(u8, all_capabilities.items, '\x00');
            var cap_first = true;
            while (cap_it.next()) |cap_name| {
                if (!cap_first) try writer.writeAll(", ");
                cap_first = false;
                try writer.print("`{s}`", .{cap_name});
            }
            try writer.writeByte('\n');
        }
    }
}

/// Render a slice of Stage values to a markdown string.
/// Result is allocator-owned; caller frees.
pub fn formatStaged(
    allocator: std.mem.Allocator,
    query: []const u8,
    stages: []const types_mod.Stage,
    summary: ?[]const u8,
    _: []const u8,
) ![]u8 {
    var out: std.ArrayList(u8) = .empty;
    defer out.deinit(allocator);
    const w = out.writer(allocator);

    try w.print("# Explain: {s}\n\n", .{query});
    if (summary) |s| {
        const trimmed = std.mem.trim(u8, s, " \t\n\r");
        if (trimmed.len > 0) try w.print("{s}\n\n", .{trimmed});
    }

    for (stages) |s| {
        if (s.kind != .not_found) continue;
        try w.print("{s}\n\n", .{std.mem.trim(u8, s.content, " \t\n\r")});
        return out.toOwnedSlice(allocator);
    }

    try formatCodeStages(allocator, w, stages);
    try formatCapabilitySection(w, stages);
    try formatMetadataSection(allocator, w, query, stages);

    return out.toOwnedSlice(allocator);
}

// =============================================================================
// formatLegacy — render legacy explain output to stdout (from renderExplainOutput)
// =============================================================================

/// Render legacy (non-staged) explain output directly to stdout.
/// Replaces query_engine.zig:renderExplainOutput.
pub fn formatLegacy(
    allocator: std.mem.Allocator,
    query_text: []const u8,
    results: []const SearchResult,
    search_terms: []const []const u8,
    ai_summary: ?[]const u8,
    skill_excerpts: []const SkillExcerpt,
    excerpts: []const ExcerptEntry,
    file_matches: []const FileMatchItem,
) !void {
    var ws: common.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();

    try stdout.print("# Explain: {s}\n\n", .{query_text});

    if (ai_summary) |s| {
        const trimmed_s = std.mem.trim(u8, s, " \t\n\r");
        if (trimmed_s.len > 0) try stdout.print("{s}\n\n", .{trimmed_s});
    }

    try stdout.print("---\n", .{});
    try stdout.print("**Source**: `{s}`\n", .{results[0].source});

    for (skill_excerpts[0..@min(2, skill_excerpts.len)]) |se| {
        const first_nl = std.mem.indexOfScalar(u8, se.excerpt, '\n') orelse se.excerpt.len;
        try stdout.print("**Pattern**: `{s}` — {s}\n", .{ se.name, se.excerpt[0..@min(first_nl, 120)] });
    }
    try stdout.print("\n", .{});

    for (excerpts) |e| {
        try stdout.print("```{s}\n// {s}\n{s}\n```\n\n", .{ e.lang, e.label, e.code });
    }

    // Keywords: public non-test members from primary source JSON, excluding search terms.
    {
        var kw_buf: std.ArrayList(u8) = .empty;
        defer kw_buf.deinit(allocator);
        var kw_count: usize = 0;

        if (core_metadata.loadPublicMemberNames(allocator, results[0].file_path)) |names| {
            defer {
                for (names) |n| allocator.free(n);
                allocator.free(names);
            }
            for (names) |mname| {
                if (kw_count >= 8) break;
                const mname_lower = try std.ascii.allocLowerString(allocator, mname);
                defer allocator.free(mname_lower);
                const is_term = for (search_terms) |term| {
                    if (std.mem.eql(u8, mname_lower, term)) break true;
                } else false;
                if (is_term) continue;
                if (kw_count > 0) try kw_buf.appendSlice(allocator, ", ");
                try kw_buf.writer(allocator).print("`{s}`", .{mname});
                kw_count += 1;
            }
        }
        if (kw_count > 0) try stdout.print("**See Also**: {s}\n\n", .{kw_buf.items});
    }

    // See also: used_by from top result + secondary file paths.
    {
        var see_buf: std.ArrayList(u8) = .empty;
        defer see_buf.deinit(allocator);
        var see_count: usize = 0;

        var ub_from_json: ?[][]const u8 = null;
        defer if (ub_from_json) |ub| {
            for (ub) |s| allocator.free(s);
            allocator.free(ub);
        };
        const top_used_by: [][]const u8 = if (results[0].used_by.len > 0)
            results[0].used_by
        else blk: {
            ub_from_json = core_metadata.loadUsedByFromJson(allocator, results[0].file_path);
            break :blk ub_from_json orelse &.{};
        };

        for (top_used_by[0..@min(4, top_used_by.len)]) |ub| {
            if (see_count > 0) try see_buf.appendSlice(allocator, ", ");
            try see_buf.writer(allocator).print("`{s}`", .{ub});
            see_count += 1;
        }
        for (results[1..@min(results.len, 6)]) |r| {
            if (see_count >= 6) break;
            if (r.source.len == 0 or std.mem.eql(u8, r.source, results[0].source)) continue;
            if (see_count > 0) try see_buf.appendSlice(allocator, ", ");
            try see_buf.writer(allocator).print("`{s}`", .{r.source});
            see_count += 1;
        }
        if (see_count > 0) try stdout.print("**See also**: {s}\n\n", .{see_buf.items});
    }

    if (file_matches.len > 0) {
        try stdout.print("### Files with most matches\n\n", .{});
        for (file_matches[0..@min(3, file_matches.len)]) |fm| {
            try stdout.print("- `{s}` ({d} matches): lines ", .{ fm.path, fm.count });
            for (fm.lines[0..@min(10, fm.lines.len)], 0..) |ln, li| {
                if (li > 0) try stdout.print(", ", .{});
                try stdout.print("{d}", .{ln});
            }
            try stdout.print("\n", .{});
        }
        try stdout.print("\n", .{});
    }

    try stdout.flush();
}
