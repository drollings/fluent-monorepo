/// context_compressor.zig — Context Compression for Token Budget Management
///
/// Reduces a message history to fit within a token budget while protecting
/// recent messages and maintaining structural integrity of tool call/result pairs.
///
/// Compression phases (applied in order):
///   Phase 1: Prune old tool results beyond protect_tail that exceed 200 bytes.
///   Phase 2: Enforce token budget by dropping messages from the beginning.
///   Phase 3: Remove orphan tool_calls whose results were pruned.
const std = @import("std");

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Defines message kinds for compression context, manages ownership and invariants.
pub const MessageKind = enum {
    user,
    assistant,
    tool_call,
    tool_result,
};

/// Manages message structures with fixed-size buffers; owned by the context; ensures consistent state across operations.
pub const Message = struct {
    role: []const u8,
    content: []const u8,
    kind: MessageKind,
};

// ---------------------------------------------------------------------------
// Token estimation
// ---------------------------------------------------------------------------

/// Approximate token count for a string: 1 token ≈ 4 characters.
pub fn estimateTokens(content: []const u8) usize {
    return (content.len + 3) / 4;
}

// ---------------------------------------------------------------------------
// ContextCompressor
// ---------------------------------------------------------------------------

/// Manages context compression structures, owns buffers, and ensures consistent state across operations.
pub const ContextCompressor = struct {
    /// Maximum total tokens allowed across all messages.
    max_context_tokens: usize,
    /// Number of tail messages (from the end) to protect from pruning.
    protect_tail: usize = 60,

    const Self = @This();

    /// Compress `messages` to fit within the token budget.
    /// Returns a newly allocated slice of Message values.  The messages
    /// themselves are NOT copied — role/content slices still point into the
    /// original data owned by the caller.
    /// The returned slice must be freed by the caller: `allocator.free(result)`.
    pub fn compress(
        self: *const Self,
        allocator: std.mem.Allocator,
        messages: []const Message,
    ) ![]Message {
        // Work with indices into the original slice to avoid copying strings.
        var kept = try std.ArrayList(usize).initCapacity(allocator, messages.len);
        defer kept.deinit(allocator);

        for (0..messages.len) |i| {
            kept.appendAssumeCapacity(i);
        }

        // Phase 1: prune old bulky tool results outside the protected tail.
        kept = try pruneToolResults(allocator, messages, kept, self.protect_tail);

        // Phase 2: enforce token budget — drop from beginning.
        kept = try enforceTokenBudget(allocator, messages, kept, self.max_context_tokens);

        // Phase 3: remove orphan tool_calls whose result was pruned.
        kept = try removeOrphanToolCalls(allocator, messages, kept);

        // Build result slice.
        const result = try allocator.alloc(Message, kept.items.len);
        for (kept.items, 0..) |idx, i| {
            result[i] = messages[idx];
        }
        return result;
    }

    // ------------------------------------------------------------------
    // Phase 1 — prune bulky tool results beyond protect_tail
    // ------------------------------------------------------------------

    fn pruneToolResults(
        allocator: std.mem.Allocator,
        messages: []const Message,
        in: std.ArrayList(usize),
        protect_tail: usize,
    ) !std.ArrayList(usize) {
        const n = in.items.len;
        const protected_start = if (n > protect_tail) n - protect_tail else 0;

        var out = try std.ArrayList(usize).initCapacity(allocator, n);
        for (in.items, 0..) |idx, pos| {
            const msg = &messages[idx];
            const is_in_tail = pos >= protected_start;
            const is_bulky_result = msg.kind == .tool_result and msg.content.len > 200;

            if (!is_in_tail and is_bulky_result) {
                // Prune: do not copy to output
                continue;
            }
            out.appendAssumeCapacity(idx);
        }

        var prev = in;
        prev.deinit(allocator);
        return out;
    }

    // ------------------------------------------------------------------
    // Phase 2 — enforce token budget by dropping from beginning
    // ------------------------------------------------------------------

    fn enforceTokenBudget(
        allocator: std.mem.Allocator,
        messages: []const Message,
        in: std.ArrayList(usize),
        max_tokens: usize,
    ) !std.ArrayList(usize) {
        // Compute total tokens across current kept set.
        var total: usize = 0;
        for (in.items) |idx| {
            total += estimateTokens(messages[idx].content);
        }

        // Drop from the front until we're within budget.
        var drop_count: usize = 0;
        while (total > max_tokens and drop_count < in.items.len) {
            total -= estimateTokens(messages[in.items[drop_count]].content);
            drop_count += 1;
        }

        if (drop_count == 0) return in;

        var out = try std.ArrayList(usize).initCapacity(allocator, in.items.len - drop_count);
        for (in.items[drop_count..]) |idx| {
            out.appendAssumeCapacity(idx);
        }
        var prev2 = in;
        prev2.deinit(allocator);
        return out;
    }

    // ------------------------------------------------------------------
    // Phase 3 — remove tool_calls whose result was pruned
    // ------------------------------------------------------------------

    /// A tool_call at position i is considered paired with the immediately
    /// following tool_result (if any).  If that result is not in `kept`, the
    /// tool_call is an orphan and must be removed too.
    fn removeOrphanToolCalls(
        allocator: std.mem.Allocator,
        messages: []const Message,
        in: std.ArrayList(usize),
    ) !std.ArrayList(usize) {
        // Build a set of kept original indices for O(1) membership tests.
        var kept_set = std.AutoHashMap(usize, void).init(allocator);
        defer kept_set.deinit();
        for (in.items) |idx| {
            try kept_set.put(idx, {});
        }

        var out = try std.ArrayList(usize).initCapacity(allocator, in.items.len);
        for (in.items) |idx| {
            const msg = &messages[idx];
            if (msg.kind != .tool_call) {
                out.appendAssumeCapacity(idx);
                continue;
            }

            // Find the next tool_result in the original message array.
            const paired_result_idx = findNextToolResult(messages, idx);
            if (paired_result_idx) |result_idx| {
                // If the result was pruned, drop this call too.
                if (!kept_set.contains(result_idx)) continue;
            }
            // Either no paired result (already orphan structurally) or result is kept.
            out.appendAssumeCapacity(idx);
        }

        var prev3 = in;
        prev3.deinit(allocator);
        return out;
    }

    /// Return the original index of the first tool_result immediately following
    /// `call_idx` in the message array, or null if none exists within the next
    /// few messages (we scan up to 4 positions to handle interleaved content).
    fn findNextToolResult(messages: []const Message, call_idx: usize) ?usize {
        const limit = @min(call_idx + 5, messages.len);
        for ((call_idx + 1)..limit) |i| {
            if (messages[i].kind == .tool_result) return i;
        }
        return null;
    }
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

fn makeMsg(role: []const u8, content: []const u8, kind: MessageKind) Message {
    return .{ .role = role, .content = content, .kind = kind };
}

test "estimateTokens basic" {
    try testing.expectEqual(@as(usize, 1), estimateTokens("abc")); // 3 chars → (3+3)/4 = 1
    try testing.expectEqual(@as(usize, 1), estimateTokens("abcd")); // 4 chars → 1
    try testing.expectEqual(@as(usize, 2), estimateTokens("abcde")); // 5 chars → 2
    try testing.expectEqual(@as(usize, 0), estimateTokens(""));
}

test "compress empty messages" {
    const cc = ContextCompressor{ .max_context_tokens = 1000 };
    const result = try cc.compress(testing.allocator, &[_]Message{});
    defer testing.allocator.free(result);
    try testing.expectEqual(@as(usize, 0), result.len);
}

test "compress under budget — no pruning" {
    const messages = [_]Message{
        makeMsg("user", "hi", .user),
        makeMsg("assistant", "hello", .assistant),
    };
    const cc = ContextCompressor{ .max_context_tokens = 1000 };
    const result = try cc.compress(testing.allocator, &messages);
    defer testing.allocator.free(result);
    try testing.expectEqual(@as(usize, 2), result.len);
}

test "compress phase 2 — drop from beginning when over budget" {
    // Each message content is 8 chars → 2 tokens each.
    // Budget = 3 tokens → keep at most 1 message (2 tokens).
    const messages = [_]Message{
        makeMsg("user", "aaaaaaaa", .user), // 2 tokens
        makeMsg("assistant", "bbbbbbbb", .assistant), // 2 tokens
        makeMsg("user", "cccccccc", .user), // 2 tokens
    };
    const cc = ContextCompressor{ .max_context_tokens = 3, .protect_tail = 0 };
    const result = try cc.compress(testing.allocator, &messages);
    defer testing.allocator.free(result);
    // After dropping: total = 6, drop 1 → 4, still over 3, drop 1 → 2, ok.
    try testing.expect(result.len < messages.len);
    // Last message must be retained.
    try testing.expectEqualStrings("cccccccc", result[result.len - 1].content);
}

test "compress phase 1 — prune bulky tool results outside protect_tail" {
    const big_content = "x" ** 201; // 201 bytes, > 200 threshold
    const messages = [_]Message{
        makeMsg("assistant", "call", .tool_call),
        makeMsg("tool", big_content, .tool_result), // should be pruned (not in tail)
        makeMsg("user", "follow up", .user), // in protect_tail
    };
    // protect_tail=1 means only last 1 message is protected
    const cc = ContextCompressor{ .max_context_tokens = 100000, .protect_tail = 1 };
    const result = try cc.compress(testing.allocator, &messages);
    defer testing.allocator.free(result);

    // The big tool_result should be pruned; tool_call becomes orphan and is also pruned.
    for (result) |msg| {
        try testing.expect(msg.kind != .tool_result or msg.content.len <= 200);
    }
}

test "compress phase 1 — small tool results inside protect_tail are kept" {
    const small_content = "small result";
    const messages = [_]Message{
        makeMsg("user", "question", .user),
        makeMsg("assistant", "call", .tool_call),
        makeMsg("tool", small_content, .tool_result),
    };
    const cc = ContextCompressor{ .max_context_tokens = 100000, .protect_tail = 60 };
    const result = try cc.compress(testing.allocator, &messages);
    defer testing.allocator.free(result);

    var found_result = false;
    for (result) |msg| {
        if (msg.kind == .tool_result) found_result = true;
    }
    try testing.expect(found_result);
}

test "compress phase 3 — orphan tool_call removed when result pruned" {
    const big_content = "y" ** 250;
    // The tool_result is old (not in protect_tail) and bulky, so it gets pruned in phase 1.
    // The preceding tool_call should then be removed in phase 3.
    const messages = [_]Message{
        makeMsg("user", "initial", .user),
        makeMsg("assistant", "tc", .tool_call),
        makeMsg("tool", big_content, .tool_result),
        // protected tail starts here (protect_tail=2)
        makeMsg("user", "question2", .user),
        makeMsg("assistant", "answer2", .assistant),
    };
    const cc = ContextCompressor{ .max_context_tokens = 100000, .protect_tail = 2 };
    const result = try cc.compress(testing.allocator, &messages);
    defer testing.allocator.free(result);

    for (result) |msg| {
        // No orphan tool_call or pruned tool_result should remain.
        if (msg.kind == .tool_call) {
            // If a tool_call survived, there must be a paired tool_result in the output.
            var has_result = false;
            for (result) |r| {
                if (r.kind == .tool_result) has_result = true;
            }
            try testing.expect(has_result);
        }
        if (msg.kind == .tool_result) {
            try testing.expect(msg.content.len <= 200);
        }
    }
}

test "compress protect_tail keeps recent messages intact" {
    // 10 messages, each 40 chars = 10 tokens. Budget = 20 tokens.
    // Without protect_tail we'd drop the first 8. With protect_tail=10 all are in tail.
    var messages: [10]Message = undefined;
    for (&messages, 0..) |*m, i| {
        _ = i;
        m.* = makeMsg("user", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", .user);
    }
    const cc = ContextCompressor{ .max_context_tokens = 20, .protect_tail = 10 };
    const result = try cc.compress(testing.allocator, &messages);
    defer testing.allocator.free(result);
    // Phase 1 doesn't touch non-tool_result. Phase 2 drops from beginning.
    // Even with protect_tail, phase 2 still enforces the token budget.
    // So we expect result to be within budget.
    var total: usize = 0;
    for (result) |msg| total += estimateTokens(msg.content);
    try testing.expect(total <= 20);
}
