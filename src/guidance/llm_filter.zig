//! llm_filter.zig — LLM-based relevance filtering for the staged explain pipeline.
//!
//! For each prose, insight, or skill_doc Stage, asks a local LLM:
//!   "Is this content relevant to the query? Answer YES or NO."
//!
//! Code and metadata stages are always kept verbatim.
//! On LLM error or unavailability, returns all input stages unchanged.

const std = @import("std");
const llm = @import("common");
const types = @import("types.zig");

/// Filter `stages` for relevance to `query` using the provided LLM client.
///
/// - .code and .metadata stages are always included.
/// - .prose, .insight, .skill_doc stages are evaluated: kept if LLM says YES.
///   On LLM failure the stage is kept (fail-open = better recall).
///
/// Returns a new owned slice of Stages (each Stage has freshly-duped strings).
/// Caller must free with types.freeStages() then allocator.free(slice).
pub fn filterStages(
    allocator: std.mem.Allocator,
    client: *llm.LlmClient,
    query: []const u8,
    stages: []const types.Stage,
) ![]types.Stage {
    var out: std.ArrayList(types.Stage) = .{};
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

/// Ask the LLM whether `content` is relevant to `query`.
/// Returns true (keep) or false (discard).  Fails open on error.
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

/// Deep-copy a Stage so the returned copy fully owns its strings.
fn dupeStage(allocator: std.mem.Allocator, s: types.Stage) !types.Stage {
    return types.Stage{
        .kind = s.kind,
        .content = try allocator.dupe(u8, s.content),
        .source = try allocator.dupe(u8, s.source),
        .line = s.line,
    };
}
