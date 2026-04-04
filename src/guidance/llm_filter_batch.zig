//! llm_filter_batch.zig — Batch LLM relevance filtering for the staged explain pipeline.
//!
//! Replaces the per-stage LLM call pattern in `llm_filter.zig` with a single
//! batch call that asks the LLM to identify relevant stages in one request.
//!
//! §Performance improvement:
//! - Old: N LLM calls (one per prose/insight/skill_doc stage)
//! - New: 1 LLM call (all stages sent in a single prompt)
//!
//! §Fallback behaviour:
//! - On LLM error or unavailability: return all stages unchanged (fail-open).
//! - On parse error: return all stages unchanged (fail-open).
//!
//! §Token budget:
//! - `preFilterByBudget()` deterministically drops stages that would exceed
//!   the budget before the LLM call, ensuring the prompt fits in context.
//! - Code and metadata stages are always kept, regardless of budget or LLM.

const std = @import("std");
const llm = @import("common");
const types = @import("types.zig");

const Allocator = std.mem.Allocator;

/// Approximate tokens for a string: 1 token ≈ 4 bytes.
/// Delegates to the shared common.token_budget.estimate().
pub fn estimateTokens(content: []const u8) usize {
    return llm.token_budget.estimate(content);
}

/// Deterministic pre-filter: drop prose/insight/skill_doc stages that would
/// push the total token count over `budget`.  Code and metadata stages are
/// always preserved and counted toward the budget.
///
/// Returns an allocator-owned slice of Stage pointers into the original `stages`
/// slice (no copying).  The result slice must be freed with `allocator.free()`.
pub fn preFilterByBudget(
    allocator: Allocator,
    stages: []const types.Stage,
    budget: usize,
) ![]const types.Stage {
    var kept: std.ArrayList(types.Stage) = .empty;
    errdefer {
        types.freeStages(allocator, kept.items);
        kept.deinit(allocator);
    }

    var tokens_used: usize = 0;

    // Pass 1: always include code and metadata, count tokens.
    for (stages) |s| {
        if (s.kind == .code or s.kind == .metadata) {
            tokens_used += estimateTokens(s.content);
            try kept.append(allocator, try dupeStage(allocator, s));
        }
    }

    // Pass 2: include prose/insight/skill_doc while budget allows.
    for (stages) |s| {
        if (s.kind == .code or s.kind == .metadata) continue;
        const t = estimateTokens(s.content);
        if (tokens_used + t > budget) continue; // Drop over-budget prose
        tokens_used += t;
        try kept.append(allocator, try dupeStage(allocator, s));
    }

    return kept.toOwnedSlice(allocator);
}

/// Batch-filter `stages` for relevance to `query` using a single LLM call.
///
/// Phase 1: deterministic token-budget pre-filter (always runs, no LLM).
/// Phase 2: single LLM call asking "which of these N excerpts are relevant?"
/// Phase 3: assemble result from returned indices.
///
/// Returns allocator-owned slice of Stages.  On any failure, falls back to
/// returning all pre-filtered stages (fail-open).
pub fn filterStagesBatch(
    allocator: Allocator,
    client: *llm.LlmClient,
    query: []const u8,
    stages: []const types.Stage,
    budget: usize,
) ![]types.Stage {
    if (stages.len == 0) return allocator.alloc(types.Stage, 0);

    // Phase 1: deterministic budget pre-filter.
    const pre_filtered = try preFilterByBudget(allocator, stages, budget);
    defer {
        types.freeStages(allocator, pre_filtered);
        allocator.free(pre_filtered);
    }

    if (pre_filtered.len == 0) return allocator.alloc(types.Stage, 0);

    // Code/metadata stages are always kept — split them from prose.
    var always_keep: std.ArrayList(types.Stage) = .empty;
    errdefer {
        types.freeStages(allocator, always_keep.items);
        always_keep.deinit(allocator);
    }
    var prose_stages: std.ArrayList(types.Stage) = .empty;
    errdefer {
        types.freeStages(allocator, prose_stages.items);
        prose_stages.deinit(allocator);
    }

    for (pre_filtered) |s| {
        if (s.kind == .code or s.kind == .metadata) {
            try always_keep.append(allocator, try dupeStage(allocator, s));
        } else {
            try prose_stages.append(allocator, try dupeStage(allocator, s));
        }
    }

    // If no prose to filter, return the always-keep stages directly.
    if (prose_stages.items.len == 0) {
        prose_stages.deinit(allocator);
        return always_keep.toOwnedSlice(allocator);
    }

    // Phase 2: single batch LLM call for prose relevance.
    const relevant_indices = askRelevantBatch(
        allocator,
        client,
        query,
        prose_stages.items,
    ) catch null;

    // Phase 3: assemble result.
    var result: std.ArrayList(types.Stage) = .empty;
    errdefer {
        types.freeStages(allocator, result.items);
        result.deinit(allocator);
    }

    // Always include code/metadata.
    for (always_keep.items) |s| {
        try result.append(allocator, try dupeStage(allocator, s));
    }

    if (relevant_indices) |indices| {
        defer allocator.free(indices);
        for (indices) |idx| {
            if (idx < prose_stages.items.len) {
                try result.append(allocator, try dupeStage(allocator, prose_stages.items[idx]));
            }
        }
    } else {
        // Fail-open: include all prose stages.
        for (prose_stages.items) |s| {
            try result.append(allocator, try dupeStage(allocator, s));
        }
    }

    // Cleanup temps.
    types.freeStages(allocator, always_keep.items);
    always_keep.deinit(allocator);
    types.freeStages(allocator, prose_stages.items);
    prose_stages.deinit(allocator);

    return result.toOwnedSlice(allocator);
}

/// Ask the LLM which of `stages` are relevant to `query`.
/// Returns an allocator-owned slice of 0-based indices into `stages`.
/// Returns an error on LLM failure (caller should fail-open).
fn askRelevantBatch(
    allocator: Allocator,
    client: *llm.LlmClient,
    query: []const u8,
    stages: []const types.Stage,
) ![]usize {
    // Build prompt listing all prose excerpts numbered 1..N.
    var prompt_buf: std.ArrayList(u8) = .empty;
    defer prompt_buf.deinit(allocator);
    const w = prompt_buf.writer(allocator);

    try w.print(
        "Query: \"{s}\"\n\n" ++
            "Which of the following excerpts are relevant to this query?\n" ++
            "Reply ONLY with the numbers of relevant excerpts, comma-separated (e.g. \"1,3,5\").\n" ++
            "If none are relevant, reply \"none\".\n\n",
        .{query},
    );

    for (stages, 0..) |s, i| {
        const truncated = s.content[0..@min(s.content.len, 300)];
        try w.print("[{d}] {s}\n\n", .{ i + 1, truncated });
    }

    const max_response_tokens: u32 = @intCast(@min(stages.len * 4 + 20, 200));
    const response_opt = try client.complete(prompt_buf.items, max_response_tokens, 0.0, null);
    const response = response_opt orelse return error.NoResponse;
    defer allocator.free(response);

    const stripped = llm.stripThinkBlock(response);
    const trimmed = std.mem.trim(u8, stripped, " \t\r\n");

    if (std.ascii.eqlIgnoreCase(trimmed, "none")) {
        return allocator.alloc(usize, 0);
    }

    // Parse comma-separated numbers.
    var indices: std.ArrayList(usize) = .empty;
    errdefer indices.deinit(allocator);

    var parts = std.mem.splitScalar(u8, trimmed, ',');
    while (parts.next()) |part| {
        const num_str = std.mem.trim(u8, part, " \t\r\n");
        if (num_str.len == 0) continue;
        const n = std.fmt.parseInt(usize, num_str, 10) catch continue;
        if (n == 0 or n > stages.len) continue;
        try indices.append(allocator, n - 1); // Convert 1-based to 0-based
    }

    return indices.toOwnedSlice(allocator);
}

/// Transforms a stage by duplicating its data structure using an allocator.
fn dupeStage(allocator: Allocator, s: types.Stage) !types.Stage {
    return types.Stage{
        .kind = s.kind,
        .content = try allocator.dupe(u8, s.content),
        .source = try allocator.dupe(u8, s.source),
        .line = s.line,
    };
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "estimateTokens: approximation" {
    try testing.expectEqual(@as(usize, 1), estimateTokens("hi!"));
    try testing.expectEqual(@as(usize, 2), estimateTokens("hello!  "));
    try testing.expectEqual(@as(usize, 0), estimateTokens(""));
}

test "preFilterByBudget: code always kept" {
    const allocator = testing.allocator;
    const stages = [_]types.Stage{
        .{ .kind = .code, .content = "fn foo() void {}", .source = "a.zig", .line = 1 },
        .{ .kind = .prose, .content = "A" ** 1000, .source = "a.zig" },
        .{ .kind = .metadata, .content = "keywords: foo", .source = "a.zig" },
    };

    // Budget of 10 tokens: code (4 tokens) + metadata (3 tokens) = 7, both fit.
    // Prose is 1000 chars = 250 tokens — exceeds budget.
    const result = try preFilterByBudget(allocator, &stages, 10);
    defer {
        types.freeStages(allocator, result);
        allocator.free(result);
    }

    try testing.expectEqual(@as(usize, 2), result.len);
    for (result) |s| {
        try testing.expect(s.kind == .code or s.kind == .metadata);
    }
}

test "preFilterByBudget: prose included when budget allows" {
    const allocator = testing.allocator;
    const stages = [_]types.Stage{
        .{ .kind = .prose, .content = "Short.", .source = "a.zig" },
    };

    const result = try preFilterByBudget(allocator, &stages, 100);
    defer {
        types.freeStages(allocator, result);
        allocator.free(result);
    }
    try testing.expectEqual(@as(usize, 1), result.len);
}
