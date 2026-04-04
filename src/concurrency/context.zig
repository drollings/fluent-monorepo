//! context.zig — Cancellation and deadline propagation (M11).
//!
//! `Context` is a 24-byte, stack-allocatable struct with an atomic cancellation
//! flag and an optional absolute deadline.  No heap allocation, no destructor.
//!
//! ## Ownership
//!
//! The caller owns the Context and must keep it alive until all work units
//! that hold a `*const Context` pointer to it have completed.
//!
//! ## Parent–child cancellation
//!
//! There is no registration tree.  Pass the parent's `*Context` pointer to child
//! work units that should observe parent cancellation.  `withTimeout` and
//! `withDeadline` derive an independently cancellable context with a stricter
//! deadline — they do not inherit the parent's flag automatically.
//!
//! ## Thread safety
//!
//! `isCancelled`, `isExpired`, and `cancel` are safe to call from multiple
//! threads concurrently.  First call to `cancel` wins; subsequent calls are no-ops.

const std = @import("std");

/// Manages shared context across threads with ownership and lifecycle control; ensures safe access patterns.
pub const Context = struct {
    /// Bit 0: cancelled flag.  Bits 1–16: u16 error code from `anyerror`.
    state: std.atomic.Value(u32),
    /// Absolute deadline as `std.time.nanoTimestamp()` (i64), or null.
    /// i64 covers deadlines until year ~2262 — sufficient for any timeout.
    deadline: ?i64,

    /// Root context: never cancelled, no deadline.  Stack-allocate this.
    pub fn background() Context {
        return .{
            .state = std.atomic.Value(u32).init(0),
            .deadline = null,
        };
    }

    /// Derive a context that expires `duration_ns` nanoseconds from now.
    /// Does not inherit cancellation from a parent — pass the parent's pointer
    /// to child work units that should observe parent cancellation.
    pub fn withTimeout(duration_ns: u64) Context {
        // Add in i128 to avoid overflow, then truncate to i64 (safe for < ~292 year timeouts).
        const deadline: i64 = @truncate(std.time.nanoTimestamp() + @as(i128, duration_ns));
        return .{
            .state = std.atomic.Value(u32).init(0),
            .deadline = deadline,
        };
    }

    /// Derive a context with an absolute nanoTimestamp deadline.
    pub fn withDeadline(deadline_ns: i64) Context {
        return .{
            .state = std.atomic.Value(u32).init(0),
            .deadline = deadline_ns,
        };
    }

    /// Returns true if explicitly cancelled or the deadline has passed.
    pub fn isCancelled(self: *const Context) bool {
        if (self.state.load(.acquire) & 0x1 != 0) return true;
        return self.isExpired();
    }

    /// Returns true only if the deadline has passed.  Does not check the flag.
    pub fn isExpired(self: *const Context) bool {
        const d = self.deadline orelse return false;
        const now: i64 = @truncate(std.time.nanoTimestamp());
        return now >= d;
    }

    /// Returns the cancellation reason, or null if not explicitly cancelled.
    ///
    /// If the cancellation flag is set and the deadline is also expired,
    /// returns `error.DeadlineExceeded`.  Otherwise returns the stored error.
    ///
    /// Note: returns null if only the deadline fired (not explicitly cancelled).
    /// Use `isCancelled()` to check both flags.
    pub fn err(self: *const Context) ?anyerror {
        const s = self.state.load(.acquire);
        if (s & 0x1 == 0) return null;
        if (self.isExpired()) return error.DeadlineExceeded;
        const code: u16 = @intCast((s >> 1) & 0xFFFF);
        return if (code == 0) error.Cancelled else @errorFromInt(code);
    }

    /// Set the cancellation flag.  First caller wins; subsequent calls are no-ops.
    /// Safe to call from any thread.
    pub fn cancel(self: *Context, reason: anyerror) void {
        const code: u16 = @intFromError(reason);
        const new_state: u32 = (@as(u32, code) << 1) | 0x1;
        // cmpxchgStrong: only write if not already cancelled (current == 0).
        // Returns null on success, actual value on failure — both paths discard.
        _ = self.state.cmpxchgStrong(0, new_state, .release, .monotonic);
    }
};

// ── Size invariant ────────────────────────────────────────────────────────────

comptime {
    std.debug.assert(@sizeOf(Context) <= 24);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

const testing = std.testing;

test "Context: background is not cancelled" {
    const ctx = Context.background();
    try testing.expect(!ctx.isCancelled());
    try testing.expect(!ctx.isExpired());
    try testing.expectEqual(@as(?anyerror, null), ctx.err());
}

test "Context: cancel sets isCancelled and err" {
    var ctx = Context.background();
    ctx.cancel(error.TestError);
    try testing.expect(ctx.isCancelled());
    const e = ctx.err() orelse return error.ExpectedError;
    try testing.expectEqual(error.TestError, e);
}

test "Context: first cancel wins, second is ignored" {
    var ctx = Context.background();
    ctx.cancel(error.TestError);
    ctx.cancel(error.AnotherError);
    // First error must be preserved.
    const e = ctx.err() orelse return error.ExpectedError;
    try testing.expectEqual(error.TestError, e);
}

test "Context: withTimeout 1ns deadline is immediately expired" {
    // 1 ns deadline; by the time isExpired() is called, time has advanced.
    const ctx = Context.withTimeout(1);
    // Spin briefly to ensure at least 1 ns has passed.
    std.atomic.spinLoopHint();
    try testing.expect(ctx.isExpired());
    try testing.expect(ctx.isCancelled());
}

test "Context: withTimeout large deadline is not expired" {
    const ctx = Context.withTimeout(60 * std.time.ns_per_s); // 60 seconds
    try testing.expect(!ctx.isExpired());
    try testing.expect(!ctx.isCancelled());
}

test "Context: withDeadline past deadline is expired" {
    const past: i64 = @truncate(std.time.nanoTimestamp() - 1_000_000);
    const ctx = Context.withDeadline(past);
    try testing.expect(ctx.isExpired());
    try testing.expect(ctx.isCancelled());
}

test "Context: withDeadline future deadline is not expired" {
    const future: i64 = @truncate(std.time.nanoTimestamp() + 60 * std.time.ns_per_s);
    const ctx = Context.withDeadline(future);
    try testing.expect(!ctx.isExpired());
    try testing.expect(!ctx.isCancelled());
}

test "Context: err returns null when only deadline expired (not explicitly cancelled)" {
    // Deadline expired but no explicit cancel — err() returns null, isCancelled() returns true.
    const ctx = Context.withTimeout(1);
    std.atomic.spinLoopHint();
    try testing.expect(ctx.isCancelled()); // deadline
    try testing.expectEqual(@as(?anyerror, null), ctx.err()); // not explicitly cancelled
}

test "Context: cancel then err returns DeadlineExceeded if also expired" {
    var ctx = Context.withTimeout(1); // will expire
    std.atomic.spinLoopHint();
    ctx.cancel(error.SomeError);
    // Both expired and cancelled: err() returns DeadlineExceeded.
    const e = ctx.err() orelse return error.ExpectedError;
    try testing.expectEqual(error.DeadlineExceeded, e);
}

test "Context: thread safety — cancel from two threads, first wins" {
    var ctx = Context.background();

    const T = struct {
        fn cancelWith(c: *Context, e: anyerror) void {
            c.cancel(e);
        }
    };

    const t1 = try std.Thread.spawn(.{}, T.cancelWith, .{ &ctx, error.ThreadA });
    const t2 = try std.Thread.spawn(.{}, T.cancelWith, .{ &ctx, error.ThreadB });
    t1.join();
    t2.join();

    // Either thread's error is valid; the point is exactly one wins.
    try testing.expect(ctx.isCancelled());
    const maybe_e = ctx.err();
    try testing.expect(maybe_e != null);
    const e = maybe_e.?;
    try testing.expect(e == error.ThreadA or e == error.ThreadB);
}

test "Context: GPA no leaks — background, withTimeout, cancel" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("memory leak");
    _ = gpa.allocator();

    var ctx = Context.background();
    ctx.cancel(error.Cancelled);
    try testing.expect(ctx.isCancelled());

    const ctx2 = Context.withTimeout(60 * std.time.ns_per_s);
    try testing.expect(!ctx2.isExpired());
}

