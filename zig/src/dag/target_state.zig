//! target_state.zig — Execution-only view of a Target for a single DAG run.
//!
//! TargetState is ephemeral: one instance per Target per run.  It tracks
//! execution status without duplicating the structural metadata (name, deps,
//! commands) that lives in the Target.
//!
//! ## Relationship to Target
//!
//!   Target        — structural, persistent, owned by the DAG graph
//!   TargetState   — runtime, ephemeral, owned by the DAG walker for one run
//!
//! ## Usage (DAG walker sketch)
//!
//!   var states = try allocator.alloc(TargetState, targets.len);
//!   for (targets, 0..) |*t, i| states[i] = TargetState.init(t);
//!
//!   // Mark a target ready when all deps complete:
//!   states[i].transition(.running);
//!
//!   // On completion:
//!   states[i].transition(.succeeded);  // or .failed

const std = @import("std");
const Target = @import("target.zig");

pub const Status = enum {
    /// Waiting for dependencies.
    pending,
    /// Semaphore acquired; handler executing.
    running,
    /// Handler returned without error.
    succeeded,
    /// Handler returned an error.
    failed,
    /// Cancelled before execution began.
    skipped,
};

pub const TargetState = struct {
    target: *const Target,
    status: Status = .pending,

    pub fn init(target: *const Target) TargetState {
        return .{ .target = target };
    }

    /// Transition to `next`.  Panics on invalid transitions in debug builds.
    pub fn transition(self: *TargetState, next: Status) void {
        if (std.debug.runtime_safety) {
            const valid = switch (self.status) {
                .pending => next == .running or next == .skipped,
                .running => next == .succeeded or next == .failed,
                .succeeded => false,
                .failed => false,
                .skipped => false,
            };
            if (!valid) std.debug.panic(
                "TargetState: invalid transition {s} → {s}",
                .{ @tagName(self.status), @tagName(next) },
            );
        }
        self.status = next;
    }

    pub fn isDone(self: TargetState) bool {
        return switch (self.status) {
            .succeeded, .failed, .skipped => true,
            else => false,
        };
    }
};

// ─── Tests ────────────────────────────────────────────────────────────────────

const testing = std.testing;

test "TargetState: initial status is pending" {
    // Minimal Target stub — only bit_index and name are needed for this test.
    var t = std.mem.zeroes(Target);
    const s = TargetState.init(&t);
    try testing.expectEqual(Status.pending, s.status);
}

test "TargetState: transition pending→running→succeeded" {
    var t = std.mem.zeroes(Target);
    var s = TargetState.init(&t);
    s.transition(.running);
    try testing.expectEqual(Status.running, s.status);
    s.transition(.succeeded);
    try testing.expectEqual(Status.succeeded, s.status);
    try testing.expect(s.isDone());
}

test "TargetState: transition pending→skipped is done" {
    var t = std.mem.zeroes(Target);
    var s = TargetState.init(&t);
    s.transition(.skipped);
    try testing.expect(s.isDone());
}

test "TargetState: transition pending→running→failed is done" {
    var t = std.mem.zeroes(Target);
    var s = TargetState.init(&t);
    s.transition(.running);
    s.transition(.failed);
    try testing.expect(s.isDone());
}
