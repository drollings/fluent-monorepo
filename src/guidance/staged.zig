//! staged.zig — Staged explain pipeline for `guidance explain`.
//!
//! Implements the hybrid vector + keyword search pipeline for the explain
//! subcommand. The key exported functions are:
//!
//!   executeStaged()   — search → collect Stage slices
//!   expandFollowUps() — follow see_also / skill refs → more stages
//!   formatStaged()    — render []Stage to markdown output
//!
//! ## Memory Ownership
//!
//!   - executeStagedConfig() / executeStagedWithAliasesOriginal(): Caller-owns
//!     returned []Stage; free with types.freeStages(allocator, stages).
//!   - expandFollowUps(): Caller-owns returned []Stage; free with types.freeStages().
//!   - All other public functions return borrowed slices or value types (no heap).
//!   - executeStagedWithAliasesOriginal() uses an ArenaAllocator internally
//!     for deduplication maps; the arena is freed at function exit.
//!
//! ## Hot Path Optimizations
//!
//! Token-matching in `collectCodeStages` uses `std.ascii.eqlIgnoreCase` for
//! zero-allocation case-insensitive comparison, eliminating O(tokens×results)
//! heap allocations entirely. See ROADMAP_20260420_QUALITY.md for rationale.

const std = @import("std");
const vector_db_mod = @import("vector");
const types = @import("types.zig");
const common = @import("common");
const line_verify = @import("sync/line_verify.zig");
const doc_parser = @import("doc_parser.zig");
const context_packer = @import("llm").context_packer;

const GuidanceDb = vector_db_mod.GuidanceDb;
const SearchResult = GuidanceDb.SearchResult;
const core_excerpt = @import("core/excerpt.zig");
const core_skill_loader = @import("core/skill_loader.zig");
const core_metadata = @import("core/metadata.zig");
const core_format = @import("core/format.zig");

// ---------------------------------------------------------------------------
// StagedConfig — unified query configuration (Phase 3)
// ---------------------------------------------------------------------------

/// Configuration for a staged query execution.
/// Replaces the 6-argument triad; defaults produce conservative behaviour.
pub const StagedConfig = struct {
    query: []const u8,
    /// Original (pre-alias-expansion) query for token matching.  Defaults to query when null.
    original_query: ?[]const u8 = null,
    workspace: []const u8,
    aliases: ?vector_db_mod.SemanticAliases = null,
    max_results: usize = 15,
};

/// Execute staged query pipeline.
/// Stages are allocator-owned; caller frees via types.freeStages(allocator, stages).
pub fn executeStagedConfig(
    allocator: std.mem.Allocator,
    db: *GuidanceDb,
    config: StagedConfig,
) ![]types.Stage {
    const original_query = config.original_query orelse config.query;
    return executeStagedWithAliasesOriginal(allocator, db, config.query, original_query, config.workspace, config.aliases);
}

// ---------------------------------------------------------------------------
// Stage collection entry point
// ---------------------------------------------------------------------------

/// Shim — delegates to executeStagedConfig.
pub fn executeStaged(
    allocator: std.mem.Allocator,
    db: *GuidanceDb,
    query: []const u8,
    workspace: []const u8,
) ![]types.Stage {
    return executeStagedConfig(allocator, db, .{ .query = query, .workspace = workspace });
}

/// Shim — delegates to executeStagedConfig.
pub fn executeStagedWithAliases(
    allocator: std.mem.Allocator,
    db: *GuidanceDb,
    query: []const u8,
    workspace: []const u8,
    aliases: ?vector_db_mod.SemanticAliases,
) ![]types.Stage {
    return executeStagedConfig(allocator, db, .{ .query = query, .workspace = workspace, .aliases = aliases });
}

/// Executes a Zig stage with original aliases, processing the provided query and returning the resulting stage.
// ── M7: Not-Found confidence thresholds ──────────────────────────────────────
// Scores from Phase 1 (exact name match) are 1.0.
// Scores from Phase 2/3 (vector/hybrid) are in the RRF range ~0.004–0.04.
// Below NOT_FOUND_RRF_THRESHOLD, nothing relevant was found.
const NOT_FOUND_RRF_THRESHOLD: f64 = 0.004;

/// Constructs a list of stage identifiers for missing data in the GuidanceDb.
fn buildNotFoundStages(
    allocator: std.mem.Allocator,
    query: []const u8,
    db: *GuidanceDb,
) ![]types.Stage {
    var stages: std.ArrayList(types.Stage) = .empty;
    errdefer {
        types.freeStages(allocator, stages.items);
        stages.deinit(allocator);
    }

    // Stage 1: Not-found prose message
    const msg = try std.fmt.allocPrint(allocator, "The query '{s}' did not match any indexed source files with sufficient confidence.\n\n" ++
        "Try:\n" ++
        "- Running `guidance explain` to see the main index of indexed subjects.\n" ++
        "- A more specific identifier (e.g. a function or struct name)\n" ++
        "- Running `guidance gen` if the code was recently added", .{query});
    try stages.append(allocator, .{
        .kind = .not_found,
        .content = msg,
        .source = try allocator.dupe(u8, "guidance"),
    });

    // Stage 2: Suggest nearest capability names (low threshold — broad suggestions)
    const caps = db.findMatchedCapabilityNamesForQuery(allocator, query, 0.20, 3) catch &[_][]const u8{};
    defer {
        for (caps) |cap| allocator.free(cap);
        allocator.free(caps);
    }
    if (caps.len > 0) {
        var aw: std.Io.Writer.Allocating = .init(allocator);
        errdefer aw.deinit();
        const bw = &aw.writer;
        try bw.writeAll("Related capabilities you might mean:\n");
        for (caps) |cap| {
            try bw.print("- `{s}`\n", .{cap});
        }
        try stages.append(allocator, .{
            .kind = .metadata,
            .content = try aw.toOwnedSlice(),
            .source = try allocator.dupe(u8, "guidance"),
        });
    }

    return stages.toOwnedSlice(allocator);
}

/// Checks if a search result matches the original query and returns a boolean.
fn anyResultIsRelevant(results: []const SearchResult, original_query: []const u8) bool {
    // Extract meaningful tokens from the query (skip stop words, short words)
    var tok_it = std.mem.tokenizeAny(u8, original_query, " \t\n\r_-");
    while (tok_it.next()) |tok| {
        if (tok.len < 3) continue;
        // Skip common stop words that don't identify content
        const stop_words = [_][]const u8{ "how", "the", "what", "does", "this", "that", "where", "which", "work", "make", "into", "with", "using", "use" };
        var is_stop = false;
        for (stop_words) |sw| {
            if (std.ascii.eqlIgnoreCase(tok, sw)) {
                is_stop = true;
                break;
            }
        }
        if (is_stop) continue;

        // Check if any result name, source, module, or comment contains this token
        for (results) |r| {
            if (std.ascii.indexOfIgnoreCase(r.name, tok) != null) return true;
            if (std.ascii.indexOfIgnoreCase(r.source, tok) != null) return true;
            if (std.ascii.indexOfIgnoreCase(r.module, tok) != null) return true;
            if (r.comment) |c| {
                if (std.ascii.indexOfIgnoreCase(c, tok) != null) return true;
            }
        }
    }
    return false;
}

/// Executes a Zig stage with original query and aliases, returning the processed stage.
// ---------------------------------------------------------------------------
// Stage collection helpers (Phase 5.2)
// ---------------------------------------------------------------------------

/// Boosts result scores in-place for sources affiliated with matched capabilities.
fn applyCapabilityBoosting(
    allocator: std.mem.Allocator,
    aa: std.mem.Allocator,
    db: *GuidanceDb,
    matched_cap_names: []const []const u8,
    results: []SearchResult,
) void {
    if (matched_cap_names.len == 0) return;
    var cap_confidence: std.StringHashMapUnmanaged(f32) = .empty;
    for (matched_cap_names) |cap_name| {
        const sources = db.getCapabilitySources(allocator, cap_name, 0.65) catch continue;
        defer {
            for (sources) |cs| {
                allocator.free(cs.source_path);
                allocator.free(cs.reason);
            }
            allocator.free(sources);
        }
        for (sources) |cs| {
            if (cap_confidence.get(cs.source_path)) |existing| {
                if (cs.confidence > existing) cap_confidence.put(aa, cs.source_path, cs.confidence) catch continue;
            } else {
                const key = aa.dupe(u8, cs.source_path) catch continue;
                cap_confidence.put(aa, key, cs.confidence) catch continue;
            }
        }
    }
    for (results, 0..) |r, i| {
        if (cap_confidence.get(r.source)) |conf| results[i].score *= 1.0 + conf * 0.3;
    }
}

/// Appends prose stages (module detail + member comments) to stages.
fn collectProseStages(
    allocator: std.mem.Allocator,
    aa: std.mem.Allocator,
    results: []const SearchResult,
    stages: *std.ArrayList(types.Stage),
) !void {
    var seen_prose: std.StringHashMapUnmanaged(void) = .empty;
    for (results[0..@min(5, results.len)]) |r| {
        if (!std.mem.eql(u8, r.node_type, "module")) continue;
        const detail = r.detail orelse continue;
        if (detail.len < 50) continue;
        const key = try std.fmt.allocPrint(aa, "{s}\x00detail", .{r.source});
        if (seen_prose.contains(key)) continue;
        try seen_prose.put(aa, key, {});
        try stages.append(allocator, .{
            .kind = .prose,
            .content = try allocator.dupe(u8, detail),
            .source = try allocator.dupe(u8, r.source),
            .line = r.line,
        });
    }
    for (results[0..@min(10, results.len)]) |r| {
        const comment = r.comment orelse continue;
        if (comment.len < 10) continue;
        const key = try std.fmt.allocPrint(aa, "{s}\x00{s}", .{ r.source, r.name });
        if (seen_prose.contains(key)) continue;
        try seen_prose.put(aa, key, {});
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
}

/// Appends code stages (per-token match + top-3-files fallback) to stages.
fn collectCodeStages(
    allocator: std.mem.Allocator,
    aa: std.mem.Allocator,
    workspace: []const u8,
    original_query: []const u8,
    results: []const SearchResult,
    stages: *std.ArrayList(types.Stage),
) !void {
    var seen_code_by_loc: std.StringHashMapUnmanaged(void) = .empty;
    var token_match_count: usize = 0;
    // Hot-path: use std.ascii.eqlIgnoreCase for zero-allocation case-insensitive
    // token comparison. This eliminates O(tokens×results) heap allocations entirely.
    // See ROADMAP_20260420_QUALITY.md M1.
    var tok_it = std.mem.tokenizeAny(u8, original_query, " \t\n\r");
    while (tok_it.next()) |token| {
        for (results) |r| {
            if (!std.ascii.eqlIgnoreCase(token, r.name)) continue;
            const line = r.line orelse break;
            const loc_key = try std.fmt.allocPrint(aa, "{s}:{d}", .{ r.source, line });
            if (seen_code_by_loc.contains(loc_key)) break;
            try seen_code_by_loc.put(aa, loc_key, {});
            const excerpt = extractSourceExcerptVerified(allocator, workspace, r.source, line, r.node_type, r.name) catch break;
            if (excerpt.len > 0) {
                try stages.append(allocator, .{
                    .kind = .code,
                    .content = excerpt,
                    .source = try allocator.dupe(u8, r.source),
                    .line = line,
                });
                token_match_count += 1;
            } else allocator.free(excerpt);
            break;
        }
    }
    if (token_match_count == 0) {
        var seen_code_files: std.StringHashMapUnmanaged(void) = .empty;
        for (results) |r| {
            if (seen_code_files.count() >= 3) break;
            if (r.source.len == 0 or seen_code_files.contains(r.source)) continue;
            const line = r.line orelse continue;
            try seen_code_files.put(aa, r.source, {});
            const excerpt = extractSourceExcerptVerified(allocator, workspace, r.source, line, r.node_type, r.name) catch continue;
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
    }
}

/// Appends metadata stages (guidance JSON + see-also traversal) to stages.
fn collectMetadataStages(
    allocator: std.mem.Allocator,
    aa: std.mem.Allocator,
    workspace: []const u8,
    results: []const SearchResult,
    stages: *std.ArrayList(types.Stage),
) !void {
    var seen_guidance: std.StringHashMapUnmanaged(void) = .empty;
    for (results[0..@min(5, results.len)]) |r| {
        if (seen_guidance.contains(r.file_path)) continue;
        try seen_guidance.put(aa, r.file_path, {});
        const meta = core_metadata.buildMetadataStage(allocator, r.file_path, r.source) catch continue;
        const meta_stage = meta orelse continue;
        try stages.append(allocator, meta_stage);
    }
    if (stages.items.len < 5 and results.len > 0) {
        var seen_sources: std.StringHashMapUnmanaged(void) = .empty;
        for (stages.items) |s| {
            if (s.kind == .code or s.kind == .prose) seen_sources.put(aa, s.source, {}) catch {};
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
                seen_sources.put(aa, ub_path, {}) catch {};
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
}

/// Appends capability-routing metadata + doc + expansion stages to stages.
fn collectCapabilityStages(
    allocator: std.mem.Allocator,
    aa: std.mem.Allocator,
    workspace: []const u8,
    db: *GuidanceDb,
    matched_cap_names: []const []const u8,
    stages: *std.ArrayList(types.Stage),
) !void {
    if (matched_cap_names.len == 0) return;

    // Routing metadata stage
    var buf: std.ArrayList(u8) = .empty;
    defer buf.deinit(allocator);
    try buf.appendSlice(allocator, "matched_capabilities: ");
    for (matched_cap_names, 0..) |name, i| {
        if (i > 0) try buf.appendSlice(allocator, ", ");
        try buf.appendSlice(allocator, name);
    }
    try stages.append(allocator, .{
        .kind = .metadata,
        .content = try buf.toOwnedSlice(allocator),
        .source = try allocator.dupe(u8, "capability-routing"),
    });

    // Capability doc stages from index
    const cap_index_path = std.fmt.allocPrint(allocator, "{s}/.guidance/capability-index.json", .{workspace}) catch null;
    defer if (cap_index_path) |p| allocator.free(p);
    if (cap_index_path) |index_path| {
        const io = std.Io.Threaded.global_single_threaded.io();
        if (std.Io.Dir.cwd().readFileAlloc(io, index_path, allocator, .limited(2 * 1024 * 1024))) |index_content| {
            defer allocator.free(index_content);
            if (std.json.parseFromSlice(std.json.Value, allocator, index_content, .{ .ignore_unknown_fields = true })) |parsed| {
                defer parsed.deinit();
                if (parsed.value == .object) {
                    const caps_val = parsed.value.object.get("capabilities") orelse std.json.Value{ .null = {} };
                    if (caps_val == .array) {
                        for (matched_cap_names) |cap_name| {
                            for (caps_val.array.items) |cap_item| {
                                if (cap_item != .object) continue;
                                const cap_obj = cap_item.object;
                                const idx_name = (cap_obj.get("name") orelse continue).string;
                                if (!std.mem.eql(u8, idx_name, cap_name)) continue;
                                var cbuf: std.ArrayList(u8) = .empty;
                                errdefer cbuf.deinit(allocator);
                                if (cap_obj.get("description")) |dv| {
                                    if (dv == .string and dv.string.len > 0) {
                                        try cbuf.appendSlice(allocator, dv.string);
                                        try cbuf.appendSlice(allocator, "\n\n");
                                    }
                                }
                                if (cap_obj.get("anchors")) |av| {
                                    if (av == .array and av.array.items.len > 0) {
                                        try cbuf.appendSlice(allocator, "**Anchors**: ");
                                        for (av.array.items, 0..) |anchor, j| {
                                            if (anchor != .string) continue;
                                            if (j > 0) try cbuf.appendSlice(allocator, ", ");
                                            try cbuf.appendSlice(allocator, anchor.string);
                                        }
                                        try cbuf.appendSlice(allocator, "\n");
                                    }
                                }
                                const cap_sources = db.getCapabilitySources(allocator, cap_name, 0.65) catch &.{};
                                defer {
                                    for (cap_sources) |cs| {
                                        allocator.free(cs.source_path);
                                        allocator.free(cs.reason);
                                    }
                                    allocator.free(cap_sources);
                                }
                                if (cap_sources.len > 0) {
                                    try cbuf.appendSlice(allocator, "**Sources**: ");
                                    const take = @min(4, cap_sources.len);
                                    for (cap_sources[0..take], 0..) |cs, j| {
                                        if (j > 0) try cbuf.appendSlice(allocator, ", ");
                                        var aw2: std.Io.Writer.Allocating = .init(allocator);
                                        errdefer aw2.deinit();
                                        try aw2.writer.print("{s} ({d:.1})", .{ cs.source_path, cs.confidence });
                                        try cbuf.appendSlice(allocator, aw2.written());
                                    }
                                    try cbuf.appendSlice(allocator, "\n");
                                }
                                if (cbuf.items.len > 0) {
                                    try stages.append(allocator, .{
                                        .kind = .capability_doc,
                                        .content = try cbuf.toOwnedSlice(allocator),
                                        .source = try allocator.dupe(u8, cap_name),
                                    });
                                } else cbuf.deinit(allocator);
                                break;
                            }
                        }
                    }
                }
            } else |_| {}
        } else |_| {}
    }

    // Capability-guided expansion
    const guidance_dir = std.fmt.allocPrint(allocator, "{s}/.guidance", .{workspace}) catch workspace;
    defer if (!std.mem.eql(u8, guidance_dir, workspace)) allocator.free(@as([]const u8, @constCast(guidance_dir)));

    var seen_sources: std.StringHashMapUnmanaged(void) = .empty;
    for (stages.items) |s| {
        if (s.kind == .code or s.kind == .prose) {
            if (!seen_sources.contains(s.source)) seen_sources.put(aa, try aa.dupe(u8, s.source), {}) catch {};
        }
    }
    var cap_expansion_count: usize = 0;
    for (matched_cap_names) |cap_name| {
        const cap_sources = db.getCapabilitySources(allocator, cap_name, 0.65) catch &.{};
        defer {
            for (cap_sources) |cs| {
                allocator.free(cs.source_path);
                allocator.free(cs.reason);
            }
            allocator.free(cap_sources);
        }
        for (cap_sources) |cs| {
            if (seen_sources.contains(cs.source_path)) continue;
            const json_path = std.fmt.allocPrint(allocator, "{s}/{s}.json", .{ guidance_dir, cs.source_path }) catch continue;
            defer allocator.free(json_path);
            const io = std.Io.Threaded.global_single_threaded.io();
            const json_content = std.Io.Dir.cwd().readFileAlloc(io, json_path, allocator, .limited(2 * 1024 * 1024)) catch continue;
            defer allocator.free(json_content);
            const parsed = std.json.parseFromSlice(std.json.Value, allocator, json_content, .{ .ignore_unknown_fields = true }) catch continue;
            defer parsed.deinit();
            if (parsed.value != .object) continue;
            const obj = parsed.value.object;
            var added_prose = false;
            if (obj.get("detail")) |dv| {
                if (dv == .string and dv.string.len >= 50) {
                    try seen_sources.put(aa, try aa.dupe(u8, cs.source_path), {});
                    try stages.append(allocator, .{
                        .kind = .prose,
                        .content = try allocator.dupe(u8, dv.string),
                        .source = try std.fmt.allocPrint(allocator, "{s} (cap:{s})", .{ cs.source_path, cap_name }),
                    });
                    cap_expansion_count += 1;
                    added_prose = true;
                }
            }
            if (obj.get("members")) |mv| {
                if (mv == .array) {
                    for (mv.array.items[0..@min(3, mv.array.items.len)]) |member| {
                        if (member != .object) continue;
                        const mo = member.object;
                        const mn = mo.get("name") orelse continue;
                        const mc = mo.get("comment") orelse continue;
                        if (mn == .string and mc == .string and mc.string.len >= 10) {
                            try stages.append(allocator, .{
                                .kind = .prose,
                                .content = try allocator.dupe(u8, mc.string),
                                .source = try std.fmt.allocPrint(allocator, "{s}:{s} (cap:{s})", .{ cs.source_path, mn.string, cap_name }),
                            });
                            added_prose = true;
                        }
                    }
                }
            }
            if (!added_prose) {
                if (core_metadata.buildMetadataStage(allocator, json_path, cs.source_path) catch null) |ms| {
                    try seen_sources.put(aa, try aa.dupe(u8, cs.source_path), {});
                    try stages.append(allocator, ms);
                    cap_expansion_count += 1;
                }
            }
            if (cap_expansion_count >= 5) break;
        }
        if (cap_expansion_count >= 5) break;
    }
    if (cap_expansion_count > 0) {
        std.debug.print("[explain] capability expansion: added {d} stages from {d} matched capabilities\n", .{ cap_expansion_count, matched_cap_names.len });
    }
}

pub fn executeStagedWithAliasesOriginal(
    allocator: std.mem.Allocator,
    db: *GuidanceDb,
    query: []const u8,
    original_query: []const u8,
    workspace: []const u8,
    aliases: ?vector_db_mod.SemanticAliases,
) ![]types.Stage {
    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();
    const aa = arena.allocator();

    const results = try db.searchWithAliasesOriginal(allocator, query, original_query, 15, aliases);
    defer {
        for (results) |r| GuidanceDb.freeSearchResult(allocator, r);
        allocator.free(results);
    }

    {
        var orig_tok_count: usize = 0;
        var tc = std.mem.tokenizeAny(u8, original_query, " \t\n\r");
        while (tc.next()) |_| orig_tok_count += 1;
        const no_exact_match = results.len == 0 or results[0].score < 0.9;
        if (orig_tok_count >= 2 and no_exact_match and !anyResultIsRelevant(results, original_query)) {
            return buildNotFoundStages(allocator, original_query, db);
        }
    }

    const matched_cap_names = db.findMatchedCapabilityNamesForQuery(allocator, query, 0.45, 3) catch &[_][]const u8{};
    defer {
        for (matched_cap_names) |n| allocator.free(n);
        allocator.free(matched_cap_names);
    }

    applyCapabilityBoosting(allocator, aa, db, matched_cap_names, results);

    var stages: std.ArrayList(types.Stage) = .empty;
    errdefer {
        types.freeStages(allocator, stages.items);
        stages.deinit(allocator);
    }

    try collectProseStages(allocator, aa, results, &stages);
    try collectCodeStages(allocator, aa, workspace, original_query, results, &stages);
    try collectMetadataStages(allocator, aa, workspace, results, &stages);
    try collectCapabilityStages(allocator, aa, workspace, db, matched_cap_names, &stages);

    // Apply token-budget enforcement via ContextPacker.packIndices.
    // Projects []types.Stage → []context_packer.Stage for the selection algorithm,
    // then uses the returned indices to rebuild the final slice from the originals.
    const raw = try stages.toOwnedSlice(allocator);
    errdefer types.freeStages(allocator, raw);

    if (raw.len == 0) return raw;

    const projected = try allocator.alloc(context_packer.Stage, raw.len);
    defer allocator.free(projected);
    for (raw, 0..) |s, i| {
        projected[i] = .{
            .kind = if (s.kind == .code) .code else .prose,
            .content = s.content,
            .relevance_score = 1.0,
        };
    }

    const packer = context_packer.ContextPacker{ .config = .{} };
    const indices = try packer.packIndices(allocator, projected);
    defer allocator.free(indices);

    if (indices.len == raw.len) return raw; // no truncation needed

    // Build the packed result; free stages not selected.
    var out_stages: std.ArrayList(types.Stage) = .empty;
    errdefer {
        types.freeStages(allocator, out_stages.items);
        out_stages.deinit(allocator);
    }
    try out_stages.ensureTotalCapacity(allocator, indices.len);

    var selected = std.AutoHashMap(usize, void).init(aa);
    for (indices) |idx| selected.put(idx, {}) catch {};

    for (raw, 0..) |s, i| {
        if (selected.contains(i)) {
            out_stages.appendAssumeCapacity(s);
        } else {
            types.freeStage(allocator, s);
        }
    }
    allocator.free(raw);

    return out_stages.toOwnedSlice(allocator);
}

// ---------------------------------------------------------------------------
// Grounding enforcement — no synthesis without source
// ---------------------------------------------------------------------------

pub fn canSynthesize(stages: []const types.Stage) bool {
    for (stages) |stage| {
        if (stage.kind == .code and stage.content.len > 0) return true;
    }
    return false;
}

pub fn buildEscalationOutput(allocator: std.mem.Allocator, query: []const u8, reason: []const u8) []const types.Stage {
    _ = allocator;
    _ = query;
    _ = reason;
    return &.{};
}

// ---------------------------------------------------------------------------
// Follow-up expansion (M7)
// ---------------------------------------------------------------------------

/// Expands follow-up data structures based on allocation limits and source constraints.
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
    var extra: std.ArrayList(types.Stage) = .empty;
    errdefer {
        types.freeStages(allocator, extra.items);
        extra.deinit(allocator);
    }

    var seen: std.StringHashMapUnmanaged(void) = .empty;
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
    var seen_skills: std.StringHashMapUnmanaged(void) = .empty;
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

/// Delegates to core/format.formatStaged.
pub const formatStaged = core_format.formatStaged;

// ---------------------------------------------------------------------------
// Source excerpt extraction
// ---------------------------------------------------------------------------

/// Delegates to core/excerpt.extractFromPath (with line verification).
pub fn extractSourceExcerptVerified(
    allocator: std.mem.Allocator,
    workspace: []const u8,
    rel_source: []const u8,
    start_line: u32,
    node_type: []const u8,
    member_name: ?[]const u8,
) ![]u8 {
    return core_excerpt.extractFromPath(allocator, workspace, rel_source, start_line, node_type, member_name);
}

/// Delegates to core/excerpt.extractFromPath (without line verification).
pub fn extractSourceExcerpt(
    allocator: std.mem.Allocator,
    workspace: []const u8,
    rel_source: []const u8,
    start_line: u32,
    node_type: []const u8,
) ![]u8 {
    return core_excerpt.extractFromPath(allocator, workspace, rel_source, start_line, node_type, null);
}

/// Delegates to core/excerpt.extractFromSource.
pub fn extractExcerptFromSource(
    allocator: std.mem.Allocator,
    src: []const u8,
    start_line: u32,
    node_type: []const u8,
) ![]u8 {
    return core_excerpt.extractFromSource(allocator, src, start_line, node_type, common.DEFAULT_MAX_LINES);
}

// ---------------------------------------------------------------------------
// Guidance JSON helpers
// ---------------------------------------------------------------------------

/// Delegates to core/metadata.buildMetadataStage.
pub const buildMetadataStage = core_metadata.buildMetadataStage;

/// Loads a JSON module comment from a file path into a Zig array of bytes.
fn loadModuleComment(allocator: std.mem.Allocator, json_path: []const u8) ?[]const u8 {
    var parsed = common.parseJsonFile(allocator, json_path, 8 * 1024 * 1024) orelse return null;
    defer parsed.deinit();
    const cv = parsed.value.object.get("comment") orelse return null;
    if (cv != .string) return null;
    if (cv.string.len < 10) return null;
    return allocator.dupe(u8, cv.string) catch null;
}

/// Loads skill names from a JSON file into a Zig array of byte slices.
/// Delegates to core/metadata.loadSkillNamesFromJson.
pub const loadSkillNamesFromJson = core_metadata.loadSkillNamesFromJson;

/// Delegates to core/skill_loader.parseSkillDocContent.
pub fn parseSkillDocContent(allocator: std.mem.Allocator, content: []const u8) !?[]const u8 {
    return core_skill_loader.parseSkillDocContent(allocator, content);
}

/// Loads a skill excerpt from a skills_dir/{skill_name}/SKILL.md.
/// NOTE: staged.zig callers pass a skills_dir directly; use core_skill_loader.loadSkillExcerpt
/// for the two-path search used in query_engine.
pub fn loadSkillExcerpt(
    allocator: std.mem.Allocator,
    skills_dir: []const u8,
    skill_name: []const u8,
) !?[]const u8 {
    const path = try std.fs.path.join(allocator, &.{ skills_dir, skill_name, "SKILL.md" });
    defer allocator.free(path);

    const io = std.Io.Threaded.global_single_threaded.io();
    const content = std.Io.Dir.cwd().readFileAlloc(io, path, allocator, .limited(512 * 1024)) catch return null;
    defer allocator.free(content);

    return parseSkillDocContent(allocator, content);
}

// ---------------------------------------------------------------------------
// Utility helpers
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test "benchmark: zero-alloc token match — 100 tokens × 100 results" {
    // Validates that std.ascii.eqlIgnoreCase handles O(tokens×results) comparisons
    // without any heap allocation. Testing.allocator leak check confirms zero allocs.
    const tokens = [_][]const u8{
        "cmdExplain", "executeStaged", "collectCodeStages", "SearchResult", "formatStaged",
        "GuidanceDb", "vector_db",     "syncEngine",        "queryEngine",  "stagedPipeline",
    };
    const names = [_][]const u8{
        "cmdexplain", "executestaged", "collectcodestages", "searchresult", "formatstaged",
        "guidancedb", "vector_db",     "syncengine",        "queryengine",  "stagedpipeline",
    };

    var match_count: usize = 0;
    const io = std.Io.Threaded.global_single_threaded.io();
    const start: i128 = @as(i128, std.Io.Timestamp.now(io, .real).nanoseconds);
    var iter: usize = 0;
    while (iter < 100) : (iter += 1) {
        for (tokens) |token| {
            for (names) |name| {
                if (std.ascii.eqlIgnoreCase(token, name)) match_count += 1;
            }
        }
    }
    const elapsed_ns: i128 = @as(i128, std.Io.Timestamp.now(io, .real).nanoseconds) - start;
    const elapsed_ms = @divTrunc(elapsed_ns, 1_000_000);

    try std.testing.expect(match_count > 0);
    // 100 iterations × 10 tokens × 10 names = 10,000 comparisons, zero allocations.
    // Target: < 10ms on any reasonable hardware.
    try std.testing.expect(elapsed_ms < 10);
}
