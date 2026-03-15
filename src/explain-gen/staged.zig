//! staged.zig — Staged explain pipeline for `explain-gen explain`.
//!
//! Implements the hybrid FTS5 + guidance JSON pipeline described in
//! ROADMAP_EXPLAIN_ENHANCE.md.  The key exported functions are:
//!
//!   executeStaged()   — FTS5 search → collect Stage slices
//!   expandFollowUps() — follow see_also / skill refs → more stages
//!   formatStaged()    — render []Stage to markdown output

const std = @import("std");
const db_mod = @import("db.zig");
const types = @import("types.zig");

/// Check if a file path matches common test file patterns (language-agnostic).
/// Used to filter test files from "Used by" display.
fn isTestPath(rel_path: []const u8) bool {
    const basename = std.fs.path.basename(rel_path);
    const stem = blk: {
        const ext = std.fs.path.extension(basename);
        break :blk if (ext.len > 0) basename[0 .. basename.len - ext.len] else basename;
    };
    if (std.mem.endsWith(u8, stem, "_test")) return true;
    if (std.mem.startsWith(u8, stem, "test_")) return true;
    if (std.mem.eql(u8, stem, "tests")) return true;
    if (std.mem.indexOf(u8, rel_path, "/test/") != null) return true;
    if (std.mem.indexOf(u8, rel_path, "/tests/") != null) return true;
    if (std.mem.indexOf(u8, rel_path, "\\test\\") != null) return true;
    if (std.mem.indexOf(u8, rel_path, "\\tests\\") != null) return true;
    return false;
}

// ---------------------------------------------------------------------------
// Stage collection entry point
// ---------------------------------------------------------------------------

/// Collect all stages for a query by searching the FTS5 database and loading
/// supporting data from guidance JSON files.
///
/// Returned slice is owned by the caller; free with types.freeStages() then
/// allocator.free(slice).
pub fn executeStaged(
    allocator: std.mem.Allocator,
    db: *db_mod.ExplainDb,
    query: []const u8,
    workspace: []const u8,
) ![]types.Stage {
    return executeStagedWithAliases(allocator, db, query, workspace, null);
}

/// Collect stages with optional semantic alias expansion.
pub fn executeStagedWithAliases(
    allocator: std.mem.Allocator,
    db: *db_mod.ExplainDb,
    query: []const u8,
    workspace: []const u8,
    aliases: ?db_mod.SemanticAliases,
) ![]types.Stage {
    // ── FTS5 search with alias expansion ──────────────────────────────────────
    const results = try db.searchWithAliases(allocator, query, 15, aliases);
    defer {
        for (results) |r| db_mod.ExplainDb.freeSearchResult(allocator, r);
        allocator.free(results);
    }

    var stages: std.ArrayList(types.Stage) = .{};
    errdefer {
        types.freeStages(allocator, stages.items);
        stages.deinit(allocator);
    }

    // ── Prose stages: module + member comments ────────────────────────────────
    // Track seen (source, name) pairs to avoid duplicate prose entries.
    var seen_prose: std.StringHashMapUnmanaged(void) = .{};
    defer {
        var it = seen_prose.keyIterator();
        while (it.next()) |k| allocator.free(k.*);
        seen_prose.deinit(allocator);
    }

    for (results[0..@min(10, results.len)]) |r| {
        const comment = r.comment orelse continue;
        if (comment.len < 10) continue; // skip trivial stubs

        // Key: "source\x00name" to dedup per-member comments.
        const key = try std.fmt.allocPrint(allocator, "{s}\x00{s}", .{ r.source, r.name });
        defer allocator.free(key);
        if (seen_prose.contains(key)) continue;
        try seen_prose.put(allocator, try allocator.dupe(u8, key), {});

        // For module-level rows show a brief label.
        const prose_src = if (std.mem.eql(u8, r.node_type, "module"))
            try allocator.dupe(u8, r.source)
        else
            try std.fmt.allocPrint(allocator, "{s}:{s}", .{ r.source, r.name });

        try stages.append(allocator, .{
            .kind = .prose,
            .content = try allocator.dupe(u8, comment),
            .source = prose_src,
            .line = r.line,
        });
    }

    // ── Code stages: source excerpts for top 3 unique source files ────────────
    var seen_code_files: std.StringHashMapUnmanaged(void) = .{};
    defer seen_code_files.deinit(allocator);

    for (results) |r| {
        if (seen_code_files.count() >= 3) break;
        if (r.source.len == 0) continue;
        if (seen_code_files.contains(r.source)) continue;
        const line = r.line orelse continue;

        try seen_code_files.put(allocator, r.source, {});

        const excerpt = extractSourceExcerpt(allocator, workspace, r.source, line, r.node_type) catch continue;
        if (excerpt.len == 0) {
            allocator.free(excerpt);
            continue;
        }

        try stages.append(allocator, .{
            .kind = .code,
            .content = excerpt,
            .source = try allocator.dupe(u8, r.source),
            .line = line,
        });
    }

    // ── Metadata stages: guidance JSON keywords / see_also / skills ───────────
    var seen_guidance: std.StringHashMapUnmanaged(void) = .{};
    defer seen_guidance.deinit(allocator);

    for (results[0..@min(5, results.len)]) |r| {
        if (seen_guidance.contains(r.file_path)) continue;
        try seen_guidance.put(allocator, r.file_path, {});

        const meta = buildMetadataStage(allocator, r.file_path, r.source) catch continue;
        const meta_stage = meta orelse continue;
        try stages.append(allocator, meta_stage);
    }

    // ── See-also traversal for sparse results ──────────────────────────────────
    // If we have few results (< 3 code stages), follow used_by paths.
    if (stages.items.len < 5 and results.len > 0) {
        var seen_sources: std.StringHashMapUnmanaged(void) = .{};
        defer {
            var kit = seen_sources.keyIterator();
            while (kit.next()) |k| allocator.free(k.*);
            seen_sources.deinit(allocator);
        }

        for (stages.items) |s| {
            if (s.kind == .code or s.kind == .prose) {
                try seen_sources.put(allocator, try allocator.dupe(u8, s.source), {});
            }
        }

        for (results[0..@min(3, results.len)]) |r| {
            if (r.used_by.len == 0) continue;

            for (r.used_by[0..@min(3, r.used_by.len)]) |ub_path| {
                if (seen_sources.contains(ub_path)) continue;

                const excerpt = extractSourceExcerpt(allocator, workspace, ub_path, 1, "module") catch continue;
                if (excerpt.len == 0) {
                    allocator.free(excerpt);
                    continue;
                }

                try seen_sources.put(allocator, try allocator.dupe(u8, ub_path), {});
                try stages.append(allocator, .{
                    .kind = .code,
                    .content = excerpt,
                    .source = try allocator.dupe(u8, ub_path),
                    .line = 1,
                });

                if (stages.items.len >= 8) break;
            }
            if (stages.items.len >= 8) break;
        }
    }

    return stages.toOwnedSlice(allocator);
}

// ---------------------------------------------------------------------------
// Follow-up expansion (M7)
// ---------------------------------------------------------------------------

/// Follow metadata breadcrumbs: used_by paths and skill refs found in the top
/// search results.  Returns a NEW slice of additional stages to append.
/// Caller owns returned slice and must free with types.freeStages() + free(slice).
pub fn expandFollowUps(
    allocator: std.mem.Allocator,
    db_results_file_paths: []const []const u8, // file_path fields from search results
    db_results_sources: []const []const u8, // source fields from search results
    db_results_used_by: []const []const []const u8, // used_by slices from search results
    workspace: []const u8,
    guidance_dir: []const u8,
    skills_dir: []const u8,
    existing_sources: []const []const u8, // already-seen source paths (dedup)
    limit: usize,
) ![]types.Stage {
    _ = workspace; // reserved for future source excerpt expansion
    var extra: std.ArrayList(types.Stage) = .{};
    errdefer {
        types.freeStages(allocator, extra.items);
        extra.deinit(allocator);
    }

    var seen: std.StringHashMapUnmanaged(void) = .{};
    defer seen.deinit(allocator);

    // Pre-populate seen with existing sources.
    for (existing_sources) |s| {
        try seen.put(allocator, s, {});
    }

    // ── See-also from used_by fields ──────────────────────────────────────────
    for (db_results_used_by[0..@min(3, db_results_used_by.len)]) |ub_slice| {
        for (ub_slice[0..@min(3, ub_slice.len)]) |ub_path| {
            if (seen.contains(ub_path)) continue;
            if (extra.items.len >= limit) break;
            try seen.put(allocator, ub_path, {});

            // Derive guidance JSON path for ub_path.
            const json_path = try std.fmt.allocPrint(allocator, "{s}/src/{s}.json", .{ guidance_dir, ub_path });
            defer allocator.free(json_path);

            const meta = buildMetadataStage(allocator, json_path, ub_path) catch continue;
            if (meta) |m| try extra.append(allocator, m);

            // Load module comment as prose stage.
            if (loadModuleComment(allocator, json_path)) |comment| {
                try extra.append(allocator, .{
                    .kind = .prose,
                    .content = comment,
                    .source = try allocator.dupe(u8, ub_path),
                });
            }
        }
    }

    // ── Skill docs from guidance JSON skill refs ────────────────────────────────
    var seen_skills: std.StringHashMapUnmanaged(void) = .{};
    defer {
        var kit = seen_skills.keyIterator();
        while (kit.next()) |k| allocator.free(k.*);
        seen_skills.deinit(allocator);
    }

    for (db_results_file_paths[0..@min(3, db_results_file_paths.len)], 0..) |fp, idx| {
        _ = db_results_sources[idx]; // suppress unused warning
        const skill_names = loadSkillNamesFromJson(allocator, fp) catch continue;
        defer {
            for (skill_names) |n| allocator.free(n);
            allocator.free(skill_names);
        }
        for (skill_names[0..@min(2, skill_names.len)]) |sname| {
            if (seen_skills.contains(sname)) continue;
            if (extra.items.len >= limit) break;
            try seen_skills.put(allocator, try allocator.dupe(u8, sname), {});

            const skill_content = loadSkillExcerpt(allocator, skills_dir, sname) catch continue;
            const excerpt = skill_content orelse continue;
            try extra.append(allocator, .{
                .kind = .skill_doc,
                .content = excerpt,
                .source = try allocator.dupe(u8, sname),
            });
        }
    }

    return extra.toOwnedSlice(allocator);
}

// ---------------------------------------------------------------------------
// Output formatting (M9)
// ---------------------------------------------------------------------------

/// Format a []Stage slice into clean markdown output.
/// If `summary` is non-null it is prepended as the synthesized answer block.
/// Returns an owned allocation; caller must free.
pub fn formatStaged(
    allocator: std.mem.Allocator,
    query: []const u8,
    stages: []const types.Stage,
    summary: ?[]const u8,
    workspace: []const u8,
) ![]u8 {
    _ = workspace;
    var out: std.ArrayList(u8) = .{};
    defer out.deinit(allocator);
    const w = out.writer(allocator);

    try w.print("# Explain: {s}\n\n", .{query});

    // ── Synthesized summary (long-query path only) ────────────────────────────
    if (summary) |s| {
        const trimmed = std.mem.trim(u8, s, " \t\n\r");
        if (trimmed.len > 0) {
            try w.print("{s}\n\n", .{trimmed});
        }
    }

    try w.writeAll("---\n\n");

    // ── Group stages by source ────────────────────────────────────────────────
    // Determine unique source order (preserve insertion order).
    var source_order: std.ArrayList([]const u8) = .{};
    defer source_order.deinit(allocator);
    var seen_srcs: std.StringHashMapUnmanaged(void) = .{};
    defer seen_srcs.deinit(allocator);

    for (stages) |s| {
        if (s.kind == .insight or s.kind == .skill_doc) continue;
        if (seen_srcs.contains(s.source)) continue;
        try seen_srcs.put(allocator, s.source, {});
        try source_order.append(allocator, s.source);
    }

    // Emit source sections.
    for (source_order.items) |src| {
        // Find the prose stages for this source.
        var has_content = false;
        for (stages) |s| {
            if (!std.mem.eql(u8, s.source, src)) continue;
            if (s.kind == .prose) {
                if (!has_content) {
                    try w.print("## Source: `{s}`\n\n", .{src});
                    has_content = true;
                }
                try w.print("{s}\n\n", .{std.mem.trim(u8, s.content, " \t\n\r")});
            }
        }

        // Emit code stages for this source.
        for (stages) |s| {
            if (!std.mem.eql(u8, s.source, src)) continue;
            if (s.kind != .code) continue;

            if (!has_content) {
                try w.print("## Source: `{s}`\n\n", .{src});
                has_content = true;
            }

            const lang = langFromPath(src);
            if (s.line) |ln| {
                try w.print("```{s}\n// {s}:{d}\n", .{ lang, src, ln });
            } else {
                try w.print("```{s}\n// {s}\n", .{ lang, src });
            }

            // Code is already extracted as complete units, no second truncation.
            try w.print("{s}", .{s.content});
            try w.writeAll("\n```\n\n");
        }
    }

    // ── Skill doc stages ──────────────────────────────────────────────────────
    var skill_header_written = false;
    for (stages) |s| {
        if (s.kind != .skill_doc) continue;
        if (!skill_header_written) {
            try w.writeAll("## Knowledge Base\n\n**READ BEFORE IMPLEMENTING**\n\n");
            skill_header_written = true;
        }
        const excerpt = std.mem.trim(u8, s.content, " \t\n\r");
        const first_nl = std.mem.indexOfScalar(u8, excerpt, '\n') orelse excerpt.len;
        try w.print("- **{s}**: {s}\n", .{ s.source, excerpt[0..@min(first_nl, 200)] });
    }
    if (skill_header_written) try w.writeByte('\n');

    // ── References: collect all metadata stages ───────────────────────────────
    var ref_header_written = false;
    var all_keywords: std.ArrayList(u8) = .{};
    defer all_keywords.deinit(allocator);
    var all_see_also: std.ArrayList(u8) = .{};
    defer all_see_also.deinit(allocator);
    var all_skills: std.ArrayList(u8) = .{};
    defer all_skills.deinit(allocator);

    // Deduplicate keywords, see_also paths, and skills across all metadata stages.
    var seen_kw: std.StringHashMapUnmanaged(void) = .{};
    defer seen_kw.deinit(allocator);
    var seen_see_also: std.StringHashMapUnmanaged(void) = .{};
    defer seen_see_also.deinit(allocator);
    var seen_skills_ref: std.StringHashMapUnmanaged(void) = .{};
    defer seen_skills_ref.deinit(allocator);

    for (stages) |s| {
        if (s.kind != .metadata) continue;
        var lines = std.mem.splitScalar(u8, s.content, '\n');
        while (lines.next()) |line| {
            if (std.mem.startsWith(u8, line, "keywords: ")) {
                const v = line["keywords: ".len..];
                var parts = std.mem.splitSequence(u8, v, ", ");
                while (parts.next()) |part| {
                    const p = std.mem.trim(u8, part, " \t");
                    if (p.len == 0 or seen_kw.contains(p)) continue;
                    try seen_kw.put(allocator, p, {});
                    if (all_keywords.items.len > 0) try all_keywords.appendSlice(allocator, ", ");
                    try all_keywords.appendSlice(allocator, p);
                }
            } else if (std.mem.startsWith(u8, line, "used_by: ")) {
                // Split comma-separated paths and deduplicate.
                const v = line["used_by: ".len..];
                var parts = std.mem.splitSequence(u8, v, ", ");
                while (parts.next()) |part| {
                    const p = std.mem.trim(u8, part, " \t");
                    if (p.len == 0 or seen_see_also.contains(p)) continue;
                    try seen_see_also.put(allocator, p, {});
                    if (all_see_also.items.len > 0) try all_see_also.appendSlice(allocator, ", ");
                    try all_see_also.appendSlice(allocator, p);
                }
            } else if (std.mem.startsWith(u8, line, "skills: ")) {
                const v = line["skills: ".len..];
                var parts = std.mem.splitSequence(u8, v, ", ");
                while (parts.next()) |part| {
                    const p = std.mem.trim(u8, part, " \t");
                    if (p.len == 0 or seen_skills_ref.contains(p)) continue;
                    try seen_skills_ref.put(allocator, p, {});
                    if (all_skills.items.len > 0) try all_skills.appendSlice(allocator, ", ");
                    try all_skills.appendSlice(allocator, p);
                }
            }
        }
    }

    if (all_keywords.items.len > 0 or all_see_also.items.len > 0 or all_skills.items.len > 0) {
        if (!ref_header_written) {
            try w.writeAll("## References\n\n");
            ref_header_written = true;
        }
        if (all_keywords.items.len > 0) try w.print("- **Keywords**: {s}\n", .{all_keywords.items});
        if (all_see_also.items.len > 0) try w.print("- **Used by**: {s}\n", .{all_see_also.items});
        if (all_skills.items.len > 0) try w.print("- **Skills**: {s}\n", .{all_skills.items});
    }

    return out.toOwnedSlice(allocator);
}

// ---------------------------------------------------------------------------
// Source excerpt extraction
// ---------------------------------------------------------------------------

/// Load source file and extract an excerpt starting at `start_line` (1-based).
/// Extracts complete functions/structs by tracking brace depth.
/// For functions: extracts the entire function.
/// For structs/enums/unions: abbreviates to signatures.
/// Returns an owned allocation; caller must free.
pub fn extractSourceExcerpt(
    allocator: std.mem.Allocator,
    workspace: []const u8,
    rel_source: []const u8,
    start_line: u32,
    node_type: []const u8,
) ![]u8 {
    const abs_path = try std.fs.path.join(allocator, &.{ workspace, rel_source });
    defer allocator.free(abs_path);

    const file = std.fs.openFileAbsolute(abs_path, .{}) catch return allocator.dupe(u8, "");
    defer file.close();

    const src = file.readToEndAlloc(allocator, 10 * 1024 * 1024) catch return allocator.dupe(u8, "");
    defer allocator.free(src);

    return extractExcerptFromSource(allocator, src, start_line, node_type);
}

/// Extract a complete logical unit (function/struct/etc) starting at `start_line` (1-based).
/// Uses brace counting to find the end of the scope.
/// For functions: extracts the entire function (no line limit).
/// For structs/enums/unions: abbreviates to declarations only.
/// Returns an owned allocation.
pub fn extractExcerptFromSource(
    allocator: std.mem.Allocator,
    src: []const u8,
    start_line: u32,
    node_type: []const u8,
) ![]u8 {
    const is_fn = std.mem.eql(u8, node_type, "fn_decl") or
        std.mem.eql(u8, node_type, "fn_private") or
        std.mem.eql(u8, node_type, "method") or
        std.mem.eql(u8, node_type, "method_private");
    const is_container = std.mem.eql(u8, node_type, "struct") or
        std.mem.eql(u8, node_type, "enum") or
        std.mem.eql(u8, node_type, "union");

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

        // For containers, abbreviate: show declarations and comments at container level
        if (is_container and started_scope and brace_depth > scope_start_depth) {
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
                // Skip body content inside nested containers
                brace_depth += line_brace_delta;
                continue;
            }
        }

        try lines.append(allocator, trimmed);

        // Update brace depth after adding the line
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

        // No hard line limit for functions - they should be complete
        // For other types (test_decl, enum_field, etc.), apply a default limit
        if (!is_fn and !is_container and lines.items.len >= 200) break;
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

// ---------------------------------------------------------------------------
// Guidance JSON helpers
// ---------------------------------------------------------------------------

/// Build a metadata Stage from a guidance JSON file.
/// Returns null when the file is absent or has no useful metadata.
fn buildMetadataStage(
    allocator: std.mem.Allocator,
    json_path: []const u8,
    source: []const u8,
) !?types.Stage {
    const f = std.fs.openFileAbsolute(json_path, .{}) catch return null;
    defer f.close();

    const content = f.readToEndAlloc(allocator, 8 * 1024 * 1024) catch return null;
    defer allocator.free(content);

    var parsed = std.json.parseFromSlice(std.json.Value, allocator, content, .{ .ignore_unknown_fields = true }) catch return null;
    defer parsed.deinit();

    if (parsed.value != .object) return null;
    const root = parsed.value.object;

    var meta_buf: std.ArrayList(u8) = .{};
    errdefer meta_buf.deinit(allocator);
    const mw = meta_buf.writer(allocator);

    // keywords: public member names.
    if (root.get("members")) |mv| {
        if (mv == .array) {
            var kw_count: usize = 0;
            var kw_buf: std.ArrayList(u8) = .{};
            defer kw_buf.deinit(allocator);
            for (mv.array.items) |item| {
                if (item != .object) continue;
                const is_pub: bool = blk: {
                    const pv = item.object.get("is_pub") orelse break :blk false;
                    if (pv != .bool) break :blk false;
                    break :blk pv.bool;
                };
                if (!is_pub) continue;
                const tv = item.object.get("type") orelse continue;
                if (tv != .string) continue;
                if (std.mem.eql(u8, tv.string, "test_decl")) continue;
                const nv = item.object.get("name") orelse continue;
                if (nv != .string) continue;
                if (kw_count > 0) try kw_buf.appendSlice(allocator, ", ");
                try kw_buf.appendSlice(allocator, nv.string);
                kw_count += 1;
                if (kw_count >= 12) break;
            }
            if (kw_buf.items.len > 0) {
                try mw.print("keywords: {s}\n", .{kw_buf.items});
            }
        }
    }

    // used_by: reverse dependency paths (exclude test files).
    if (root.get("used_by")) |ubv| {
        if (ubv == .array and ubv.array.items.len > 0) {
            var count: usize = 0;
            for (ubv.array.items) |item| {
                if (item != .string) continue;
                if (isTestPath(item.string)) continue;
                if (count == 0) {
                    try mw.writeAll("used_by: ");
                } else {
                    try mw.writeAll(", ");
                }
                try mw.writeAll(item.string);
                count += 1;
                if (count >= 5) break;
            }
            if (count > 0) try mw.writeByte('\n');
        }
    }

    // skills: skill refs.
    if (root.get("skills")) |sv| {
        if (sv == .array and sv.array.items.len > 0) {
            try mw.writeAll("skills: ");
            for (sv.array.items[0..@min(4, sv.array.items.len)], 0..) |item, i| {
                const ref: []const u8 = switch (item) {
                    .string => |s| s,
                    .object => blk: {
                        const rv = item.object.get("ref") orelse break :blk "";
                        if (rv != .string) break :blk "";
                        break :blk rv.string;
                    },
                    else => "",
                };
                if (ref.len == 0) continue;
                // Extract skill name from path: ".skills/foo/SKILL.md" → "foo".
                const base = std.fs.path.basename(ref);
                const skill_name: []const u8 = if (std.mem.eql(u8, base, "SKILL.md")) blk: {
                    const dir = std.fs.path.dirname(ref) orelse break :blk base;
                    break :blk std.fs.path.basename(dir);
                } else base;
                if (skill_name.len == 0) continue;
                if (i > 0) try mw.writeAll(", ");
                try mw.writeAll(skill_name);
            }
            try mw.writeByte('\n');
        }
    }

    if (meta_buf.items.len == 0) return null;

    return types.Stage{
        .kind = .metadata,
        .content = try meta_buf.toOwnedSlice(allocator),
        .source = try allocator.dupe(u8, source),
    };
}

/// Load just the module-level comment from a guidance JSON file.
/// Returns an owned string or null.
fn loadModuleComment(allocator: std.mem.Allocator, json_path: []const u8) ?[]const u8 {
    const f = std.fs.openFileAbsolute(json_path, .{}) catch return null;
    defer f.close();
    const content = f.readToEndAlloc(allocator, 8 * 1024 * 1024) catch return null;
    defer allocator.free(content);

    var parsed = std.json.parseFromSlice(std.json.Value, allocator, content, .{ .ignore_unknown_fields = true }) catch return null;
    defer parsed.deinit();

    if (parsed.value != .object) return null;
    const cv = parsed.value.object.get("comment") orelse return null;
    if (cv != .string) return null;
    if (cv.string.len < 10) return null;
    return allocator.dupe(u8, cv.string) catch null;
}

/// Load skill names (short names like "zig-current") from a guidance JSON file.
/// Returns an owned slice of owned strings; caller frees.
fn loadSkillNamesFromJson(
    allocator: std.mem.Allocator,
    json_path: []const u8,
) ![][]const u8 {
    const f = std.fs.openFileAbsolute(json_path, .{}) catch return &.{};
    defer f.close();
    const content = f.readToEndAlloc(allocator, 8 * 1024 * 1024) catch return &.{};
    defer allocator.free(content);

    var parsed = std.json.parseFromSlice(std.json.Value, allocator, content, .{ .ignore_unknown_fields = true }) catch return &.{};
    defer parsed.deinit();

    if (parsed.value != .object) return &.{};
    const sv = parsed.value.object.get("skills") orelse return &.{};
    if (sv != .array) return &.{};

    var out: std.ArrayList([]const u8) = .{};
    errdefer {
        for (out.items) |s| allocator.free(s);
        out.deinit(allocator);
    }

    for (sv.array.items) |item| {
        const ref: []const u8 = switch (item) {
            .string => |s| s,
            .object => blk: {
                const rv = item.object.get("ref") orelse break :blk "";
                if (rv != .string) break :blk "";
                break :blk rv.string;
            },
            else => "",
        };
        if (ref.len == 0) continue;
        const base = std.fs.path.basename(ref);
        const skill_name: []const u8 = if (std.mem.eql(u8, base, "SKILL.md")) blk: {
            const dir = std.fs.path.dirname(ref) orelse break :blk base;
            break :blk std.fs.path.basename(dir);
        } else base;
        if (skill_name.len == 0) continue;
        try out.append(allocator, try allocator.dupe(u8, skill_name));
    }

    return out.toOwnedSlice(allocator);
}

/// Load the first paragraph / description from a SKILL.md file.
/// Returns an owned string or null.
pub fn loadSkillExcerpt(
    allocator: std.mem.Allocator,
    skills_dir: []const u8,
    skill_name: []const u8,
) !?[]const u8 {
    const path = try std.fs.path.join(allocator, &.{ skills_dir, skill_name, "SKILL.md" });
    defer allocator.free(path);

    const sf = std.fs.openFileAbsolute(path, .{}) catch return null;
    defer sf.close();
    const content = sf.readToEndAlloc(allocator, 512 * 1024) catch return null;
    defer allocator.free(content);

    // YAML front matter — look for `description:`.
    if (std.mem.startsWith(u8, content, "---\n")) {
        const fm_close = std.mem.indexOf(u8, content[4..], "\n---\n");
        if (fm_close) |fmc| {
            const fm_body = content[4 .. 4 + fmc];
            var fm_lines = std.mem.splitScalar(u8, fm_body, '\n');
            while (fm_lines.next()) |fl| {
                if (std.mem.startsWith(u8, fl, "description:")) {
                    const val = std.mem.trim(u8, fl["description:".len..], " \t\r");
                    if (val.len > 0) return try allocator.dupe(u8, val[0..@min(val.len, 300)]);
                }
            }
            // No description: — first non-empty body line after front matter.
            const after = content[4 + fmc + 5 ..];
            var body = std.mem.splitScalar(u8, after, '\n');
            while (body.next()) |bl| {
                const t = std.mem.trim(u8, bl, " \t\r");
                if (t.len > 0 and !std.mem.startsWith(u8, t, "#"))
                    return try allocator.dupe(u8, t[0..@min(t.len, 300)]);
            }
        }
    }

    // No front matter — first paragraph (up to blank line), max 600 chars.
    const para_end = std.mem.indexOf(u8, content, "\n\n") orelse content.len;
    return try allocator.dupe(u8, content[0..@min(para_end, 600)]);
}

// ---------------------------------------------------------------------------
// Utility helpers
// ---------------------------------------------------------------------------

/// Determine fenced code block language from a file extension.
fn langFromPath(path: []const u8) []const u8 {
    if (std.mem.endsWith(u8, path, ".zig")) return "zig";
    if (std.mem.endsWith(u8, path, ".py")) return "python";
    if (std.mem.endsWith(u8, path, ".rs")) return "rust";
    if (std.mem.endsWith(u8, path, ".ts") or std.mem.endsWith(u8, path, ".tsx")) return "typescript";
    if (std.mem.endsWith(u8, path, ".js")) return "javascript";
    return "text";
}

/// Return a slice of `text` containing at most `max_lines` newline-separated lines.
/// This is a view into the original slice — no allocation.
fn truncateLines(text: []const u8, max_lines: usize) []const u8 {
    var count: usize = 0;
    var i: usize = 0;
    while (i < text.len) {
        if (text[i] == '\n') {
            count += 1;
            if (count >= max_lines) return text[0..i];
        }
        i += 1;
    }
    return text;
}
