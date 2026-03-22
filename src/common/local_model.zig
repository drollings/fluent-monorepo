/// local_model.zig — Local LLM Task Decomposition (P6.1)
///
/// LocalDecomposer calls a local LLM to break a complex query into an ordered
/// list of simpler sub-tasks.  The LLM is prompted to return a JSON array of
/// strings.  Malformed responses (non-array, empty, think-block garbage) are
/// detected with isMalformedResponse() and a fallback single-task slice is
/// returned so the caller always gets at least one workable item.
///
/// Arena contract: the returned [][]const u8 and all strings within it are
/// allocated from the caller-supplied arena.  Callers own the arena lifetime.
const std = @import("std");
const llm_root = @import("llm");

pub const LlmClient = llm_root.LlmClient;
pub const LlmConfig = llm_root.LlmConfig;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

pub const DecomposerConfig = struct {
    /// LLM API config (endpoint + model).
    llm: LlmConfig,
    /// Maximum sub-tasks the LLM is allowed to return.
    max_subtasks: usize = 5,
    /// Maximum recursion depth for sub-task routing (enforced by caller).
    max_depth: u8 = 2,
};

// ---------------------------------------------------------------------------
// LocalDecomposer
// ---------------------------------------------------------------------------

pub const LocalDecomposer = struct {
    allocator: std.mem.Allocator,
    config: DecomposerConfig,

    pub fn init(allocator: std.mem.Allocator, config: DecomposerConfig) LocalDecomposer {
        return .{ .allocator = allocator, .config = config };
    }

    /// Decompose `task` into an ordered list of sub-task strings.
    ///
    /// Returns a slice allocated from `arena`.  On any LLM failure or malformed
    /// response the function returns a single-element slice containing `task`
    /// so the caller always has at least one item to route.
    pub fn decompose(self: *LocalDecomposer, arena: std.mem.Allocator, task: []const u8) ![][]const u8 {
        var client = LlmClient.init(self.allocator, self.config.llm) catch {
            return self.fallback(arena, task);
        };
        defer client.deinit();

        const system_prompt =
            \\You are a task planner. Given a user query, decompose it into at most 5
            \\concrete, ordered sub-tasks. Reply with ONLY a JSON array of strings, no
            \\preamble, no explanation. Example:
            \\["Find relevant documents","Filter by date","Summarize results"]
        ;

        const raw = client.complete(task, 256, 0.2, system_prompt) catch {
            return self.fallback(arena, task);
        } orelse return self.fallback(arena, task);
        defer self.allocator.free(raw);

        // Strip think blocks before parsing.
        const stripped = stripThinkBlock(raw);
        if (isMalformedJsonArray(stripped)) return self.fallback(arena, task);

        return parseJsonArray(arena, stripped, self.config.max_subtasks) catch
            self.fallback(arena, task);
    }

    // -------------------------------------------------------------------------
    // Private helpers
    // -------------------------------------------------------------------------

    fn fallback(self: *LocalDecomposer, arena: std.mem.Allocator, task: []const u8) ![][]const u8 {
        _ = self;
        const tasks = try arena.alloc([]const u8, 1);
        tasks[0] = try arena.dupe(u8, task);
        return tasks;
    }
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Strip <think>...</think> blocks (identical to common/llm.zig version but
/// inlined here to avoid importing the full common module root from common/).
fn stripThinkBlock(text: []const u8) []const u8 {
    if (std.mem.indexOf(u8, text, "<think>")) |start| {
        if (std.mem.indexOfPos(u8, text, start + 7, "</think>")) |end| {
            const after = end + 8;
            if (after >= text.len) return "";
            return std.mem.trim(u8, text[after..], " \t\r\n");
        }
        return std.mem.trim(u8, text[0..start], " \t\r\n");
    }
    return text;
}

/// Return true if `text` does not look like a JSON array.
fn isMalformedJsonArray(text: []const u8) bool {
    const t = std.mem.trim(u8, text, " \t\r\n");
    if (t.len == 0) return true;
    if (t[0] != '[') return true;
    if (t[t.len - 1] != ']') return true;
    return false;
}

/// Parse a JSON array of strings from `text` into `arena`-allocated slice.
/// At most `limit` entries are returned.
fn parseJsonArray(arena: std.mem.Allocator, text: []const u8, limit: usize) ![][]const u8 {
    var parsed = try std.json.parseFromSlice(std.json.Value, arena, text, .{});
    defer parsed.deinit();

    const arr = switch (parsed.value) {
        .array => |a| a,
        else => return error.NotAnArray,
    };

    const count = @min(arr.items.len, limit);
    if (count == 0) return error.EmptyArray;

    const result = try arena.alloc([]const u8, count);
    for (arr.items[0..count], 0..) |item, i| {
        const s = switch (item) {
            .string => |str| str,
            else => return error.NotAStringArray,
        };
        result[i] = try arena.dupe(u8, s);
    }
    return result;
}

// =============================================================================
// Tests — P6.1
// =============================================================================

const testing = std.testing;

test "isMalformedJsonArray: rejects non-arrays" {
    try testing.expect(isMalformedJsonArray(""));
    try testing.expect(isMalformedJsonArray("hello"));
    try testing.expect(isMalformedJsonArray("{\"a\":1}"));
    try testing.expect(isMalformedJsonArray("[\"unclosed"));
}

test "isMalformedJsonArray: accepts well-formed arrays" {
    try testing.expect(!isMalformedJsonArray("[\"a\",\"b\"]"));
    try testing.expect(!isMalformedJsonArray("[]"));
}

test "stripThinkBlock: removes think tags" {
    const raw = "<think>reasoning here</think>actual answer";
    const result = stripThinkBlock(raw);
    try testing.expectEqualStrings("actual answer", result);
}

test "stripThinkBlock: no think block passes through" {
    const raw = "[\"task1\",\"task2\"]";
    try testing.expectEqualStrings(raw, stripThinkBlock(raw));
}

test "parseJsonArray: parses simple array" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();
    const result = try parseJsonArray(a, "[\"task1\",\"task2\",\"task3\"]", 10);
    try testing.expectEqual(@as(usize, 3), result.len);
    try testing.expectEqualStrings("task1", result[0]);
    try testing.expectEqualStrings("task3", result[2]);
}

test "parseJsonArray: respects limit" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();
    const result = try parseJsonArray(a, "[\"a\",\"b\",\"c\",\"d\",\"e\"]", 3);
    try testing.expectEqual(@as(usize, 3), result.len);
}

test "LocalDecomposer.fallback returns single task" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    var decomposer = LocalDecomposer.init(testing.allocator, .{
        .llm = .{ .api_url = "http://localhost:11434/v1/chat/completions", .model = "test" },
    });
    const result = try decomposer.fallback(arena.allocator(), "find scientists");
    try testing.expectEqual(@as(usize, 1), result.len);
    try testing.expectEqualStrings("find scientists", result[0]);
}
