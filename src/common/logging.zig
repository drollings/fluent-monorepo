//! logging.zig — Structured logging context and timing scope for Fluent WEAVER.
//!
//! Request-scoped structured logging with timing and correlation.
//! For global log configuration and output, see common.log.Logger.
//!
//! ## Design
//!
//! `LogContext` is a thread-local struct that propagates request-scoped metadata
//! (request_id, user_id, trace_id, span_id) through a call chain without threading
//! the context through every function signature.
//!
//! `Scope` wraps a named operation: it logs start (with context) and end (with
//! duration) via `std.log.debug`. When context is null, no logging occurs — zero
//! overhead in production with filters set above `debug`.
//!
//! `callLogged` wraps a single function call in a Scope:
//!
//!   const result = try callLogged("embed", embedder.embed, .{ allocator, text });
//!
//! For manual scope management:
//!
//!   const scope = Scope.begin("embed");
//!   defer scope.end();
//!   const result = try embedder.embed(allocator, text);
//!
//! ## Thread Safety
//!
//! `LogContext.current` is `threadlocal` — each OS thread has its own copy.
//! No synchronization required. Set context at the entry point of a request
//! handler and clear it on exit:
//!
//!   LogContext.set(.{ .request_id = "req-123" });
//!   defer LogContext.clear();
//!
//! Context does NOT propagate across thread boundaries automatically. When
//! spawning a worker thread, pass the context value explicitly and call
//! `LogContext.set()` on the new thread.

const std = @import("std");

// ── LogContext ────────────────────────────────────────────────────────────────

pub const LogContext = struct {
    request_id: ?[]const u8 = null,
    user_id: ?[]const u8 = null,
    trace_id: ?[]const u8 = null,
    span_id: ?[]const u8 = null,

    /// Per-thread active context.  Null when no context is active.
    threadlocal var current: ?LogContext = null;

    /// Install `ctx` as the active context for the current thread.
    pub fn set(ctx: LogContext) void {
        current = ctx;
    }

    /// Return the active context for the current thread, or null.
    pub fn get() ?LogContext {
        return current;
    }

    /// Clear the active context for the current thread.
    pub fn clear() void {
        current = null;
    }

    /// `std.fmt` format support via `{f}` specifier.
    /// Produces `[req=<id> user=<id> trace=<id> span=<id>]`.
    /// Only non-null fields are emitted.
    pub fn format(self: LogContext, writer: anytype) !void {
        try writer.writeByte('[');
        var wrote_any = false;
        if (self.request_id) |id| {
            try writer.print("req={s}", .{id});
            wrote_any = true;
        }
        if (self.user_id) |id| {
            if (wrote_any) try writer.writeByte(' ');
            try writer.print("user={s}", .{id});
            wrote_any = true;
        }
        if (self.trace_id) |id| {
            if (wrote_any) try writer.writeByte(' ');
            try writer.print("trace={s}", .{id});
            wrote_any = true;
        }
        if (self.span_id) |id| {
            if (wrote_any) try writer.writeByte(' ');
            try writer.print("span={s}", .{id});
        }
        try writer.writeByte(']');
    }
};

// ── Scope ─────────────────────────────────────────────────────────────────────

/// Manages scope boundaries with fixed-size buffers; owned by the module; ensures safe initialization/deinit.
pub const Scope = struct {
    name: []const u8,
    start_ns: i96,
    has_context: bool,

    fn nanoNow() i96 {
        return std.Io.Timestamp.now(std.Io.Threaded.global_single_threaded.io(), .real).nanoseconds;
    }

    /// Begin a named scope.  Logs start if context is active.
    pub fn begin(name: []const u8) Scope {
        const ctx = LogContext.get();
        const s = Scope{
            .name = name,
            .start_ns = nanoNow(),
            .has_context = ctx != null,
        };
        if (ctx) |c| {
            std.log.debug("{s}: start {f}", .{ name, c });
        }
        return s;
    }

    /// End the scope.  Logs duration (µs) if context was active at begin.
    pub fn end(self: Scope) void {
        if (!self.has_context) return;
        const elapsed_us = @divTrunc(nanoNow() - self.start_ns, 1000);
        const ctx = LogContext.get();
        if (ctx) |c| {
            std.log.debug("{s}: end duration={d}µs {f}", .{ self.name, elapsed_us, c });
        } else {
            std.log.debug("{s}: end duration={d}µs", .{ self.name, elapsed_us });
        }
    }
};

// ── callLogged ────────────────────────────────────────────────────────────────

/// Converts a Zig array to a function call signature, returning its type info.
pub inline fn callLogged(
    comptime name: []const u8,
    func: anytype,
    args: anytype,
) @typeInfo(@TypeOf(func)).@"fn".return_type.? {
    const scope = Scope.begin(name);
    defer scope.end();
    return @call(.auto, func, args);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

const testing = std.testing;

test "LogContext: set and get" {
    LogContext.set(.{ .request_id = "req-abc", .user_id = "u1" });
    defer LogContext.clear();

    const ctx = LogContext.get();
    try testing.expect(ctx != null);
    try testing.expectEqualStrings("req-abc", ctx.?.request_id.?);
    try testing.expectEqualStrings("u1", ctx.?.user_id.?);
}

test "LogContext: clear removes context" {
    LogContext.set(.{ .request_id = "req-xyz" });
    LogContext.clear();
    try testing.expectEqual(@as(?LogContext, null), LogContext.get());
}

test "LogContext: null by default (fresh thread)" {
    // This test runs in the same thread as others but relies on isolation via
    // clear() at the end of each test.  Verify clean state after prior clears.
    LogContext.clear(); // ensure clean slate
    try testing.expectEqual(@as(?LogContext, null), LogContext.get());
}

test "LogContext: format with all fields" {
    const ctx = LogContext{
        .request_id = "req-1",
        .user_id = "user-2",
        .trace_id = "trace-3",
        .span_id = "span-4",
    };
    var buf: [256]u8 = undefined;
    const s = try std.fmt.bufPrint(&buf, "{f}", .{ctx});
    try testing.expect(std.mem.indexOf(u8, s, "req=req-1") != null);
    try testing.expect(std.mem.indexOf(u8, s, "user=user-2") != null);
    try testing.expect(std.mem.indexOf(u8, s, "trace=trace-3") != null);
    try testing.expect(std.mem.indexOf(u8, s, "span=span-4") != null);
}

test "LogContext: format with partial fields" {
    const ctx = LogContext{ .request_id = "req-only" };
    var buf: [128]u8 = undefined;
    const s = try std.fmt.bufPrint(&buf, "{f}", .{ctx});
    try testing.expect(std.mem.indexOf(u8, s, "req=req-only") != null);
    try testing.expect(std.mem.indexOf(u8, s, "user=") == null);
}

test "LogContext: format with no fields" {
    const ctx = LogContext{};
    var buf: [64]u8 = undefined;
    const s = try std.fmt.bufPrint(&buf, "{f}", .{ctx});
    try testing.expectEqualStrings("[]", s);
}

test "callLogged: wraps plain function" {
    const add = struct {
        fn f(a: i32, b: i32) i32 {
            return a + b;
        }
    }.f;

    LogContext.set(.{ .request_id = "req-cl" });
    defer LogContext.clear();

    const result = callLogged("add", add, .{ 3, 4 });
    try testing.expectEqual(@as(i32, 7), result);
}

test "callLogged: wraps error-returning function" {
    const mayFail = struct {
        fn f(fail: bool) !i32 {
            if (fail) return error.Oops;
            return 42;
        }
    }.f;

    LogContext.set(.{ .request_id = "req-err" });
    defer LogContext.clear();

    const ok = try callLogged("mayFail_ok", mayFail, .{false});
    try testing.expectEqual(@as(i32, 42), ok);

    const err = callLogged("mayFail_err", mayFail, .{true});
    try testing.expectError(error.Oops, err);
}

test "LogContext: GPA no leaks" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("memory leak");

    // LogContext holds no allocated memory — just slices pointing elsewhere.
    LogContext.set(.{ .request_id = "req-gpa", .trace_id = "t1" });
    const ctx = LogContext.get();
    try testing.expect(ctx != null);
    LogContext.clear();
}
