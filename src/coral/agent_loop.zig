/// agent_loop.zig — Agent-Loop Reserved Tools (P4.2)
///
/// Defines the reserved tool names handled directly by the agent loop rather
/// than being forwarded to the tool executor.  This mirrors the Hermes
/// `_AGENT_LOOP_TOOLS` pattern.
///
/// §Reserved tools:
///   memory   — Update persistent memory (MEMORY.md).
///   todo     — Manage the session task list.
///   delegate — Spawn a child agent for a sub-task.
///   clarify  — Request clarification from the user.
///
/// §Dispatch:
///   Before forwarding a tool call to the general executor, the agent loop
///   calls `isReserved(tool_name)`.  If true, it calls `handle()` instead.
///   This keeps agent-loop bookkeeping out of the generic tool executor.
const std = @import("std");
const Allocator = std.mem.Allocator;

// ---------------------------------------------------------------------------
// AgentLoopTools
// ---------------------------------------------------------------------------

/// Manages agent loop state with fixed buffers; encapsulates ownership and invariants.
pub const AgentLoopTools = struct {
    /// The canonical set of tool names reserved for the agent loop.
    pub const RESERVED = [_][]const u8{
        "memory",
        "todo",
        "delegate",
        "clarify",
    };

    /// Return true if `tool_name` is a reserved agent-loop tool.
    pub fn isReserved(tool_name: []const u8) bool {
        for (RESERVED) |r| {
            if (std.mem.eql(u8, tool_name, r)) return true;
        }
        return false;
    }

    /// Handle a reserved tool call.  Returns an arena-owned response string.
    ///
    /// `args_json` is the raw JSON arguments string from the LLM.
    pub fn handle(
        arena: Allocator,
        tool_name: []const u8,
        args_json: []const u8,
    ) ![]const u8 {
        if (std.mem.eql(u8, tool_name, "memory")) {
            return handleMemory(arena, args_json);
        } else if (std.mem.eql(u8, tool_name, "todo")) {
            return handleTodo(arena, args_json);
        } else if (std.mem.eql(u8, tool_name, "delegate")) {
            return handleDelegate(arena, args_json);
        } else if (std.mem.eql(u8, tool_name, "clarify")) {
            return handleClarify(arena, args_json);
        }
        return error.UnknownReservedTool;
    }

    // -----------------------------------------------------------------------
    // Reserved tool handlers
    // -----------------------------------------------------------------------

    fn handleMemory(arena: Allocator, args_json: []const u8) ![]const u8 {
        _ = args_json;
        // Stub: in production, parse args_json and update MEMORY.md.
        return arena.dupe(u8, "memory updated");
    }

    fn handleTodo(arena: Allocator, args_json: []const u8) ![]const u8 {
        _ = args_json;
        // Stub: in production, parse args_json and update todo list.
        return arena.dupe(u8, "todo updated");
    }

    fn handleDelegate(arena: Allocator, args_json: []const u8) ![]const u8 {
        _ = args_json;
        // Stub: in production, spawn child agent with depth+1.
        return arena.dupe(u8, "delegation queued");
    }

    fn handleClarify(arena: Allocator, args_json: []const u8) ![]const u8 {
        _ = args_json;
        // Stub: in production, surface the clarification question to the user.
        return arena.dupe(u8, "clarification requested");
    }
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "AgentLoopTools: isReserved recognises all reserved tools" {
    try testing.expect(AgentLoopTools.isReserved("memory"));
    try testing.expect(AgentLoopTools.isReserved("todo"));
    try testing.expect(AgentLoopTools.isReserved("delegate"));
    try testing.expect(AgentLoopTools.isReserved("clarify"));
}

test "AgentLoopTools: isReserved rejects unknown tools" {
    try testing.expect(!AgentLoopTools.isReserved("search"));
    try testing.expect(!AgentLoopTools.isReserved("execute_sql"));
    try testing.expect(!AgentLoopTools.isReserved(""));
}

test "AgentLoopTools: handle returns response for each reserved tool" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();

    const mem_resp = try AgentLoopTools.handle(a, "memory", "{}");
    try testing.expectEqualStrings("memory updated", mem_resp);

    const todo_resp = try AgentLoopTools.handle(a, "todo", "{}");
    try testing.expectEqualStrings("todo updated", todo_resp);

    const del_resp = try AgentLoopTools.handle(a, "delegate", "{}");
    try testing.expectEqualStrings("delegation queued", del_resp);

    const clar_resp = try AgentLoopTools.handle(a, "clarify", "{}");
    try testing.expectEqualStrings("clarification requested", clar_resp);
}

test "AgentLoopTools: handle returns error for unknown tool" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    try testing.expectError(
        error.UnknownReservedTool,
        AgentLoopTools.handle(arena.allocator(), "search", "{}"),
    );
}

test "AgentLoopTools: RESERVED list has 4 entries" {
    try testing.expectEqual(@as(usize, 4), AgentLoopTools.RESERVED.len);
}

