//! synthesize.zig — LLM-based synthesis for the staged explain pipeline.
//!
//! Combines prose stages and module detail into an answer proportionate to query detail.
//! Uses fast model with cached detail context.

const std = @import("std");
const llm = @import("llm");
const common = @import("common");
const types = @import("../types.zig");

/// Result of synthesis: summary text and suggested follow-up keywords.
pub const SynthesisResult = struct {
    summary: ?[]const u8,
    followup_keywords: ?[][]const u8,
};

/// Checks if a query matches a direct lookup in the Zig data structure, returning true or false.
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

/// Transforms a Zig slice into a synthesized Zig result using an allocator and stage pipeline.
pub fn synthesize(
    allocator: std.mem.Allocator,
    client: *llm.LlmClient,
    query: []const u8,
    stages: []const types.Stage,
) !SynthesisResult {
    const is_direct = isDirectLookup(query);

    // Count words in query to determine synthesis depth
    var word_count: usize = 0;
    var word_it = std.mem.tokenizeAny(u8, query, " \t\n\r");
    while (word_it.next()) |_| word_count += 1;
    const is_long_query = word_count >= 5;

    // M8: Collect source file paths from stages for grounding
    var sources: std.ArrayList([]const u8) = .empty;
    defer sources.deinit(allocator);
    {
        var seen_src: std.StringHashMapUnmanaged(void) = .empty;
        defer seen_src.deinit(allocator);
        for (stages) |s| {
            if (!seen_src.contains(s.source)) {
                try seen_src.put(allocator, s.source, {});
                try sources.append(allocator, s.source);
            }
        }
    }

    // Collect detail content (from module documentation)
    var detail_buf_aw: std.Io.Writer.Allocating = .init(allocator);
    errdefer detail_buf_aw.deinit();
    const dw = &detail_buf_aw.writer;

    // Collect prose content (from comments)
    var prose_buf_aw: std.Io.Writer.Allocating = .init(allocator);
    errdefer prose_buf_aw.deinit();
    const pw = &prose_buf_aw.writer;

    // Collect code snippets
    var code_buf_aw: std.Io.Writer.Allocating = .init(allocator);
    errdefer code_buf_aw.deinit();
    const cw = &code_buf_aw.writer;

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

    if (detail_buf_aw.written().len == 0 and prose_buf_aw.written().len == 0) {
        // For long queries, still attempt synthesis with just the query text
        // and code snippets (if any). Don't bail out early.
        if (!is_long_query and code_buf_aw.written().len == 0) {
            return .{ .summary = null, .followup_keywords = null };
        }
    }

    var full_prompt_aw: std.Io.Writer.Allocating = .init(allocator);
    errdefer full_prompt_aw.deinit();

    if (is_direct) {
        // ── Brief summary for direct lookup ─────────────────────────────────────
        try full_prompt_aw.writer.print(
            \\You are a code documentation assistant. Write a brief summary.
            \\
            \\Query: {s}
            \\
        , .{query});

        if (prose_buf_aw.written().len > 0) {
            try full_prompt_aw.writer.print("\nComment:\n{s}\n", .{prose_buf_aw.written()});
        }

        if (code_buf_aw.written().len > 0) {
            try full_prompt_aw.writer.print("\nCode:\n{s}\n", .{code_buf_aw.written()});
        }

        try full_prompt_aw.writer.writeAll(
            \\
            \\Write a ONE-SENTENCE summary (under 400 characters) describing what this is.
            \\Do NOT use headers or sections. Just one clear sentence.
            \\After your summary, suggest 2 related keywords.
            \\Format: KEYWORDS: keyword1, keyword2
            \\
        );

        // Use lower max_tokens for brief output
        const raw_opt = client.complete(full_prompt_aw.written(), 300, 0.2, null) catch return .{ .summary = null, .followup_keywords = null };
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

            var kw_list: std.ArrayList([]const u8) = .empty;
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

    // ── Comprehensive answer for context query ───────────────────────────────
    // is_long_query already computed at function start

    // M8: Build source list for grounding (prevents hallucination)
    var sources_buf_aw: std.Io.Writer.Allocating = .init(allocator);
    errdefer sources_buf_aw.deinit();
    const sw = &sources_buf_aw.writer;
    try sw.writeAll("Source files referenced:\n");
    for (sources.items) |src| {
        try sw.print("- {s}\n", .{src});
    }

    if (is_long_query) {
        // Enhanced synthesis for long queries with structured sections + grounding
        try full_prompt_aw.writer.print(
            \\You are a technical documentation expert. Explain how the code works in detail.
            \\
            \\Query: {s}
            \\
            \\IMPORTANT: Base your answer ONLY on the source content provided below.
            \\Do NOT invent functions, structs, or components that are not explicitly listed.
            \\Use the actual names from the source files.
            \\
            \\{s}
            \\
            \\Provide a comprehensive answer with the following structure:
            \\
            \\## Overview
            \\1-2 sentences explaining what this is and its purpose.
            \\
            \\## Key Components
            \\- List 3-5 main components (use names from the source files above)
            \\
            \\## How It Works
            \\2-3 paragraphs explaining the flow, algorithms, and interactions.
            \\Include specific technical details from the source content.
            \\
            \\## Data Flow
            \\Brief description of how data moves through the system.
            \\
        , .{ query, sources_buf_aw.written() });
    } else {
        // Brief synthesis for short queries + grounding
        try full_prompt_aw.writer.print(
            \\You are a code documentation assistant. Write a concise technical answer.
            \\
            \\Query: {s}
            \\
            \\IMPORTANT: Base your answer ONLY on the source content provided below.
            \\Do NOT invent components not listed.
            \\
            \\{s}
            \\
        , .{ query, sources_buf_aw.written() });
    }

    if (detail_buf_aw.written().len > 0) {
        try full_prompt_aw.writer.print("\n## Module Documentation\n\n{s}\n", .{detail_buf_aw.written()});
    }

    if (prose_buf_aw.written().len > 0) {
        try full_prompt_aw.writer.print("\n## Code Comments\n\n{s}\n", .{prose_buf_aw.written()});
    }

    if (code_buf_aw.written().len > 0) {
        try full_prompt_aw.writer.print("\n## Source Code\n\n{s}\n", .{code_buf_aw.written()});
    }

    if (is_long_query) {
        try full_prompt_aw.writer.writeAll(
            \\
            \\Be thorough and technical. Use specific terms from the source code above.
            \\After your answer, suggest 2-3 related search queries.
            \\Format: KEYWORDS: keyword1, keyword2, keyword3
            \\
        );
    } else {
        try full_prompt_aw.writer.writeAll(
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
    }

    // Use fast model with moderate max_tokens for concise output
    const raw_opt = client.complete(full_prompt_aw.written(), 600, 0.3, null) catch return .{ .summary = null, .followup_keywords = null };
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

        var kw_list: std.ArrayList([]const u8) = .empty;
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

/// Extracts prose source slices from a Zig allocation context, returning a flat array of byte slices.
pub fn extractProseSources(
    allocator: std.mem.Allocator,
    stages: []const types.Stage,
) ![]const []const u8 {
    var out: std.ArrayList([]const u8) = .empty;
    errdefer out.deinit(allocator);
    for (stages) |s| {
        if (s.kind == .prose or s.kind == .insight) {
            try out.append(allocator, s.content);
        }
    }
    return out.toOwnedSlice(allocator);
}

/// Removes empty or absent sentence segments from the input text using an allocator.
pub fn stripAbsenceSentences(allocator: std.mem.Allocator, text: []const u8) ![]u8 {
    // All comparisons are case-insensitive via common.containsAny.
    const absence_kws = [_][]const u8{
        "no other",       "not present",  "only has", "does not contain",
        "does not exist", "nothing else", "none are", "none were",
    };

    var buf: std.ArrayList(u8) = .empty;
    errdefer buf.deinit(allocator);

    var line_it = std.mem.splitScalar(u8, text, '\n');
    while (line_it.next()) |line| {
        if (!common.containsAny(line, &absence_kws)) {
            try buf.appendSlice(allocator, line);
            try buf.append(allocator, '\n');
        }
    }
    return buf.toOwnedSlice(allocator);
}
