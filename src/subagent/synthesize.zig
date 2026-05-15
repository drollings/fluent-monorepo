//! synthesize.zig — Context-isolated summarization for the subagent FSM.
//!
//! Uses context packing to produce bounded-token SummarizedContext from tool
//! results. Structured results (bash output, explain stages) pass through
//! directly when they fit the budget. Unstructured or too-long results are
//! compressed via an LLM synthesis call with scratchpad context.
//!
//! M7 integration: ContextPacker for budget-aware stage assembly and
//! ContextCompressor for scratchpad → LLM context compression.

const std = @import("std");
const types = @import("types.zig");
const string_mod = @import("common").string;
const token_budget = @import("llm").token_budget;
const context_packer = @import("llm").context_packer;
const context_compressor = @import("llm").context_compressor;

pub const LlmFn = fn (allocator: std.mem.Allocator, prompt: []const u8, system: []const u8, max_tokens: u32) ?[]const u8;

pub const SynthesizeResult = struct {
    context: types.SummarizedContext,
    used_llm: bool,
};

pub fn synthesize(
    allocator: std.mem.Allocator,
    result: *const types.ToolResult,
    item: types.ChecklistItem,
    scratchpad_ctx: ?[]const u8,
    llm_fn: ?*const LlmFn,
    max_summary_tokens: u32,
) !SynthesizeResult {
    if (result.success and result.structured != null) {
        return synthesizeStructured(allocator, result, item, max_summary_tokens);
    }

    if (result.success and result.raw != null) {
        const raw = result.raw.?;
        const raw_tokens = token_budget.estimate(raw);
        if (raw_tokens <= max_summary_tokens) {
            return .{
                .context = .{
                    .summary = try allocator.dupe(u8, raw),
                    .facts = &.{},
                    .citations = &.{},
                    .token_cost = @intCast(raw_tokens),
                },
                .used_llm = false,
            };
        }
        if (llm_fn) |fn_ptr| {
            return synthesizeViaLlm(allocator, raw, item, scratchpad_ctx, fn_ptr, max_summary_tokens);
        }
        const truncated = string_mod.truncateAtSentence(allocator, raw, max_summary_tokens * 4) catch raw;
        return .{
            .context = .{
                .summary = if (truncated.ptr != raw.ptr) truncated else try allocator.dupe(u8, truncated),
                .facts = &.{},
                .citations = &.{},
                .token_cost = @intCast(token_budget.estimate(truncated)),
            },
            .used_llm = false,
        };
    }

    const msg = if (result.raw) |r| r else "tool completed with no output";
    return .{
        .context = .{
            .summary = try allocator.dupe(u8, msg),
            .facts = &.{},
            .citations = &.{},
            .token_cost = @intCast(token_budget.estimate(msg)),
        },
        .used_llm = false,
    };
}

fn synthesizeStructured(
    allocator: std.mem.Allocator,
    result: *const types.ToolResult,
    item: types.ChecklistItem,
    max_summary_tokens: u32,
) !SynthesizeResult {
    _ = max_summary_tokens;
    var facts_list: std.ArrayList([]const u8) = .empty;
    errdefer {
        for (facts_list.items) |f| allocator.free(f);
        facts_list.deinit(allocator);
    }
    var citations_list: std.ArrayList(types.Citation) = .empty;
    errdefer citations_list.deinit(allocator);

    if (result.stages) |stages| {
        const packer = context_packer.ContextPacker{ .config = .{
            .head_protect = 1,
            .tail_protect = 2,
            .max_stages = 15,
            .token_budget = 4000,
            .prose_relevance_threshold = 0.3,
        } };

        var packer_stages: std.ArrayList(context_packer.Stage) = .empty;
        defer packer_stages.deinit(allocator);
        for (stages) |stage| {
            const kind: context_packer.StageKind = switch (stage.kind) {
                .prose => .prose,
                .code => .code,
                .metadata => .prose,
                .insight => .prose,
                .skill_doc => .prose,
                .capability_doc => .prose,
                .not_found => .prose,
            };
            try packer_stages.append(allocator, .{
                .kind = kind,
                .content = stage.content,
                .relevance_score = @floatCast(stage.relevance),
            });
        }

        const selected = packer.pack(allocator, packer_stages.items) catch &[_]context_packer.Stage{};
        defer allocator.free(selected);

        var summary_buf: std.ArrayList(u8) = .empty;
        errdefer summary_buf.deinit(allocator);
        for (selected) |ps| {
            if (summary_buf.items.len > 0) try summary_buf.append(allocator, '\n');
            try summary_buf.appendSlice(allocator, ps.content);
        }
        const summary = try summary_buf.toOwnedSlice(allocator);

        for (stages) |stage| {
            if (stage.source.len > 0) {
                try citations_list.append(allocator, .{
                    .file = try allocator.dupe(u8, stage.source),
                    .line = stage.line,
                });
            }
        }

        const token_cost: u32 = @intCast(token_budget.estimate(summary));
        return .{
            .context = .{
                .summary = summary,
                .facts = facts_list.items,
                .citations = citations_list.items,
                .token_cost = token_cost,
            },
            .used_llm = false,
        };
    }

    const summary = try allocator.dupe(u8, result.structured orelse item.text);
    const token_cost: u32 = @intCast(token_budget.estimate(summary));

    return .{
        .context = .{
            .summary = summary,
            .facts = facts_list.items,
            .citations = citations_list.items,
            .token_cost = token_cost,
        },
        .used_llm = false,
    };
}

fn synthesizeViaLlm(
    allocator: std.mem.Allocator,
    raw: []const u8,
    item: types.ChecklistItem,
    scratchpad_ctx: ?[]const u8,
    llm_fn: *const LlmFn,
    max_summary_tokens: u32,
) !SynthesizeResult {
    var prompt_buf: std.ArrayList(u8) = .empty;
    errdefer prompt_buf.deinit(allocator);

    try prompt_buf.appendSlice(allocator, "Summarize the following result in under ");
    var aw: std.Io.Writer.Allocating = .init(allocator);
    errdefer aw.deinit();
    try aw.writer.print("{d}", .{max_summary_tokens});
    const num = try aw.toOwnedSlice();
    defer allocator.free(num);
    try prompt_buf.appendSlice(allocator, " tokens.\n\nItem: ");
    try prompt_buf.appendSlice(allocator, item.text);
    try prompt_buf.appendSlice(allocator, "\n\nResult:\n");
    const truncated_raw = string_mod.truncateAtSentence(allocator, raw, 2000) catch raw;
    try prompt_buf.appendSlice(allocator, truncated_raw);

    if (scratchpad_ctx) |ctx| {
        const compressor = context_compressor.ContextCompressor{ .max_context_tokens = 1000, .protect_tail = 20 };
        const messages = [_]context_compressor.Message{
            .{ .role = "user", .content = ctx, .kind = .user },
        };
        const compressed = compressor.compress(allocator, &messages) catch &[_]context_compressor.Message{};
        defer allocator.free(compressed);

        try prompt_buf.appendSlice(allocator, "\n\nContext from previous iterations:\n");
        for (compressed) |msg| {
            try prompt_buf.appendSlice(allocator, msg.content);
            try prompt_buf.appendSlice(allocator, "\n");
        }
    }

    if (llm_fn(allocator, prompt_buf.items, "You are a concise summarizer for a software engineering subagent. Extract key facts and actionable information only.", max_summary_tokens)) |summary| {
        return .{
            .context = .{
                .summary = summary,
                .facts = &.{},
                .citations = &.{},
                .token_cost = @intCast(token_budget.estimate(summary)),
            },
            .used_llm = true,
        };
    }

    const fallback = string_mod.truncateAtSentence(allocator, raw, max_summary_tokens * 4) catch raw;
    return .{
        .context = .{
            .summary = if (fallback.ptr != raw.ptr) fallback else try allocator.dupe(u8, fallback),
            .facts = &.{},
            .citations = &.{},
            .token_cost = @intCast(token_budget.estimate(fallback)),
        },
        .used_llm = false,
    };
}

const testing = std.testing;

test "synthesize: pass-through for short bash output" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const item: types.ChecklistItem = .{ .index = 0, .text = "run make test", .completed = false, .line_number = 1 };
    const result: types.ToolResult = .{ .action = .bash, .success = true, .raw = "all tests passed" };
    const synthesized = try synthesize(allocator, &result, item, null, null, 200);

    try testing.expect(!synthesized.used_llm);
    try testing.expect(synthesized.context.token_cost > 0);
    allocator.free(synthesized.context.summary);
}

test "synthesize: structured explain result" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const item: types.ChecklistItem = .{ .index = 0, .text = "explain filterStages", .completed = false, .line_number = 1 };
    var stages = [_]types.ExplainStage{
        .{ .kind = .code, .content = "fn filterStages(...)", .source = "staged.zig", .line = 30, .relevance = 1.0 },
    };
    const result: types.ToolResult = .{
        .action = .explain,
        .success = true,
        .structured = "filterStages is a function...",
        .stages = &stages,
    };
    const synthesized = try synthesize(allocator, &result, item, null, null, 200);

    try testing.expect(!synthesized.used_llm);
    try testing.expect(synthesized.context.citations.len >= 1);
    allocator.free(synthesized.context.summary);
    for (synthesized.context.citations) |c| {
        if (c.file) |f| allocator.free(f);
    }
    allocator.free(synthesized.context.citations);
}
