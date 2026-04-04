//! staged.zig — Staged explain pipeline for `guidance explain`.
//!
//! Implements the hybrid vector + keyword search pipeline for the explain
//! subcommand. The key exported functions are:
//!
//!   executeStaged()   — search → collect Stage slices
//!   expandFollowUps() — follow see_also / skill refs → more stages
//!   formatStaged()    — render []Stage to markdown output

const std = @import("std");
const vector_db_mod = @import("vector");
const types = @import("types.zig");
const llm = @import("common");
const line_verify = @import("line_verify.zig");
const doc_parser = @import("doc_parser.zig");

const GuidanceDb = vector_db_mod.GuidanceDb;
const SearchResult = GuidanceDb.SearchResult;

// ---------------------------------------------------------------------------
// Stage collection entry point
// ---------------------------------------------------------------------------

/// Executes a staged query using the provided allocator, database, and workspace, returning a Stage slice.
pub fn executeStaged(
    allocator: std.mem.Allocator,
    db: *GuidanceDb,
    query: []const u8,
    workspace: []const u8,
) ![]types.Stage {
    return executeStagedWithAliases(allocator, db, query, workspace, null);
}

/// Executes a Zig stage with optional alias resolution, returning the processed stage.
pub fn executeStagedWithAliases(
    allocator: std.mem.Allocator,
    db: *GuidanceDb,
    query: []const u8,
    workspace: []const u8,
    aliases: ?vector_db_mod.SemanticAliases,
) ![]types.Stage {
    return executeStagedWithAliasesOriginal(allocator, db, query, query, workspace, aliases);
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
    var stages: std.ArrayList(types.Stage) = .{};
    errdefer {
        types.freeStages(allocator, stages.items);
        stages.deinit(allocator);
    }

    // Stage 1: Not-found prose message
    const msg = try std.fmt.allocPrint(allocator, "No code found matching '{s}' in this codebase. " ++
        "The query did not match any indexed source files with sufficient confidence.\n\n" ++
        "Try:\n" ++
        "- A more specific identifier (e.g. a function or struct name)\n" ++
        "- A different keyword from the codebase vocabulary\n" ++
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
        var buf: std.ArrayList(u8) = .{};
        defer buf.deinit(allocator);
        const bw = buf.writer(allocator);
        try bw.writeAll("Related capabilities you might mean:\n");
        for (caps) |cap| {
            try bw.print("- `{s}`\n", .{cap});
        }
        try stages.append(allocator, .{
            .kind = .metadata,
            .content = try buf.toOwnedSlice(allocator),
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
pub fn executeStagedWithAliasesOriginal(
    allocator: std.mem.Allocator,
    db: *GuidanceDb,
    query: []const u8,
    original_query: []const u8,
    workspace: []const u8,
    aliases: ?vector_db_mod.SemanticAliases,
) ![]types.Stage {
    // ── Vector/hybrid search with alias expansion ─────────────────────────────
    const results = try db.searchWithAliasesOriginal(allocator, query, original_query, 15, aliases);
    defer {
        for (results) |r| GuidanceDb.freeSearchResult(allocator, r);
        allocator.free(results);
    }

    // ── M7: Negative answer path ──────────────────────────────────────────────
    // For multi-word queries: if no result name or source contains any key query
    // token AND no Phase-1 exact match was found (score < 0.9), return "not found".
    //
    // Phase 1 (exact name match) sets score = 1.0 — always relevant, skip check.
    // Phase 2/3 (vector/hybrid RRF) sets score in [0.004, 0.04] — unreliable alone.
    // We use token overlap as the primary signal, score only to detect Phase 1.
    {
        var orig_tok_count: usize = 0;
        var tc = std.mem.tokenizeAny(u8, original_query, " \t\n\r");
        while (tc.next()) |_| orig_tok_count += 1;

        const is_multi_word = orig_tok_count >= 2;
        // If top result score >= 0.9, Phase 1 exact match was found — definitely relevant.
        const no_exact_match = results.len == 0 or results[0].score < 0.9;

        if (is_multi_word and no_exact_match and !anyResultIsRelevant(results, original_query)) {
            return buildNotFoundStages(allocator, original_query, db);
        }
    }

    // ── Capability routing: which capabilities influenced the search ──────────
    const matched_cap_names = db.findMatchedCapabilityNamesForQuery(allocator, query, 0.45, 3) catch &[_][]const u8{};
    defer {
        for (matched_cap_names) |n| allocator.free(n);
        allocator.free(matched_cap_names);
    }

    // ── M7.2: Capability score boosting ───────────────────────────────────────
    // Boost scores for results whose source file is a capability source.
    // boost = 1.0 + (confidence * 0.3), so range is 1.0–1.3
    if (matched_cap_names.len > 0) {
        var cap_confidence: std.StringHashMapUnmanaged(f32) = .{};
        defer {
            var it = cap_confidence.iterator();
            while (it.next()) |entry| allocator.free(entry.key_ptr.*);
            cap_confidence.deinit(allocator);
        }

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
                    if (cs.confidence > existing) {
                        cap_confidence.put(allocator, cs.source_path, cs.confidence) catch continue;
                    }
                } else {
                    const key = allocator.dupe(u8, cs.source_path) catch continue;
                    cap_confidence.put(allocator, key, cs.confidence) catch {
                        allocator.free(key);
                        continue;
                    };
                }
            }
        }

        for (results, 0..) |r, i| {
            if (cap_confidence.get(r.source)) |conf| {
                const boost = 1.0 + conf * 0.3;
                results[i].score *= boost;
            }
        }
    }

    var stages: std.ArrayList(types.Stage) = .{};
    errdefer {
        types.freeStages(allocator, stages.items);
        stages.deinit(allocator);
    }

    // ── Prose stages: module detail + member comments ────────────────────────────────
    // Track seen (source, name) pairs to avoid duplicate prose entries.
    var seen_prose: std.StringHashMapUnmanaged(void) = .{};
    defer {
        var it = seen_prose.keyIterator();
        while (it.next()) |k| allocator.free(k.*);
        seen_prose.deinit(allocator);
    }

    // First, add module detail stages (high-value comprehensive documentation)
    for (results[0..@min(5, results.len)]) |r| {
        if (!std.mem.eql(u8, r.node_type, "module")) continue;
        const detail = r.detail orelse continue;
        if (detail.len < 50) continue;

        const key = try std.fmt.allocPrint(allocator, "{s}\x00detail", .{r.source});
        defer allocator.free(key);
        if (seen_prose.contains(key)) continue;
        try seen_prose.put(allocator, try allocator.dupe(u8, key), {});

        try stages.append(allocator, .{
            .kind = .prose,
            .content = try allocator.dupe(u8, detail),
            .source = try allocator.dupe(u8, r.source),
            .line = r.line,
        });
    }

    // Then add member comments
    for (results[0..@min(10, results.len)]) |r| {
        const comment = r.comment orelse continue;
        if (comment.len < 10) continue;

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

    // ── Code stages: per-token exact matching then top-3-files fallback ─────────
    // For each token in the original query, find an exact name match in results
    // and emit its code excerpt.  This handles multi-keyword queries like
    // "filterStages dupeStage" by showing a separate excerpt for each identifier.
    // Falls back to top-3-unique-files when no token matches exactly (e.g. for
    // pure concept queries like "staged pipeline architecture").
    var seen_code_by_loc: std.StringHashMapUnmanaged(void) = .{};
    defer {
        var kit = seen_code_by_loc.keyIterator();
        while (kit.next()) |k| allocator.free(k.*);
        seen_code_by_loc.deinit(allocator);
    }

    var token_match_count: usize = 0;
    {
        var tok_it = std.mem.tokenizeAny(u8, original_query, " \t\n\r");
        while (tok_it.next()) |token| {
            const token_lower = std.ascii.allocLowerString(allocator, token) catch continue;
            defer allocator.free(token_lower);
            for (results) |r| {
                const name_lower = std.ascii.allocLowerString(allocator, r.name) catch continue;
                defer allocator.free(name_lower);
                if (!std.mem.eql(u8, token_lower, name_lower)) continue;
                const line = r.line orelse break;
                const loc_key = try std.fmt.allocPrint(allocator, "{s}:{d}", .{ r.source, line });
                if (seen_code_by_loc.contains(loc_key)) {
                    allocator.free(loc_key);
                    break;
                }
                try seen_code_by_loc.put(allocator, loc_key, {});
                const excerpt = extractSourceExcerptVerified(allocator, workspace, r.source, line, r.node_type, r.name) catch break;
                if (excerpt.len > 0) {
                    try stages.append(allocator, .{
                        .kind = .code,
                        .content = excerpt,
                        .source = try allocator.dupe(u8, r.source),
                        .line = line,
                    });
                    token_match_count += 1;
                } else {
                    allocator.free(excerpt);
                }
                break; // one match per token
            }
        }
    }

    // Fallback: no token matched exactly — show top 3 unique source files
    if (token_match_count == 0) {
        var seen_code_files: std.StringHashMapUnmanaged(void) = .{};
        defer seen_code_files.deinit(allocator);

        for (results) |r| {
            if (seen_code_files.count() >= 3) break;
            if (r.source.len == 0) continue;
            if (seen_code_files.contains(r.source)) continue;
            const line = r.line orelse continue;

            try seen_code_files.put(allocator, r.source, {});

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
        defer seen_sources.deinit(allocator);

        for (stages.items) |s| {
            if (s.kind == .code or s.kind == .prose) {
                seen_sources.put(allocator, s.source, {}) catch {};
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

                seen_sources.put(allocator, ub_path, {}) catch {};
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

    // ── Matched-Capabilities metadata stage ───────────────────────────────────
    // Emit a metadata stage recording which capabilities routed this query,
    // so formatStaged() can render a "Matched Capabilities" line.
    if (matched_cap_names.len > 0) {
        var buf: std.ArrayList(u8) = .{};
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
    }

    // ── Capability doc stages (M7.3) ──────────────────────────────────────────
    // Emit a capability_doc stage for each matched capability so formatStaged()
    // can render "## Capability: name" blocks with description, anchors, and
    // top source files. This requires loading the capability-index.json.
    if (matched_cap_names.len > 0) {
        const cap_index_path = std.fmt.allocPrint(
            allocator,
            "{s}/.guidance/capability-index.json",
            .{workspace},
        ) catch null;
        defer if (cap_index_path) |p| allocator.free(p);

        if (cap_index_path) |index_path| {
            if (std.fs.cwd().readFileAlloc(allocator, index_path, 2 * 1024 * 1024)) |index_content| {
                defer allocator.free(index_content);
                if (std.json.parseFromSlice(std.json.Value, allocator, index_content, .{ .ignore_unknown_fields = true })) |parsed| {
                    defer parsed.deinit();
                    if (parsed.value == .object) {
                        const caps_val = parsed.value.object.get("capabilities") orelse
                            std.json.Value{ .null = {} };
                        if (caps_val == .array) {
                            for (matched_cap_names) |cap_name| {
                                // Find this capability in the index
                                for (caps_val.array.items) |cap_item| {
                                    if (cap_item != .object) continue;
                                    const cap_obj = cap_item.object;
                                    const idx_name = (cap_obj.get("name") orelse continue).string;
                                    if (!std.mem.eql(u8, idx_name, cap_name)) continue;

                                    // Build capability_doc content
                                    var cbuf: std.ArrayList(u8) = .{};
                                    errdefer cbuf.deinit(allocator);

                                    // Description
                                    if (cap_obj.get("description")) |dv| {
                                        if (dv == .string and dv.string.len > 0) {
                                            try cbuf.appendSlice(allocator, dv.string);
                                            try cbuf.appendSlice(allocator, "\n\n");
                                        }
                                    }

                                    // Anchors
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

                                    // Top source files from capability_sources
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
                                            try cbuf.writer(allocator).print("{s} ({d:.1})", .{ cs.source_path, cs.confidence });
                                        }
                                        try cbuf.appendSlice(allocator, "\n");
                                    }

                                    if (cbuf.items.len > 0) {
                                        try stages.append(allocator, .{
                                            .kind = .capability_doc,
                                            .content = try cbuf.toOwnedSlice(allocator),
                                            .source = try allocator.dupe(u8, cap_name),
                                        });
                                    } else {
                                        cbuf.deinit(allocator);
                                    }
                                    break;
                                }
                            }
                        }
                    }
                } else |_| {}
            } else |_| {}
        }
    }

    // ── Capability-guided expansion (M7) ───────────────────────────────────────
    // For each matched capability, query capability_sources and add source files
    // at confidence >= 0.7 that are not already in the search results.
    if (matched_cap_names.len > 0) {
        // Derive guidance_dir from workspace
        const guidance_dir = std.fmt.allocPrint(allocator, "{s}/.guidance", .{workspace}) catch workspace;
        defer if (!std.mem.eql(u8, guidance_dir, workspace)) allocator.free(@as([]const u8, @constCast(guidance_dir)));

        // Build seen_sources set from existing stages
        var seen_sources: std.StringHashMapUnmanaged(void) = .{};
        defer {
            var kit = seen_sources.keyIterator();
            while (kit.next()) |k| allocator.free(k.*);
            seen_sources.deinit(allocator);
        }

        for (stages.items) |s| {
            if (s.kind == .code or s.kind == .prose) {
                if (!seen_sources.contains(s.source)) {
                    try seen_sources.put(allocator, try allocator.dupe(u8, s.source), {});
                }
            }
        }

        // Track files added from capability expansion for verbose output
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
                // Skip if already seen
                if (seen_sources.contains(cs.source_path)) continue;

                // Try to load guidance JSON for this source.
                // source_path is already project-relative (e.g. "src/common/embeddings.zig"),
                // so the JSON lives at {guidance_dir}/{source_path}.json.
                const json_path = std.fmt.allocPrint(allocator, "{s}/{s}.json", .{ guidance_dir, cs.source_path }) catch continue;
                defer allocator.free(json_path);

                const json_content = std.fs.cwd().readFileAllocOptions(allocator, json_path, 2 * 1024 * 1024, null, .@"1", 0) catch continue;
                defer allocator.free(json_content);

                const parsed = std.json.parseFromSlice(std.json.Value, allocator, json_content, .{ .ignore_unknown_fields = true }) catch continue;
                defer parsed.deinit();

                if (parsed.value != .object) continue;
                const obj = parsed.value.object;

                var added_prose = false;

                // Add module detail if available
                if (obj.get("detail")) |detail_val| {
                    if (detail_val == .string and detail_val.string.len >= 50) {
                        try seen_sources.put(allocator, try allocator.dupe(u8, cs.source_path), {});
                        try stages.append(allocator, .{
                            .kind = .prose,
                            .content = try allocator.dupe(u8, detail_val.string),
                            .source = try std.fmt.allocPrint(allocator, "{s} (cap:{s})", .{ cs.source_path, cap_name }),
                        });
                        cap_expansion_count += 1;
                        added_prose = true;
                    }
                }

                // Add top member comments if available
                if (obj.get("members")) |members_val| {
                    if (members_val == .array) {
                        for (members_val.array.items[0..@min(3, members_val.array.items.len)]) |member| {
                            if (member != .object) continue;
                            const m_obj = member.object;
                            const m_name = m_obj.get("name") orelse continue;
                            const m_comment = m_obj.get("comment") orelse continue;
                            if (m_name == .string and m_comment == .string and m_comment.string.len >= 10) {
                                try stages.append(allocator, .{
                                    .kind = .prose,
                                    .content = try allocator.dupe(u8, m_comment.string),
                                    .source = try std.fmt.allocPrint(allocator, "{s}:{s} (cap:{s})", .{ cs.source_path, m_name.string, cap_name }),
                                });
                                added_prose = true;
                            }
                        }
                    }
                }

                // Fallback: when there is no prose (no detail, no member comments),
                // emit a metadata stage listing public member names. This surfaces
                // the file as relevant even before LLM comments are generated.
                if (!added_prose) {
                    const meta_stage = buildMetadataStage(allocator, json_path, cs.source_path) catch null;
                    if (meta_stage) |ms| {
                        try seen_sources.put(allocator, try allocator.dupe(u8, cs.source_path), {});
                        try stages.append(allocator, ms);
                        cap_expansion_count += 1;
                    }
                }

                // Limit expansion to prevent too many stages
                if (cap_expansion_count >= 5) break;
            }
            if (cap_expansion_count >= 5) break;
        }

        // Verbose output
        if (cap_expansion_count > 0) {
            std.debug.print("[explain] capability expansion: added {d} stages from {d} matched capabilities\n", .{ cap_expansion_count, matched_cap_names.len });
        }
    }

    return stages.toOwnedSlice(allocator);
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

/// Processes a Zig code snippet to format it into a structured output using provided allocator and stages.
pub fn formatStaged(
    allocator: std.mem.Allocator,
    query: []const u8,
    stages: []const types.Stage,
    summary: ?[]const u8,
    workspace: []const u8,
    capabilities_dir: []const u8,
    followup_keywords: ?[]const []const u8,
) ![]u8 {
    var out: std.ArrayList(u8) = .{};
    defer out.deinit(allocator);
    const w = out.writer(allocator);

    try w.print("# Explain: {s}\n\n", .{query});

    // ── Synthesized summary ────────────────────────────────────────────────────
    if (summary) |s| {
        const trimmed = std.mem.trim(u8, s, " \t\n\r");
        if (trimmed.len > 0) {
            try w.print("{s}\n\n", .{trimmed});
        }
    }

    // ── M7: not_found sentinel — render directly and return early ─────────────
    for (stages) |s| {
        if (s.kind != .not_found) continue;
        try w.print("{s}\n\n", .{std.mem.trim(u8, s.content, " \t\n\r")});
        return out.toOwnedSlice(allocator);
    }

    // ── Emit CODE stages only (prose/insight used for synthesis, not display) ──
    // Deduplicate by source:line so multi-keyword queries show every matched
    // function/struct/enum even when several live in the same file.
    var seen_code: std.StringHashMapUnmanaged(void) = .{};
    defer {
        var kit = seen_code.keyIterator();
        while (kit.next()) |k| allocator.free(k.*);
        seen_code.deinit(allocator);
    }

    for (stages) |s| {
        if (s.kind != .code) continue;
        const dedup_key = if (s.line) |ln|
            try std.fmt.allocPrint(allocator, "{s}:{d}", .{ s.source, ln })
        else
            try allocator.dupe(u8, s.source);
        if (seen_code.contains(dedup_key)) {
            allocator.free(dedup_key);
            continue;
        }
        try seen_code.put(allocator, dedup_key, {}); // map owns key

        const lang = llm.langFromPath(s.source);
        if (s.line) |ln| {
            const trimmed_content = std.mem.trimRight(u8, s.content, " \t\n\r");
            const end_ln = ln + std.mem.count(u8, trimmed_content, "\n");
            try w.print("## Source location: `{s}:{d}-{d}`\n\n```{s}\n", .{ s.source, ln, end_ln, lang });
        } else {
            try w.print("## Source location: `{s}`\n\n```{s}\n", .{ s.source, lang });
        }

        try w.print("{s}", .{s.content});
        try w.writeAll("\n```\n\n");
    }

    // ── Capability doc stages (M7.3) ─────────────────────────────────────────
    for (stages) |s| {
        if (s.kind != .capability_doc) continue;
        try w.print("## Capability: {s}\n\n", .{s.source});
        try w.print("{s}\n", .{std.mem.trim(u8, s.content, "\t\n\r")});
        try w.writeByte('\n');
    }

    // ── Skill doc stages ──────────────────────────────────────────────────────
    var skill_header_written = false;
    for (stages) |s| {
        if (s.kind != .skill_doc) continue;
        if (!skill_header_written) {
            try w.writeAll("## Knowledge Base\n\n**READ BEFORE IMPLEMENTING**\n\n");
            skill_header_written = true;
        }
        const excerpt = std.mem.trim(u8, s.content, "\t\n\r");
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
    var seen_caps_ref: std.StringHashMapUnmanaged(void) = .{};
    defer seen_caps_ref.deinit(allocator);
    var seen_matched_caps: std.StringHashMapUnmanaged(void) = .{};
    defer seen_matched_caps.deinit(allocator);

    var all_capabilities: std.ArrayList(u8) = .{};
    defer all_capabilities.deinit(allocator);
    var all_matched_caps: std.ArrayList(u8) = .{};
    defer all_matched_caps.deinit(allocator);

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
                    if (seen_kw.count() >= 10) continue; // cap at 10 items
                    try seen_kw.put(allocator, p, {});
                    if (all_keywords.items.len > 0) try all_keywords.appendSlice(allocator, ", ");
                    try all_keywords.append(allocator, '`');
                    try all_keywords.appendSlice(allocator, p);
                    try all_keywords.append(allocator, '`');
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
                    try all_see_also.append(allocator, '`');
                    try all_see_also.appendSlice(allocator, p);
                    try all_see_also.append(allocator, '`');
                }
            } else if (std.mem.startsWith(u8, line, "skills: ")) {
                const v = line["skills: ".len..];
                var parts = std.mem.splitSequence(u8, v, ", ");
                while (parts.next()) |part| {
                    const p = std.mem.trim(u8, part, " \t");
                    if (p.len == 0 or seen_skills_ref.contains(p)) continue;
                    try seen_skills_ref.put(allocator, p, {});
                    if (all_skills.items.len > 0) try all_skills.appendSlice(allocator, ", ");
                    try all_skills.append(allocator, '`');
                    try all_skills.appendSlice(allocator, p);
                    try all_skills.append(allocator, '`');
                }
            } else if (std.mem.startsWith(u8, line, "capabilities: ")) {
                const v = line["capabilities: ".len..];
                var parts = std.mem.splitSequence(u8, v, ", ");
                while (parts.next()) |part| {
                    const p = std.mem.trim(u8, part, " \t");
                    if (p.len == 0 or seen_caps_ref.contains(p)) continue;
                    try seen_caps_ref.put(allocator, p, {});
                    if (all_capabilities.items.len > 0) try all_capabilities.appendSlice(allocator, "\x00");
                    try all_capabilities.appendSlice(allocator, p);
                }
            } else if (std.mem.startsWith(u8, line, "matched_capabilities: ")) {
                const v = line["matched_capabilities: ".len..];
                var parts = std.mem.splitSequence(u8, v, ", ");
                while (parts.next()) |part| {
                    const p = std.mem.trim(u8, part, " \t");
                    if (p.len == 0 or seen_matched_caps.contains(p)) continue;
                    try seen_matched_caps.put(allocator, p, {});
                    if (all_matched_caps.items.len > 0) try all_matched_caps.appendSlice(allocator, ", ");
                    try all_matched_caps.append(allocator, '`');
                    try all_matched_caps.appendSlice(allocator, p);
                    try all_matched_caps.append(allocator, '`');
                }
            }
        }
    }

    if (all_keywords.items.len > 0 or all_see_also.items.len > 0 or all_skills.items.len > 0 or all_capabilities.items.len > 0 or all_matched_caps.items.len > 0) {
        if (!ref_header_written) {
            try w.writeAll("## References\n\n");
            ref_header_written = true;
        }
        if (all_matched_caps.items.len > 0) try w.print("- **Matched capabilities**: {s}\n", .{all_matched_caps.items});
        if (all_keywords.items.len > 0) try w.print("- **Suggested searches**: {s}\n", .{all_keywords.items});
        if (all_see_also.items.len > 0) try w.print("- **Used in**: {s}\n", .{all_see_also.items});
        if (all_skills.items.len > 0) try w.print("- **Skills**: {s}\n", .{all_skills.items});
        if (all_capabilities.items.len > 0) {
            try w.writeAll("- **Capabilities**: ");
            var cap_it = std.mem.splitScalar(u8, all_capabilities.items, '\x00');
            var cap_first = true;
            while (cap_it.next()) |cap_name| {
                if (!cap_first) try w.writeAll(", ");
                cap_first = false;
                if (capabilities_dir.len > 0) {
                    const abs = try std.fs.path.join(allocator, &.{ capabilities_dir, cap_name, "CAPABILITY.md" });
                    defer allocator.free(abs);
                    const rel = if (workspace.len > 0 and std.mem.startsWith(u8, abs, workspace) and abs.len > workspace.len)
                        abs[workspace.len + 1 ..]
                    else
                        abs;
                    try w.print("`{s}`", .{rel});
                } else {
                    try w.print("`{s}`", .{cap_name});
                }
            }
            try w.writeByte('\n');
        }
    }

    // ── Suggested follow-up keywords (from LLM synthesis)──────────────────────
    if (followup_keywords) |kw| {
        if (kw.len > 0) {
            try w.writeAll("\n## Suggested Queries\n\n");
            for (kw) |k| {
                try w.print("- `{s}`\n", .{k});
            }
        }
    }

    return out.toOwnedSlice(allocator);
}

// ---------------------------------------------------------------------------
// Source excerpt extraction
// ---------------------------------------------------------------------------

/// Extracts verified source excerpt from workspace, returning a slice of extracted bytes.
pub fn extractSourceExcerptVerified(
    allocator: std.mem.Allocator,
    workspace: []const u8,
    rel_source: []const u8,
    start_line: u32,
    node_type: []const u8,
    member_name: ?[]const u8,
) ![]u8 {
    const abs_path = try std.fs.path.join(allocator, &.{ workspace, rel_source });
    defer allocator.free(abs_path);

    const src = llm.readFileAlloc(allocator, abs_path, 10 * 1024 * 1024) orelse
        return allocator.dupe(u8, "");
    defer allocator.free(src);

    var effective_line = start_line;

    if (member_name) |name| {
        const member_type = memberTypeFromNodeType(node_type);
        const member = types.Member{
            .type = member_type,
            .name = name,
            .line = start_line,
        };
        const vr = try line_verify.verifyMemberLine(allocator, src, member);
        defer vr.deinit(allocator);
        if (!vr.verified) {
            if (vr.corrected_line) |cl| {
                std.log.debug("[staged] stale line for {s}:{s} — was {}, corrected to {}", .{ rel_source, name, start_line, cl });
                effective_line = cl;
            }
        }
    }

    return extractExcerptFromSource(allocator, src, effective_line, node_type);
}

/// Converts a list of node types into their corresponding Zig member type.
fn memberTypeFromNodeType(node_type: []const u8) types.MemberType {
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

/// Extracts a source excerpt from a workspace slice using an allocator and node metadata.
pub fn extractSourceExcerpt(
    allocator: std.mem.Allocator,
    workspace: []const u8,
    rel_source: []const u8,
    start_line: u32,
    node_type: []const u8,
) ![]u8 {
    const abs_path = try std.fs.path.join(allocator, &.{ workspace, rel_source });
    defer allocator.free(abs_path);

    const src = llm.readFileAlloc(allocator, abs_path, 10 * 1024 * 1024) orelse
        return allocator.dupe(u8, "");
    defer allocator.free(src);

    return extractExcerptFromSource(allocator, src, start_line, node_type);
}

/// Extracts a specified excerpt from a Zig source slice using an allocator and node types.
pub fn extractExcerptFromSource(
    allocator: std.mem.Allocator,
    src: []const u8,
    start_line: u32,
    node_type: []const u8,
) ![]u8 {
    const node_type_enum = llm.NodeType.fromString(node_type);
    const result = try llm.extractExcerpt(allocator, src, start_line, node_type_enum, llm.DEFAULT_MAX_LINES);
    return @constCast(result);
}

// ---------------------------------------------------------------------------
// Guidance JSON helpers
// ---------------------------------------------------------------------------

/// Constructs a Zig stage metadata object using provided allocator and source data.
pub fn buildMetadataStage(
    allocator: std.mem.Allocator,
    json_path: []const u8,
    source: []const u8,
) !?types.Stage {
    var parsed = llm.parseJsonFile(allocator, json_path, 8 * 1024 * 1024) orelse return null;
    defer parsed.deinit();
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
                if (llm.isTestPath(item.string)) continue;
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
                // Extract skill name from path: "skills/foo/SKILL.md" → "foo".
                const skill_name = llm.skillNameFromRef(ref);
                if (skill_name.len == 0) continue;
                if (i > 0) try mw.writeAll(", ");
                try mw.writeAll(skill_name);
            }
            try mw.writeByte('\n');
        }
    }

    // capabilities: capability refs.
    if (root.get("capabilities")) |cv| {
        if (cv == .array and cv.array.items.len > 0) {
            try mw.writeAll("capabilities: ");
            for (cv.array.items[0..@min(4, cv.array.items.len)], 0..) |item, i| {
                const cap_name: []const u8 = switch (item) {
                    .string => |s| s,
                    else => "",
                };
                if (cap_name.len == 0) continue;
                if (i > 0) try mw.writeAll(", ");
                try mw.writeAll(cap_name);
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

/// Loads a JSON module comment from a file path into a Zig array of bytes.
fn loadModuleComment(allocator: std.mem.Allocator, json_path: []const u8) ?[]const u8 {
    var parsed = llm.parseJsonFile(allocator, json_path, 8 * 1024 * 1024) orelse return null;
    defer parsed.deinit();
    const cv = parsed.value.object.get("comment") orelse return null;
    if (cv != .string) return null;
    if (cv.string.len < 10) return null;
    return allocator.dupe(u8, cv.string) catch null;
}

/// Loads skill names from a JSON file into a Zig array of byte slices.
pub fn loadSkillNamesFromJson(
    allocator: std.mem.Allocator,
    json_path: []const u8,
) ![][]const u8 {
    var parsed = llm.parseJsonFile(allocator, json_path, 8 * 1024 * 1024) orelse return &.{};
    defer parsed.deinit();

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
        const skill_name = llm.skillNameFromRef(ref);
        if (skill_name.len == 0) continue;
        try out.append(allocator, try allocator.dupe(u8, skill_name));
    }

    return out.toOwnedSlice(allocator);
}

/// Converts a C string into a Zig-safe slice, handling memory allocation and parsing.
pub fn parseSkillDocContent(allocator: std.mem.Allocator, content: []const u8) !?[]const u8 {
    return doc_parser.parseSkillDocContent(allocator, content);
}

/// Loads a skill excerpt from a directory into a Zig array slice.
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

    return parseSkillDocContent(allocator, content);
}

// ---------------------------------------------------------------------------
// Utility helpers
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test "formatStaged: empty stages output contains header" {
    const allocator = std.testing.allocator;
    const result = try formatStaged(allocator, "myquery", &.{}, null, "/workspace", "", null);
    defer allocator.free(result);
    try std.testing.expect(std.mem.indexOf(u8, result, "# Explain: myquery") != null);
}

test "formatStaged: code stage with line emits source path and line number" {
    const allocator = std.testing.allocator;
    const stages = [_]types.Stage{.{
        .kind = .code,
        .content = "pub fn foo() void {}\n",
        .source = "src/foo.zig",
        .line = 10,
    }};
    const result = try formatStaged(allocator, "q", &stages, null, "/workspace", null);
    defer allocator.free(result);
    try std.testing.expect(std.mem.indexOf(u8, result, "src/foo.zig:10") != null);
    try std.testing.expect(std.mem.indexOf(u8, result, "pub fn foo()") != null);
}

test "formatStaged: code stage without line still emits code block" {
    const allocator = std.testing.allocator;
    const stages = [_]types.Stage{.{
        .kind = .code,
        .content = "const x = 1;\n",
        .source = "src/bar.zig",
        .line = null,
    }};
    const result = try formatStaged(allocator, "q", &stages, null, "/workspace", null);
    defer allocator.free(result);
    try std.testing.expect(std.mem.indexOf(u8, result, "src/bar.zig") != null);
    try std.testing.expect(std.mem.indexOf(u8, result, "const x = 1;") != null);
}

test "formatStaged: summary appears before code sections" {
    const allocator = std.testing.allocator;
    const stages = [_]types.Stage{.{
        .kind = .code,
        .content = "pub fn foo() void {}\n",
        .source = "src/foo.zig",
        .line = 1,
    }};
    const result = try formatStaged(allocator, "q", &stages, "This is the summary.", "/workspace", null);
    defer allocator.free(result);
    const sum_pos = std.mem.indexOf(u8, result, "This is the summary.");
    const src_pos = std.mem.indexOf(u8, result, "## Source location:");
    try std.testing.expect(sum_pos != null);
    try std.testing.expect(src_pos != null);
    try std.testing.expect(sum_pos.? < src_pos.?);
}

test "formatStaged: followup keywords produce Suggested Queries section" {
    const allocator = std.testing.allocator;
    const kws = [_][]const u8{ "alpha search", "beta filter" };
    const result = try formatStaged(allocator, "q", &.{}, null, "/workspace", &kws);
    defer allocator.free(result);
    try std.testing.expect(std.mem.indexOf(u8, result, "## Suggested Queries") != null);
    try std.testing.expect(std.mem.indexOf(u8, result, "alpha search") != null);
    try std.testing.expect(std.mem.indexOf(u8, result, "beta filter") != null);
}

test "formatStaged: skill_doc stage produces Knowledge Base section" {
    const allocator = std.testing.allocator;
    const stages = [_]types.Stage{.{
        .kind = .skill_doc,
        .content = "This skill teaches you X.",
        .source = "zig-current",
        .line = null,
    }};
    const result = try formatStaged(allocator, "q", &stages, null, "/workspace", null);
    defer allocator.free(result);
    try std.testing.expect(std.mem.indexOf(u8, result, "## Knowledge Base") != null);
    try std.testing.expect(std.mem.indexOf(u8, result, "zig-current") != null);
}

test "formatStaged: metadata stage with keywords prefix produces References section" {
    const allocator = std.testing.allocator;
    const stages = [_]types.Stage{.{
        .kind = .metadata,
        .content = "keywords: vtable, allocator, arena",
        .source = "src/types.zig",
        .line = null,
    }};
    const result = try formatStaged(allocator, "q", &stages, null, "/workspace", null);
    defer allocator.free(result);
    try std.testing.expect(std.mem.indexOf(u8, result, "## References") != null);
    try std.testing.expect(std.mem.indexOf(u8, result, "vtable") != null);
}

test "parseSkillDocContent: YAML front matter with description returns description" {
    const allocator = std.testing.allocator;
    const content =
        \\---
        \\title: My Skill
        \\description: Teaches you how to use vtables.
        \\---
        \\
        \\Body paragraph here.
    ;
    const result = try parseSkillDocContent(allocator, content);
    defer if (result) |r| allocator.free(r);
    try std.testing.expect(result != null);
    try std.testing.expectEqualStrings("Teaches you how to use vtables.", result.?);
}

test "parseSkillDocContent: YAML front matter without description returns first non-empty body line" {
    const allocator = std.testing.allocator;
    const content =
        \\---
        \\title: My Skill
        \\author: someone
        \\---
        \\
        \\First real paragraph.
    ;
    const result = try parseSkillDocContent(allocator, content);
    defer if (result) |r| allocator.free(r);
    try std.testing.expect(result != null);
    try std.testing.expectEqualStrings("First real paragraph.", result.?);
}

test "parseSkillDocContent: no front matter returns first paragraph up to blank line" {
    const allocator = std.testing.allocator;
    const content = "First paragraph text.\n\nSecond paragraph here.";
    const result = try parseSkillDocContent(allocator, content);
    defer if (result) |r| allocator.free(r);
    try std.testing.expect(result != null);
    try std.testing.expectEqualStrings("First paragraph text.", result.?);
}

test "formatStaged: capability_doc stage renders Capability section" {
    const allocator = std.testing.allocator;
    const stages = [_]types.Stage{.{
        .kind = .capability_doc,
        .content = "Pluggable embedding system.\n\n**Anchors**: EmbeddingProvider\n**Sources**: src/common/embeddings.zig (1.0)\n",
        .source = "embedding-providers",
        .line = null,
    }};
    const result = try formatStaged(allocator, "embed", &stages, null, "/workspace", null);
    defer allocator.free(result);
    try std.testing.expect(std.mem.indexOf(u8, result, "## Capability: embedding-providers") != null);
    try std.testing.expect(std.mem.indexOf(u8, result, "EmbeddingProvider") != null);
}

test "formatStaged: See Also is capped at 10 unique keywords" {
    const allocator = std.testing.allocator;
    // Build metadata stage with 15 keywords
    const stages = [_]types.Stage{.{
        .kind = .metadata,
        .content = "keywords: a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, a13, a14, a15",
        .source = "src/foo.zig",
        .line = null,
    }};
    const result = try formatStaged(allocator, "q", &stages, null, "/workspace", null);
    defer allocator.free(result);
    // Should contain See Also but only up to 10 items
    const see_also_start = std.mem.indexOf(u8, result, "See also") orelse {
        try std.testing.expect(false); // must have See Also section
        return;
    };
    const see_also_line_end = std.mem.indexOfScalar(u8, result[see_also_start..], '\n') orelse result.len - see_also_start;
    const see_also_line = result[see_also_start .. see_also_start + see_also_line_end];
    // Count commas in the line — 9 commas = 10 items (max)
    var comma_count: usize = 0;
    for (see_also_line) |ch| if (ch == ',') {
        comma_count += 1;
    };
    try std.testing.expect(comma_count <= 9);
}
