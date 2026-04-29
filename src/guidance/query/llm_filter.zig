//! llm_filter.zig — LLM-based relevance filtering for the staged explain pipeline.
//!
//! For each prose, insight, or skill_doc Stage, asks a local LLM:
//!   "Is this content relevant to the query? Answer YES or NO."
//!
//! Code and metadata stages are always kept verbatim.
//! On LLM error or unavailability, returns all input unchanged.

const std = @import("std");
const llm = @import("llm");
const types = @import("../types.zig");

/// Processes a query by filtering stages using an allocator and returns a cleaned stage list.
pub fn filterStages(
    allocator: std.mem.Allocator,
    client: *llm.LlmClient,
    query: []const u8,
    stages: []const types.Stage,
) ![]types.Stage {
    var out: std.ArrayList(types.Stage) = .empty;
    errdefer {
        types.freeStages(allocator, out.items);
        out.deinit(allocator);
    }

    for (stages) |s| {
        // Code and metadata stages are always kept.
        if (s.kind == .code or s.kind == .metadata) {
            try out.append(allocator, try dupeStage(allocator, s));
            continue;
        }

        // For prose/insight/skill_doc: ask LLM for relevance.
        const keep = askRelevant(allocator, client, query, s.content) catch true; // fail-open
        if (keep) {
            try out.append(allocator, try dupeStage(allocator, s));
        }
    }

    return out.toOwnedSlice(allocator);
}

/// Entry for See Also filtering: file path + keywords/signature
pub const SeeAlsoEntry = struct {
    path: []const u8,
    keywords: []const u8,
};

/// Filters and returns a list of matching SeeAlso entries based on the provided query and client data.
pub fn filterSeeAlso(
    allocator: std.mem.Allocator,
    client: *llm.LlmClient,
    query: []const u8,
    entries: []const SeeAlsoEntry,
    max_keep: usize,
) ![][]const u8 {
    if (entries.len <= max_keep) {
        var result = try allocator.alloc([]const u8, entries.len);
        for (entries, 0..) |e, i| {
            result[i] = try allocator.dupe(u8, e.path);
        }
        return result;
    }

    // Build context string: "path: keyword1, keyword2" for each entry
    var context_buf_aw: std.Io.Writer.Allocating = .init(allocator);
    defer context_buf_aw.deinit();
    const cw = &context_buf_aw.writer;

    for (entries, 0..) |e, i| {
        if (i > 0) try cw.writeAll("\n");
        try cw.print("- {s}: {s}", .{ e.path, e.keywords });
    }

    const prompt = try std.fmt.allocPrint(
        allocator,
        \\You are a code search relevance filter.
        \\Query: "{s}"
        \\Files found:
        \\{s}
        \\
        \\Which {d} files are MOST relevant to the query?
        \\Return ONLY the file paths, one per line, in order of relevance.
        \\Do not include explanations. If fewer than {d} are relevant, return fewer.
    ,
        .{ query, context_buf_aw.written(), max_keep, max_keep },
    );
    defer allocator.free(prompt);

    const response_opt = client.complete(prompt, 200, 0.1, null) catch return try keepAll(allocator, entries);
    const response = response_opt orelse return try keepAll(allocator, entries);
    defer allocator.free(response);

    const stripped = llm.stripThinkBlock(response);
    var lines = std.mem.splitScalar(u8, stripped, '\n');

    var kept: std.ArrayList([]const u8) = .empty;
    errdefer {
        for (kept.items) |p| allocator.free(p);
        kept.deinit(allocator);
    }

    var seen: std.StringHashMapUnmanaged(void) = .empty;
    defer seen.deinit(allocator);

    // Parse response lines and match against entries
    var entry_map: std.StringHashMapUnmanaged(usize) = .empty;
    defer entry_map.deinit(allocator);
    for (entries, 0..) |e, i| {
        try entry_map.put(allocator, e.path, i);
    }

    while (lines.next()) |line| {
        const trimmed = std.mem.trim(u8, line, " \t\r");
        if (trimmed.len == 0) continue;
        if (seen.contains(trimmed)) continue;
        if (kept.items.len >= max_keep) break;

        // Check if this line matches any entry path
        if (entry_map.get(trimmed)) |_| {
            try seen.put(allocator, trimmed, {});
            try kept.append(allocator, try allocator.dupe(u8, trimmed));
        } else {
            // Try partial match: line might be a substring of path
            for (entries) |e| {
                if (std.mem.indexOf(u8, e.path, trimmed) != null and !seen.contains(e.path)) {
                    try seen.put(allocator, e.path, {});
                    try kept.append(allocator, try allocator.dupe(u8, e.path));
                    break;
                }
            }
        }
    }

    // If LLM didn't return enough, fill with remaining entries
    if (kept.items.len < max_keep) {
        for (entries) |e| {
            if (kept.items.len >= max_keep) break;
            if (seen.contains(e.path)) continue;
            try seen.put(allocator, e.path, {});
            try kept.append(allocator, try allocator.dupe(u8, e.path));
        }
    }

    return kept.toOwnedSlice(allocator);
}

/// Processes a list of entries and returns a cleaned slice of zig slices.
fn keepAll(allocator: std.mem.Allocator, entries: []const SeeAlsoEntry) ![][]const u8 {
    var result = try allocator.alloc([]const u8, entries.len);
    for (entries, 0..) |e, i| {
        result[i] = try allocator.dupe(u8, e.path);
    }
    return result;
}

/// Checks if a query matches relevant content using an allocator and client, returning a boolean result.
fn askRelevant(
    allocator: std.mem.Allocator,
    client: *llm.LlmClient,
    query: []const u8,
    content: []const u8,
) !bool {
    // Truncate content to avoid burning too many tokens.
    const content_trunc = content[0..@min(content.len, 400)];

    const prompt = try std.fmt.allocPrint(
        allocator,
        "Is the following content relevant to the query \"{s}\"?\nAnswer only YES or NO.\n\nContent:\n{s}",
        .{ query, content_trunc },
    );
    defer allocator.free(prompt);

    const response_opt = client.complete(prompt, 5, 0.0, null) catch return true; // fail-open
    const response = response_opt orelse return true;
    defer allocator.free(response);

    const stripped = llm.stripThinkBlock(response);
    const trimmed = std.mem.trim(u8, stripped, " \t\r\n");

    // Accept "YES", "yes", "Yes", etc.  Anything else → keep (fail-open).
    return !std.ascii.startsWithIgnoreCase(trimmed, "NO");
}

/// Transforms a stage into a duplicated version using an allocator.
fn dupeStage(allocator: std.mem.Allocator, s: types.Stage) !types.Stage {
    return types.Stage{
        .kind = s.kind,
        .content = try allocator.dupe(u8, s.content),
        .source = try allocator.dupe(u8, s.source),
        .line = s.line,
    };
}
