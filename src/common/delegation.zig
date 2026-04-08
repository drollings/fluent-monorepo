/// delegation.zig — Delegation Pattern for Child Agent Spawning (P4.3)
///
/// Implements depth-limited child agent delegation.  A parent agent may
/// delegate a sub-task to a child agent.  The child operates with a subset
/// of the parent's toolsets (isolation) and is subject to an iteration budget
/// to prevent runaway execution.
///
/// §Depth limiting:
///   Each `DelegationConfig` specifies `max_depth`.  When a parent at depth D
///   spawns a child, the child runs at depth D+1.  If D+1 > max_depth, the
///   call returns `error.MaxDepthExceeded` rather than spawning.
///
/// §Iteration budget:
///   `iteration_budget` limits the number of tool-call iterations the child
///   may perform.  This prevents a delegated sub-task from consuming unbounded
///   LLM calls.
///
/// §Isolation:
///   Children receive an explicit `child_toolsets` allowlist.  They do NOT
///   inherit the parent's full tool registry.  Memory is NOT inherited by
///   default — children start with a clean context.
const std = @import("std");
const Allocator = std.mem.Allocator;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Manages delegation configuration with fixed buffers; encapsulates ownership and invariants.
pub const DelegationConfig = struct {
    /// Maximum recursion depth for child agents (0 = no children allowed).
    max_depth: u8 = 2,
    /// Toolset names available to the child agent.
    child_toolsets: []const []const u8 = &[_][]const u8{},
    /// Maximum tool-call iterations for the child agent.
    iteration_budget: usize = 50,
    /// Whether the child inherits the parent's memory context.
    inherit_memory: bool = false,
};

/// Represents a delegation outcome with ownership and invariants; manages state internally.
pub const DelegationResult = struct {
    /// The child agent's final response text.
    response: []const u8,
    /// Number of iterations the child used.
    iterations_used: usize,
    /// Depth at which this child ran.
    depth: u8,
};

// ---------------------------------------------------------------------------
// Delegation
// ---------------------------------------------------------------------------

pub const Delegation = struct {
    const Self = @This();

    /// Attempt to delegate `task` to a child agent at `current_depth + 1`.
    ///
    /// `runner` must implement:
    ///   `run(arena: Allocator, task: []const u8, config: DelegationConfig, depth: u8) !DelegationResult`
    ///
    /// Returns `error.MaxDepthExceeded` if spawning would exceed `config.max_depth`.
    /// Returns `error.BudgetExhausted` if the child uses all iterations.
    pub fn delegate(
        arena: Allocator,
        current_depth: u8,
        task: []const u8,
        config: DelegationConfig,
        runner: anytype,
    ) !DelegationResult {
        if (current_depth >= config.max_depth) return error.MaxDepthExceeded;
        const child_depth = current_depth + 1;
        return runner.run(arena, task, config, child_depth);
    }
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

const MockRunner = struct {
    iterations: usize = 3,

    pub fn run(self: @This(), arena: Allocator, task: []const u8, config: DelegationConfig, depth: u8) !DelegationResult {
        _ = config;
        if (self.iterations == 0) return error.BudgetExhausted;
        const resp = try std.fmt.allocPrint(arena, "done:{s}@d{}", .{ task, depth });
        return DelegationResult{
            .response = resp,
            .iterations_used = self.iterations,
            .depth = depth,
        };
    }
};

test "Delegation: spawns child at depth+1" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const result = try Delegation.delegate(arena.allocator(), 0, "subtask", .{ .max_depth = 2 }, MockRunner{});
    try testing.expectEqual(@as(u8, 1), result.depth);
    try testing.expectEqualStrings("done:subtask@d1", result.response);
}

test "Delegation: returns MaxDepthExceeded when at limit" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    try testing.expectError(
        error.MaxDepthExceeded,
        Delegation.delegate(arena.allocator(), 2, "task", .{ .max_depth = 2 }, MockRunner{}),
    );
}

test "Delegation: max_depth=0 prevents any delegation" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    try testing.expectError(
        error.MaxDepthExceeded,
        Delegation.delegate(arena.allocator(), 0, "task", .{ .max_depth = 0 }, MockRunner{}),
    );
}

test "Delegation: runner can return BudgetExhausted" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    try testing.expectError(
        error.BudgetExhausted,
        Delegation.delegate(arena.allocator(), 0, "task", .{ .max_depth = 2 }, MockRunner{ .iterations = 0 }),
    );
}

test "DelegationConfig: defaults are sane" {
    const cfg = DelegationConfig{};
    try testing.expectEqual(@as(u8, 2), cfg.max_depth);
    try testing.expectEqual(@as(usize, 50), cfg.iteration_budget);
    try testing.expect(!cfg.inherit_memory);
}
