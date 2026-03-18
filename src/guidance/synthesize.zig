//! synthesize.zig — LLM-based synthesis for the staged explain pipeline.
//!
//! Combines prose stages into a concise 2-4 sentence summary
//! directly answering the user's query.

const std = @import("std");
const llm = @import("common");
const types = @import("types.zig");

/// Synthesize a concise answer from prose stages.
///
/// Returns an owned string or null on LLM failure / empty input.
/// Caller must free the returned string.
pub fn synthesize(
    allocator: std.mem.Allocator,
    client: *llm.LlmClient,
    query: []const u8,
    stages: []const types.Stage,
) !?[]const u8 {
    // Collect prose content, capped to avoid token burn.
    var combined: std.ArrayList(u8) = .{};
    defer combined.deinit(allocator);
    const cw = combined.writer(allocator);

    var section_count: usize = 0;
    for (stages) |s| {
        if (s.kind != .prose and s.kind != .insight) continue;
        if (combined.items.len > 3000) break; // cap at ~600 tokens
        if (section_count > 0) try cw.writeAll("\n\n---\n\n");
        try cw.writeAll(std.mem.trim(u8, s.content, " \t\n\r"));
        section_count += 1;
    }

    if (combined.items.len == 0) return null;

    const prompt = try std.fmt.allocPrint(
        allocator,
        "You are a code navigation assistant. Be precise and concise.\n" ++
            "Synthesize a clear answer to the query \"{s}\" from these explanations.\n" ++
            "Focus only on information directly relevant to the query.\n" ++
            "Omit unrelated details. Use only facts from EXPLANATIONS.\n" ++
            "STRICT RULE: Never write sentences about absence.\n\n" ++
            "EXPLANATIONS:\n{s}\n\n" ++
            "Provide your answer in 2-4 sentences. Return only the answer.",
        .{ query, combined.items },
    );
    defer allocator.free(prompt);

    const raw_opt = client.complete(prompt, 300, 0.1, null) catch return null;
    const raw = raw_opt orelse return null;
    defer allocator.free(raw);

    const stripped = llm.stripThinkBlock(raw);
    const cleaned = try stripAbsenceSentences(allocator, stripped);
    defer allocator.free(cleaned);

    const trimmed = std.mem.trim(u8, cleaned, " \t\n\r");
    if (trimmed.len == 0) return null;

    return try allocator.dupe(u8, trimmed);
}

/// Extract all prose content from stages as a slice of string views.
/// The returned strings are views into the stage content (not duped).
/// Returns an owned slice of slices (the outer slice must be freed, not the inner strings).
pub fn extractProseSources(
    allocator: std.mem.Allocator,
    stages: []const types.Stage,
) ![]const []const u8 {
    var out: std.ArrayList([]const u8) = .{};
    errdefer out.deinit(allocator);
    for (stages) |s| {
        if (s.kind == .prose or s.kind == .insight) {
            try out.append(allocator, s.content);
        }
    }
    return out.toOwnedSlice(allocator);
}

/// Remove lines containing "absence" phrases from LLM output.
/// Returns an owned copy with those lines stripped.
pub fn stripAbsenceSentences(allocator: std.mem.Allocator, text: []const u8) ![]u8 {
    const absence_kws = [_][]const u8{
        "no other",       "not present",  "only has", "does not contain",
        "does not exist", "nothing else", "none are", "none were",
    };

    var buf: std.ArrayList(u8) = .{};
    errdefer buf.deinit(allocator);

    var line_it = std.mem.splitScalar(u8, text, '\n');
    while (line_it.next()) |line| {
        const lower = try std.ascii.allocLowerString(allocator, line);
        defer allocator.free(lower);
        var is_absence = false;
        for (absence_kws) |kw| {
            if (std.mem.indexOf(u8, lower, kw) != null) {
                is_absence = true;
                break;
            }
        }
        if (!is_absence) {
            try buf.appendSlice(allocator, line);
            try buf.append(allocator, '\n');
        }
    }
    return buf.toOwnedSlice(allocator);
}
