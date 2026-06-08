//! guardrails.zig — Loop detection, failure limits, and no-progress detection.
//!
//! Uses common/hash.fnv1a64 for output hashing to detect when the subagent
//! is making no progress. Tracks consecutive failures per tool type and
//! overall iteration count to prevent infinite loops.

const std = @import("std");
const types = @import("types.zig");
const hash_mod = @import("common").hash;

pub const GuardrailCheck = enum {
    ok,
    failure_limit,
    no_progress_limit,
    iteration_limit,
    escalation,
};

pub fn checkGuardrails(
    guardrails: *types.GuardrailState,
    result: *const types.ToolResult,
    iteration: u16,
    max_iterations: u16,
) GuardrailCheck {
    if (iteration >= max_iterations) return .iteration_limit;

    if (!result.success) {
        const key = @tagName(result.action);
        if (guardrails.recordFailure(key) != null) {
            return .failure_limit;
        }
    } else {
        const key = @tagName(result.action);
        guardrails.clearFailure(key);
    }

    if (hasNoProgress(result)) {
        const key = "global";
        if (guardrails.recordNoProgress(key)) {
            return .no_progress_limit;
        }
    }

    return .ok;
}

pub fn hasNoProgress(result: *const types.ToolResult) bool {
    const raw = result.raw orelse return false;
    if (raw.len == 0) return true;
    const hash = hash_mod.fnv1a64(raw);
    return hash == 0;
}

pub const OutputHashTracker = struct {
    prev_hashes: std.AutoHashMap(u64, usize),
    allocator: std.mem.Allocator,

    pub fn init(allocator: std.mem.Allocator) OutputHashTracker {
        return .{
            .prev_hashes = std.AutoHashMap(u64, usize).init(allocator),
            .allocator = allocator,
        };
    }

    pub fn deinit(self: *OutputHashTracker) void {
        self.prev_hashes.deinit();
    }

    pub fn recordAndCheck(self: *OutputHashTracker, result: *const types.ToolResult) bool {
        const raw = result.raw orelse return false;
        const h = hash_mod.fnv1a64(raw);
        const entry = self.prev_hashes.getOrPut(h) catch return false;
        if (entry.found_existing) {
            entry.value_ptr.* += 1;
            return entry.value_ptr.* >= 3;
        } else {
            entry.value_ptr.* = 1;
            return false;
        }
    }

    pub fn reset(self: *OutputHashTracker) void {
        self.prev_hashes.clearAndFree();
    }
};

const testing = std.testing;

test "checkGuardrails: ok for successful result" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var gs = types.GuardrailState.init(allocator, 5, 5, 20);
    defer gs.deinit();

    const result: types.ToolResult = .{ .action = .bash, .success = true, .raw = "ok" };
    const check = checkGuardrails(&gs, &result, 1, 20);
    try testing.expectEqual(GuardrailCheck.ok, check);
}

test "checkGuardrails: iteration limit" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var gs = types.GuardrailState.init(allocator, 5, 5, 3);
    defer gs.deinit();

    const result: types.ToolResult = .{ .action = .bash, .success = true, .raw = "ok" };
    const check = checkGuardrails(&gs, &result, 3, 3);
    try testing.expectEqual(GuardrailCheck.iteration_limit, check);
}

test "checkGuardrails: failure limit" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var gs = types.GuardrailState.init(allocator, 2, 5, 20);
    defer gs.deinit();

    const fail1: types.ToolResult = .{ .action = .bash, .success = false, .raw = "fail" };
    _ = checkGuardrails(&gs, &fail1, 1, 20);
    const fail2: types.ToolResult = .{ .action = .bash, .success = false, .raw = "fail" };
    const check = checkGuardrails(&gs, &fail2, 2, 20);
    try testing.expectEqual(GuardrailCheck.failure_limit, check);
}

test "OutputHashTracker: detects repeated output" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var tracker = OutputHashTracker.init(allocator);
    defer tracker.deinit();

    const result: types.ToolResult = .{ .action = .bash, .success = true, .raw = "same output" };
    try testing.expect(!tracker.recordAndCheck(&result));
    try testing.expect(!tracker.recordAndCheck(&result));
    try testing.expect(tracker.recordAndCheck(&result));
}

test "OutputHashTracker: different outputs are fine" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var tracker = OutputHashTracker.init(allocator);
    defer tracker.deinit();

    const r1: types.ToolResult = .{ .action = .bash, .success = true, .raw = "output 1" };
    const r2: types.ToolResult = .{ .action = .bash, .success = true, .raw = "output 2" };
    try testing.expect(!tracker.recordAndCheck(&r1));
    try testing.expect(!tracker.recordAndCheck(&r2));
}
