//! synthesize.zig — LLM-based synthesis for the staged explain pipeline.
//!
//! Combines prose stages into a concise 2-4 sentence summary
//! directly answering the user's query, and suggests follow-up keywords.

const std = @import("std");
const llm = @import("common");
const types = @import("types.zig");

/// Result of synthesis: summary text and suggested follow-up keywords.
pub const SynthesisResult = struct {
    summary: ?[]const u8,
    followup_keywords: ?[][]const u8,
};

/// Synthesize a concise answer from prose stages and suggest follow-up keywords.
///
/// Returns a SynthesisResult with owned strings; caller must free summary and followup_keywords.
pub fn synthesize(
    allocator: std.mem.Allocator,
    client: *llm.LlmClient,
    query: []const u8,
    stages: []const types.Stage,
) !SynthesisResult {
    // Collect prose content, capped to avoid token burn.
    var combined: std.ArrayList(u8) = .{};
    defer combined.deinit(allocator);
    const cw = combined.writer(allocator);

    // Also collect available keywords from metadata stages for LLM context.
    var keywords_buf: std.ArrayList(u8) = .{};
    defer keywords_buf.deinit(allocator);
    const kw_writer = keywords_buf.writer(allocator);

    var section_count: usize = 0;
    for (stages) |s| {
        if (s.kind == .prose or s.kind == .insight) {
            if (combined.items.len > 3000) break; // cap at ~600 tokens
            if (section_count > 0) try cw.writeAll("\n\n---\n\n");
            try cw.writeAll(std.mem.trim(u8, s.content, " \t\n\r"));
            section_count += 1;
        } else if (s.kind == .metadata) {
            // Extract keywords from metadata for LLM context
            var lines = std.mem.splitScalar(u8, s.content, '\n');
            while (lines.next()) |line| {
                if (std.mem.startsWith(u8, line, "keywords: ")) {
                    if (keywords_buf.items.len > 0) try kw_writer.writeAll(", ");
                    try kw_writer.writeAll(line["keywords: ".len..]);
                }
            }
        }
    }

    // Collect source file names for LLM context
    var sources_buf: std.ArrayList(u8) = .{};
    defer sources_buf.deinit(allocator);
    const src_writer = sources_buf.writer(allocator);

    var seen_sources: std.StringHashMapUnmanaged(void) = .{};
    defer seen_sources.deinit(allocator);

    for (stages) |s| {
        if (s.kind == .code or s.kind == .prose) {
            if (seen_sources.contains(s.source)) continue;
            try seen_sources.put(allocator, s.source, {});
            if (sources_buf.items.len > 0) try src_writer.writeAll(", ");
            try src_writer.writeAll(s.source);
        }
    }

    if (combined.items.len == 0) return .{ .summary = null, .followup_keywords = null };

    // Build prompt for detailed synthesis (under 600 words)
    const prompt = try std.fmt.allocPrint(
        allocator,
        "You are a code navigation assistant providing comprehensive technical summaries.\n" ++
            "Answer the query \"{s}\" using the explanations below.\n\n" ++
            "Guidelines:\n" ++
            "- Be thorough and technically precise\n" ++
            "- Describe key abstractions, data structures, and their relationships\n" ++
            "- Explain how components interact and flow\n" ++
            "- Include concrete details: function names, types, patterns\n" ++
            "- Use only facts from EXPLANATIONS\n" ++
            "- Never mention absence or say 'no information found'\n\n" ++
            "AVAILABLE KEYWORDS: {s}\n" ++
            "SOURCE FILES: {s}\n\n" ++
            "EXPLANATIONS:\n{s}\n\n" ++
            "Write a detailed summary (under 600 words). After your summary, suggest 3-5 related keywords.\n" ++
            "Format the last line as: KEYWORDS: keyword1, keyword2, keyword3",
        .{ query, keywords_buf.items, sources_buf.items, combined.items },
    );
    defer allocator.free(prompt);

    const raw_opt = client.complete(prompt, 1000, 0.2, null) catch return .{ .summary = null, .followup_keywords = null };
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
            if (count >= 4) break;
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
