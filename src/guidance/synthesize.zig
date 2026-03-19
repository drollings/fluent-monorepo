//! synthesize.zig — LLM-based synthesis for the staged explain pipeline.
//!
//! Combines prose stages and module detail into a comprehensive answer
//! directly answering the user's query. Uses fast model with cached detail context.

const std = @import("std");
const llm = @import("common");
const types = @import("types.zig");

/// Result of synthesis: summary text and suggested follow-up keywords.
pub const SynthesisResult = struct {
    summary: ?[]const u8,
    followup_keywords: ?[][]const u8,
};

/// Synthesize a comprehensive answer from prose stages and module detail.
/// Uses fast model with cached detail context for efficiency.
///
/// Returns a SynthesisResult with owned strings; caller must free summary and followup_keywords.
pub fn synthesize(
    allocator: std.mem.Allocator,
    client: *llm.LlmClient,
    query: []const u8,
    stages: []const types.Stage,
) !SynthesisResult {
    // Collect detail content (from module documentation)
    var detail_buf: std.ArrayList(u8) = .{};
    defer detail_buf.deinit(allocator);
    const dw = detail_buf.writer(allocator);

    // Collect prose content (from comments)
    var prose_buf: std.ArrayList(u8) = .{};
    defer prose_buf.deinit(allocator);
    const pw = prose_buf.writer(allocator);

    // Collect code snippets
    var code_buf: std.ArrayList(u8) = .{};
    defer code_buf.deinit(allocator);
    const cw = code_buf.writer(allocator);

    var detail_count: usize = 0;
    var prose_count: usize = 0;
    var code_count: usize = 0;

    for (stages) |s| {
        if (s.kind == .prose) {
            // Check if this looks like module detail (longer content)
            if (s.content.len > 200 and detail_count < 3) {
                if (detail_count > 0) try dw.writeAll("\n\n---\n\n");
                try dw.writeAll(s.content[0..@min(2000, s.content.len)]);
                detail_count += 1;
            } else if (prose_count < 5) {
                if (prose_count > 0) try pw.writeAll("\n\n");
                try pw.writeAll(s.content);
                prose_count += 1;
            }
        } else if (s.kind == .code and code_count < 4) {
            if (code_count > 0) try cw.writeAll("\n\n");
            try cw.writeAll("```\n");
            try cw.writeAll(s.content);
            try cw.writeAll("\n```\n");
            code_count += 1;
        }
    }

    if (detail_buf.items.len == 0 and prose_buf.items.len == 0) {
        return .{ .summary = null, .followup_keywords = null };
    }

    // Build comprehensive prompt for fast model
    const prompt = try std.fmt.allocPrint(
        allocator,
        \\You are a code documentation assistant. Write a comprehensive technical answer.
        \\
        \\Query: {s}
        \\
    ,
        .{query},
    );
    defer allocator.free(prompt);

    var full_prompt: std.ArrayList(u8) = .{};
    defer full_prompt.deinit(allocator);
    const fw = full_prompt.writer(allocator);

    try fw.writeAll(prompt);

    if (detail_buf.items.len > 0) {
        try fw.print("\n## Module Documentation\n\n{s}\n", .{detail_buf.items});
    }

    if (prose_buf.items.len > 0) {
        try fw.print("\n## Code Comments\n\n{s}\n", .{prose_buf.items});
    }

    if (code_buf.items.len > 0) {
        try fw.print("\n## Source Code\n\n{s}\n", .{code_buf.items});
    }

    try fw.writeAll(
        \\
        \\Write a comprehensive answer with clear sections. Include:
        \\- Overview of the architecture/approach
        \\- Key components and their roles
        \\- Implementation details from the code
        \\- How the pieces fit together
        \\
        \\Be technically precise. Use markdown headers (##) for sections.
        \\After your answer, suggest 2-3 related keywords for further exploration.
        \\Format: KEYWORDS: keyword1, keyword2, keyword3
        \\
    );

    // Use fast model with higher max_tokens for comprehensive output
    const raw_opt = client.complete(full_prompt.items, 2000, 0.3, null) catch return .{ .summary = null, .followup_keywords = null };
    const raw = raw_opt orelse return .{ .summary = null, .followup_keywords = null };
    defer allocator.free(raw);

    const stripped = llm.stripThinkBlock(raw);
    const cleaned = try stripAbsenceSentences(allocator, stripped);
    defer allocator.free(cleaned);

    const trimmed = std.mem.trim(u8, cleaned, "\t\n\r");
    if (trimmed.len == 0) return .{ .summary = null, .followup_keywords = null };

    // Extract summary and keywords from response
    var summary_part: []const u8 = trimmed;
    var followup: ?[][]const u8 = null;

    // Look for KEYWORDS: marker
    if (std.mem.indexOf(u8, trimmed, "KEYWORDS:")) |kw_pos| {
        summary_part = std.mem.trim(u8, trimmed[0..kw_pos], "\t\n\r");
        const kw_text = trimmed[kw_pos + "KEYWORDS:".len ..];
        const kw_trimmed = std.mem.trim(u8, kw_text, " \t\n\r");

        var kw_list: std.ArrayList([]const u8) = .{};
        errdefer {
            for (kw_list.items) |k| allocator.free(k);
            kw_list.deinit(allocator);
        }

        var it = std.mem.splitAny(u8, kw_trimmed, ",\n");
        var count: usize = 0;
        while (it.next()) |kw| {
            if (count >= 3) break;
            const k = std.mem.trim(u8, kw, " \t\r");
            if (k.len > 0 and k.len < 50) {
                try kw_list.append(allocator, try allocator.dupe(u8, k));
                count += 1;
            }
        }

        if (kw_list.items.len > 0) {
            followup = try kw_list.toOwnedSlice(allocator);
        } else {
            kw_list.deinit(allocator);
        }
    }

    return .{
        .summary = if (summary_part.len > 0) try allocator.dupe(u8, summary_part) else null,
        .followup_keywords = followup,
    };
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
