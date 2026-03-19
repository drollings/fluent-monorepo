//! synthesize.zig — LLM-based synthesis for the staged explain pipeline.
//!
//! Combines prose stages and module detail into an answer proportionate to query detail.
//! Uses fast model with cached detail context.

const std = @import("std");
const llm = @import("common");
const types = @import("types.zig");

/// Result of synthesis: summary text and suggested follow-up keywords.
pub const SynthesisResult = struct {
    summary: ?[]const u8,
    followup_keywords: ?[][]const u8,
};

/// Check if query is a direct lookup (single identifier token)
fn isDirectLookup(query: []const u8) bool {
    const trimmed = std.mem.trim(u8, query, " \t\n\r");
    if (trimmed.len == 0) return false;

    // Must be single token
    var tok = std.mem.tokenizeAny(u8, trimmed, " \t\n\r");
    var count: usize = 0;
    while (tok.next()) |_| {
        count += 1;
        if (count > 1) return false;
    }
    if (count != 1) return false;

    // Must look like identifier
    if (!std.ascii.isAlphabetic(trimmed[0]) and trimmed[0] != '_') return false;
    for (trimmed) |c| {
        if (!std.ascii.isAlphanumeric(c) and c != '_') return false;
    }

    return true;
}

/// Synthesize an answer proportionate to query detail.
/// - Direct lookup (single identifier): brief summary under 400 chars
/// - Context query: comprehensive answer with sections
///
/// Returns a SynthesisResult with owned strings; caller must free summary and followup_keywords.
pub fn synthesize(
    allocator: std.mem.Allocator,
    client: *llm.LlmClient,
    query: []const u8,
    stages: []const types.Stage,
) !SynthesisResult {
    const is_direct = isDirectLookup(query);

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

    // For direct lookups, limit context more aggressively
    // For context queries, still keep it concise to get concise answers
    const max_detail: usize = if (is_direct) 1 else 1;
    const max_prose: usize = if (is_direct) 2 else 3;
    const max_code: usize = if (is_direct) 1 else 2;
    const detail_char_limit: usize = if (is_direct) 500 else 800;

    for (stages) |s| {
        if (s.kind == .prose) {
            // Check if this looks like module detail (longer content)
            if (s.content.len > 200 and detail_count < max_detail) {
                if (detail_count > 0) try dw.writeAll("\n\n---\n\n");
                try dw.writeAll(s.content[0..@min(detail_char_limit, s.content.len)]);
                detail_count += 1;
            } else if (prose_count < max_prose) {
                if (prose_count > 0) try pw.writeAll("\n\n");
                try pw.writeAll(s.content);
                prose_count += 1;
            }
        } else if (s.kind == .code and code_count < max_code) {
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

    var full_prompt: std.ArrayList(u8) = .{};
    defer full_prompt.deinit(allocator);
    const fw = full_prompt.writer(allocator);

    if (is_direct) {
        // ── Brief summary for direct lookup ─────────────────────────────────────
        try fw.print(
            \\You are a code documentation assistant. Write a brief summary.
            \\
            \\Query: {s}
            \\
        , .{query});

        if (prose_buf.items.len > 0) {
            try fw.print("\nComment:\n{s}\n", .{prose_buf.items});
        }

        if (code_buf.items.len > 0) {
            try fw.print("\nCode:\n{s}\n", .{code_buf.items});
        }

        try fw.writeAll(
            \\
            \\Write a ONE-SENTENCE summary (under 400 characters) describing what this is.
            \\Do NOT use headers or sections. Just one clear sentence.
            \\After your summary, suggest 2 related keywords.
            \\Format: KEYWORDS: keyword1, keyword2
            \\
        );

        // Use lower max_tokens for brief output
        const raw_opt = client.complete(full_prompt.items, 300, 0.2, null) catch return .{ .summary = null, .followup_keywords = null };
        const raw = raw_opt orelse return .{ .summary = null, .followup_keywords = null };
        defer allocator.free(raw);

        const stripped = llm.stripThinkBlock(raw);
        const cleaned = try stripAbsenceSentences(allocator, stripped);
        defer allocator.free(cleaned);

        const trimmed = std.mem.trim(u8, cleaned, "\t\n\r");
        if (trimmed.len == 0) return .{ .summary = null, .followup_keywords = null };

        // Extract summary and keywords
        var summary_part: []const u8 = trimmed;
        var followup: ?[][]const u8 = null;

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
                if (count >= 2) break;
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

    // ── Comprehensive answer for context query ─────────────────────────────────
    try fw.print(
        \\You are a code documentation assistant. Write a concise technical answer.
        \\
        \\Query: {s}
        \\
    , .{query});

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
        \\Write a concise answer (200-400 words). Cover:
        \\- What it does (1-2 sentences)
        \\- Key components (bullet list, max 4 items)
        \\- How it works (1-2 paragraphs)
        \\
        \\Be precise and technical. No fluff. Use bullets, not prose paragraphs.
        \\After your answer, suggest 2-3 related keywords.
        \\Format: KEYWORDS: keyword1, keyword2, keyword3
        \\
    );

    // Use fast model with moderate max_tokens for concise output
    const raw_opt = client.complete(full_prompt.items, 600, 0.3, null) catch return .{ .summary = null, .followup_keywords = null };
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
