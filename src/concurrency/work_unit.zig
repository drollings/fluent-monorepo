//! work_unit.zig — Type-erased work unit with per-unit arena (WVR pattern).
//!
//! This is the Zig equivalent of a Go closure passed to a goroutine: the
//! Handler struct is the captured state, and toAny() type-erases it for
//! submission to any ExecutionBackend.
//!
//! ## Usage
//!
//!   const TargetHandler = struct {
//!       target:    *const Target,
//!       result_ch: *zio.Channel(DagResult),
//!
//!       pub fn execute(self: *TargetHandler, arena: std.mem.Allocator) !void {
//!           const result = try runTarget(self.target, arena);
//!           try self.result_ch.send(result);
//!       }
//!   };
//!
//!   const unit = try WorkUnit(TargetHandler).init(allocator, .{
//!       .target    = my_target,
//!       .result_ch = &result_ch,
//!   });
//!   try backend.submit(unit.toAny());
//!
//! ## Ownership
//!
//! After toAny() the caller must not access the pointer.
//! runFn and deinitFn both free the struct and deinit the per-unit arena
//! on every exit path (success, error, cancellation).
//!
//! ## Cancellation
//!
//! runFn calls zio.checkCancel() before execute() — a unit whose enclosing
//! Group has been cancelled is silently skipped without entering the handler.
//! checkCancel() is a no-op outside a zio runtime, so SyncBackend is safe.
//!
//! Handlers do NOT need to call zio.checkCancel() themselves unless they want
//! mid-handler yield points (e.g. after a slow allocation step).

const std = @import("std");
const zio = @import("zio");

/// Type-erased 3-pointer work handle.  Size invariant: exactly 3 pointers.
pub const AnyWorkUnit = struct {
    ptr: *anyopaque,
    /// Execute handler, deinit arena, destroy self.  Called at most once.
    runFn: *const fn (ptr: *anyopaque) anyerror!void,
    /// Deinit arena and destroy self without executing.  Called when the
    /// unit is discarded before execution — e.g. after ZioBackend
    /// cancellation or a failed group.spawn().
    deinitFn: *const fn (ptr: *anyopaque) void,
};

comptime {
    std.debug.assert(@sizeOf(AnyWorkUnit) == 3 * @sizeOf(*anyopaque));
}

/// Typed wrapper over a `Handler`.
///
/// Handler contract:
///   pub fn execute(self: *Handler, arena: std.mem.Allocator) anyerror!void
///
/// The arena is valid for the duration of execute() only.
/// If execute() allocates data that must outlive itself, write it to a
/// Channel or a caller-owned pointer in the handler struct before returning.
pub fn WorkUnit(comptime Handler: type) type {
    return struct {
        const Self = @This();

        parent: std.mem.Allocator,
        arena: std.heap.ArenaAllocator,
        handler: Handler,

        /// Heap-allocate and initialise.  Ownership transfers to the
        /// AnyWorkUnit returned by toAny(); do not retain the pointer.
        pub fn init(allocator: std.mem.Allocator, handler: Handler) !*Self {
            const self = try allocator.create(Self);
            self.* = .{
                .parent = allocator,
                .arena = std.heap.ArenaAllocator.init(allocator),
                .handler = handler,
            };
            return self;
        }

        /// Produce a type-erased handle.  After this call the caller
        /// must not access `self`.
        pub fn toAny(self: *Self) AnyWorkUnit {
            return .{ .ptr = self, .runFn = runFn, .deinitFn = deinitFn };
        }

        fn runFn(ptr: *anyopaque) anyerror!void {
            const self: *Self = @ptrCast(@alignCast(ptr));
            const parent = self.parent;
            defer {
                self.arena.deinit();
                parent.destroy(self);
            }
            // Dead-on-arrival check: skip the handler if the enclosing Group
            // was cancelled before this unit was dequeued.  Outside a zio
            // runtime (e.g. SyncBackend) checkCancel() is a no-op.
            try zio.checkCancel();
            try self.handler.execute(self.arena.allocator());
        }

        fn deinitFn(ptr: *anyopaque) void {
            const self: *Self = @ptrCast(@alignCast(ptr));
            const parent = self.parent;
            self.arena.deinit();
            parent.destroy(self);
        }
    };
}

// ─── Tests ────────────────────────────────────────────────────────────────────

const testing = std.testing;

const BoolHandler = struct {
    executed: *bool,
    should_error: bool = false,

    pub fn execute(self: *BoolHandler, arena: std.mem.Allocator) !void {
        _ = arena;
        self.executed.* = true;
        if (self.should_error) return error.BoolHandlerFailed;
    }
};

test "WorkUnit: runFn executes handler" {
    var ok = false;
    const unit = try WorkUnit(BoolHandler).init(testing.allocator, .{ .executed = &ok });
    const any = unit.toAny();
    try any.runFn(any.ptr);
    try testing.expect(ok);
}

test "WorkUnit: runFn propagates handler error" {
    var ok = false;
    const unit = try WorkUnit(BoolHandler).init(testing.allocator, .{ .executed = &ok, .should_error = true });
    const any = unit.toAny();
    try testing.expectError(error.BoolHandlerFailed, any.runFn(any.ptr));
    try testing.expect(ok); // handler still ran
}

test "WorkUnit: deinitFn cleans up without executing" {
    var ok = false;
    const unit = try WorkUnit(BoolHandler).init(testing.allocator, .{ .executed = &ok });
    const any = unit.toAny();
    any.deinitFn(any.ptr);
    try testing.expect(!ok);
}

test "WorkUnit: GPA no leaks — success / error / deinit paths" {
    // success
    {
        var gpa: std.heap.DebugAllocator(.{}) = .init;
        defer if (gpa.deinit() == .leak) @panic("leak: success");
        var ok = false;
        const u = try WorkUnit(BoolHandler).init(gpa.allocator(), .{ .executed = &ok });
        const any = u.toAny();
        _ = any.runFn(any.ptr) catch {};
    }
    // error
    {
        var gpa: std.heap.DebugAllocator(.{}) = .init;
        defer if (gpa.deinit() == .leak) @panic("leak: error");
        var ok = false;
        const u = try WorkUnit(BoolHandler).init(gpa.allocator(), .{ .executed = &ok, .should_error = true });
        const any = u.toAny();
        _ = any.runFn(any.ptr) catch {};
    }
    // deinit
    {
        var gpa: std.heap.DebugAllocator(.{}) = .init;
        defer if (gpa.deinit() == .leak) @panic("leak: deinit");
        var ok = false;
        const u = try WorkUnit(BoolHandler).init(gpa.allocator(), .{ .executed = &ok });
        const any = u.toAny();
        any.deinitFn(any.ptr);
    }
}

test "WorkUnit: checkCancel is a no-op outside zio runtime — handler still runs" {
    // Outside a zio runtime checkCancel() returns immediately without error.
    // This test runs in the standard test harness (no zio Runtime), so the
    // pre-execution check must not prevent the handler from executing.
    var ok = false;
    const unit = try WorkUnit(BoolHandler).init(testing.allocator, .{ .executed = &ok });
    const any = unit.toAny();
    try any.runFn(any.ptr);
    try testing.expect(ok);
}

test "WorkUnit: arena is freed — alloc inside execute does not leak" {
    const ArenaHandler = struct {
        counter: *usize,

        pub fn execute(self: *@This(), arena: std.mem.Allocator) !void {
            _ = try arena.alloc(u8, 256); // arena-owned; freed with the unit
            self.counter.* += 1;
        }
    };

    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak: arena alloc");
    var count: usize = 0;
    const unit = try WorkUnit(ArenaHandler).init(gpa.allocator(), .{ .counter = &count });
    const any = unit.toAny();
    try any.runFn(any.ptr);
    try testing.expectEqual(@as(usize, 1), count);
}
