/// context_packer.zig — Context Packing with Head/Tail Protection (P3.3)
///
/// Packs a sequence of `Stage` values into a token-budgeted context window
/// while guaranteeing that essential head and tail stages are never dropped.
///
/// §Head/Tail protection rationale:
///   Head stages carry module-level documentation (purpose, API contract)
///   that anchors the rest of the context.  Tail stages carry caller/used-by
///   relationships that establish structural dependencies.  Without protection,
///   budget enforcement would greedily drop these critical entries first.
///
/// §Stage kinds:
///   .prose — human-readable documentation; filtered by relevance score.
///   .code  — source code; always included regardless of relevance.
///
/// §Budget enforcement:
///   Total token estimate across all included stages must not exceed
///   `config.token_budget`.  Token estimate: (content.len + 3) / 4 (1 tok ≈ 4 bytes).
const std = @import("std");

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub const StageKind = enum { prose, code };

/// Manages stage-specific configuration structures; owned by the context packer; ensures stable, per-stage invariants.
pub const Stage = struct {
    kind: StageKind,
    content: []const u8,
    /// Relevance score in [0.0, 1.0]; used to filter prose body stages.
    /// Code stages ignore this field.
    relevance_score: f32 = 1.0,
};

pub const ContextPackConfig = struct {
    /// Number of leading prose stages to protect unconditionally (module docs).
    head_protect: usize = 2,
    /// Number of trailing stages to protect unconditionally (callers, used-by).
    tail_protect: usize = 3,
    /// Hard cap on number of stages (applied before token budget).
    max_stages: usize = 30,
    /// Maximum total tokens across the packed context.
    token_budget: usize = 8000,
    /// Minimum relevance score for a prose body stage to be included.
    prose_relevance_threshold: f32 = 0.3,
};

// ---------------------------------------------------------------------------
// ContextPacker
// ---------------------------------------------------------------------------

const token_budget = @import("token_budget.zig");

pub const ContextPacker = struct {
    config: ContextPackConfig,

    const Self = @This();

    /// Pack `stages` respecting head/tail protection and the token budget.
    ///
    /// Returns a newly allocated slice of Stage values.  Stage slices are NOT
    /// copied — content pointers refer into the original `stages` data.
    /// The returned slice must be freed by the caller: `allocator.free(result)`.
    pub fn pack(
        self: *const Self,
        allocator: std.mem.Allocator,
        stages: []const Stage,
    ) ![]Stage {
        if (stages.len == 0) return allocator.alloc(Stage, 0);

        var result: std.ArrayListUnmanaged(Stage) = .empty;

        var tokens: usize = 0;
        const cfg = self.config;

        // Clamp head/tail protect counts to actual slice bounds.
        const n = stages.len;
        const head = @min(cfg.head_protect, n);
        const tail = if (n > cfg.head_protect) @min(cfg.tail_protect, n - head) else 0;
        const body_start = head;
        const body_end = if (n > tail) n - tail else n;

        // ── Head: always include (prose or code), up to budget.
        for (stages[0..head]) |s| {
            try result.append(allocator, s);
            tokens += estimateTokens(s.content);
        }

        // ── Body: code always kept; prose filtered by relevance + budget.
        if (body_end > body_start) {
            for (stages[body_start..body_end]) |s| {
                if (tokens >= cfg.token_budget) break;
                if (result.items.len >= cfg.max_stages) break;

                switch (s.kind) {
                    .code => {
                        try result.append(allocator, s);
                        tokens += estimateTokens(s.content);
                    },
                    .prose => {
                        if (s.relevance_score >= cfg.prose_relevance_threshold) {
                            try result.append(allocator, s);
                            tokens += estimateTokens(s.content);
                        }
                    },
                }
            }
        }

        // ── Tail: always include, regardless of remaining budget.
        if (tail > 0) {
            for (stages[n - tail ..]) |s| {
                try result.append(allocator, s);
            }
        }

        return result.toOwnedSlice(allocator);
    }

    /// Like pack(), but returns the indices of selected stages into the original
    /// `stages` slice rather than copying Stage values.  Useful when the caller's
    /// Stage type is richer than context_packer.Stage and it needs to preserve
    /// extra fields (e.g. source path, line number) that are not present here.
    ///
    /// The selection algorithm is identical to pack().  Code stages are always
    /// selected; prose body stages are filtered by relevance + token budget.
    /// Head and tail indices are always included.
    ///
    /// The returned slice must be freed by the caller: `allocator.free(result)`.
    pub fn packIndices(
        self: *const Self,
        allocator: std.mem.Allocator,
        stages: []const Stage,
    ) ![]usize {
        if (stages.len == 0) return allocator.alloc(usize, 0);

        var result: std.ArrayListUnmanaged(usize) = .empty;

        var tokens: usize = 0;
        const cfg = self.config;

        const n = stages.len;
        const head = @min(cfg.head_protect, n);
        const tail = if (n > cfg.head_protect) @min(cfg.tail_protect, n - head) else 0;
        const body_start = head;
        const body_end = if (n > tail) n - tail else n;

        // ── Head: always include.
        for (0..head) |i| {
            try result.append(allocator, i);
            tokens += estimateTokens(stages[i].content);
        }

        // ── Body: code always kept; prose filtered by relevance + budget.
        if (body_end > body_start) {
            for (body_start..body_end) |i| {
                if (tokens >= cfg.token_budget) break;
                if (result.items.len >= cfg.max_stages) break;

                const s = stages[i];
                switch (s.kind) {
                    .code => {
                        try result.append(allocator, i);
                        tokens += estimateTokens(s.content);
                    },
                    .prose => {
                        if (s.relevance_score >= cfg.prose_relevance_threshold) {
                            try result.append(allocator, i);
                            tokens += estimateTokens(s.content);
                        }
                    },
                }
            }
        }

        // ── Tail: always include.
        if (tail > 0) {
            for ((n - tail)..n) |i| {
                try result.append(allocator, i);
            }
        }

        return result.toOwnedSlice(allocator);
    }
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Calculates token estimates from Zig content, returning a usize value.
fn estimateTokens(content: []const u8) usize {
    return token_budget.estimate(content);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

/// Creates a Zig stage instance with specified kind, content, and relevance parameters.
fn makeStage(kind: StageKind, content: []const u8, relevance: f32) Stage {
    return .{ .kind = kind, .content = content, .relevance_score = relevance };
}

test "ContextPacker: empty input" {
    const packer = ContextPacker{ .config = .{} };
    const result = try packer.pack(testing.allocator, &[_]Stage{});
    defer testing.allocator.free(result);
    try testing.expectEqual(@as(usize, 0), result.len);
}

test "ContextPacker: head stages always included" {
    const stages = [_]Stage{
        makeStage(.prose, "module purpose", 1.0),
        makeStage(.prose, "primary file", 1.0),
        makeStage(.prose, "body 1", 0.0), // low relevance — normally filtered
        makeStage(.prose, "caller info", 1.0),
    };
    // head=2, tail=1, so body=[stages[2]], tail=[stages[3]]
    const packer = ContextPacker{ .config = .{
        .head_protect = 2,
        .tail_protect = 1,
        .token_budget = 100000,
        .prose_relevance_threshold = 0.5,
    } };
    const result = try packer.pack(testing.allocator, &stages);
    defer testing.allocator.free(result);

    // Head (2) included + tail (1) included; body (1) filtered out.
    try testing.expectEqual(@as(usize, 3), result.len);
    try testing.expectEqualStrings("module purpose", result[0].content);
    try testing.expectEqualStrings("primary file", result[1].content);
    try testing.expectEqualStrings("caller info", result[2].content);
}

test "ContextPacker: code stages always included in body" {
    const stages = [_]Stage{
        makeStage(.prose, "head", 1.0),
        makeStage(.code, "fn foo() void {}", 0.0), // code, relevance ignored
        makeStage(.prose, "tail", 1.0),
    };
    const packer = ContextPacker{ .config = .{
        .head_protect = 1,
        .tail_protect = 1,
        .token_budget = 100000,
        .prose_relevance_threshold = 0.5,
    } };
    const result = try packer.pack(testing.allocator, &stages);
    defer testing.allocator.free(result);
    try testing.expectEqual(@as(usize, 3), result.len);
    try testing.expectEqualStrings("fn foo() void {}", result[1].content);
}

test "ContextPacker: token budget enforced on body" {
    // Head: 1 stage (4 chars = 1 token). Budget = 0 tokens.
    // Body stages are code, but 0-budget means any accumulated tokens >= 0 → skip.
    const stages = [_]Stage{
        makeStage(.prose, "head", 1.0),
        makeStage(.code, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 1.0),
        makeStage(.code, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", 1.0),
        makeStage(.prose, "tail", 1.0),
    };
    const packer = ContextPacker{
        .config = .{
            .head_protect = 1,
            .tail_protect = 1,
            .token_budget = 0, // budget exhausted immediately after head
            .prose_relevance_threshold = 0.0,
        },
    };
    const result = try packer.pack(testing.allocator, &stages);
    defer testing.allocator.free(result);
    // Head (1 tok > 0 budget, but head always included) + body dropped + tail always included.
    try testing.expectEqual(@as(usize, 2), result.len);
    try testing.expectEqualStrings("head", result[0].content);
    try testing.expectEqualStrings("tail", result[1].content);
}

test "ContextPacker: tail always included even over budget" {
    const big = "x" ** 200; // 50 tokens
    const stages = [_]Stage{
        makeStage(.prose, "head", 1.0),
        makeStage(.prose, "tail", 1.0),
    };
    _ = big;
    const packer = ContextPacker{
        .config = .{
            .head_protect = 1,
            .tail_protect = 1,
            .token_budget = 0, // zero budget — still keep head + tail
            .prose_relevance_threshold = 0.0,
        },
    };
    const result = try packer.pack(testing.allocator, &stages);
    defer testing.allocator.free(result);
    try testing.expectEqual(@as(usize, 2), result.len);
}

test "ContextPacker: max_stages enforced" {
    const packer = ContextPacker{ .config = .{
        .head_protect = 1,
        .tail_protect = 1,
        .max_stages = 3,
        .token_budget = 100000,
        .prose_relevance_threshold = 0.0,
    } };
    // 6 stages: head=1, body=4, tail=1
    const stages = [_]Stage{
        makeStage(.prose, "head", 1.0),
        makeStage(.code, "b1", 1.0),
        makeStage(.code, "b2", 1.0),
        makeStage(.code, "b3", 1.0),
        makeStage(.code, "b4", 1.0),
        makeStage(.prose, "tail", 1.0),
    };
    const result = try packer.pack(testing.allocator, &stages);
    defer testing.allocator.free(result);
    // head(1) + body capped at max_stages=3 minus head=1 → 2 body + tail(1)
    try testing.expect(result.len <= 4);
}
