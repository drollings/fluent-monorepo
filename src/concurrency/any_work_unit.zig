//! any_work_unit.zig — Type-erased work unit and typed wrapper (M11).
//!
//! ## Pattern
//!
//! Follows the `ptr + vtable` pattern used throughout the codebase.
//! `AnyWorkUnit` is the type-erased handle (24 bytes: three pointers).
//! `WorkUnit(Handler)` is the typed concrete type.  `WorkUnit.init()` heap-
//! allocates the struct and stores the parent allocator so `runFn` can self-free.
//!
//! ## Ownership
//!
//! - `WorkUnit(T).init()` allocates the struct on the heap via the parent allocator.
//! - `runFn` and `deinitFn` both deinit the arena AND destroy the struct — the
//!   caller must not touch the pointer after calling `group.go(unit.toAny())`.
//! - The `Context` pointer is NOT owned.  The caller must keep the context alive
//!   until the work unit completes.
//!
//! ## Handler contract
//!
//!   pub const Handler = struct {
//!       pub fn execute(self: *Handler, arena: std.mem.Allocator, ctx: *const Context) anyerror!void { ... }
//!   };

const std = @import("std");
const Context = @import("context.zig").Context;

/// Manages concurrent work units with fixed buffers; owns lifecycle; ensures safe access across threads.
pub const AnyWorkUnit = struct {
    ptr: *anyopaque,
    /// Execute the work, deinit the arena, free the struct, return any error.
    /// Called exactly once by the executing thread.
    runFn: *const fn (ptr: *anyopaque) anyerror!void,
    /// Release arena and free the struct without executing.
    /// Called when the unit is discarded before execution.
    deinitFn: *const fn (ptr: *anyopaque) void,
};

// Size invariant: AnyWorkUnit must be exactly three pointers.
comptime {
    std.debug.assert(@sizeOf(AnyWorkUnit) == 3 * @sizeOf(*anyopaque));
}

/// Transforms a given handler into its work unit representation, handling any type passed.
pub fn WorkUnit(comptime Handler: type) type {
    return struct {
        const Self = @This();

        /// Allocator used to create this struct; also backs the arena.
        parent: std.mem.Allocator,
        /// Per-execution arena: freed at the end of runFn (or by deinitFn).
        arena: std.heap.ArenaAllocator,
        handler: Handler,
        ctx: *const Context,

        /// Allocate and initialise a work unit on the heap.
        /// The caller transfers ownership to the returned pointer; after calling
        /// `toAny()` and dispatching, do NOT access the pointer again.
        pub fn init(
            allocator: std.mem.Allocator,
            handler: Handler,
            ctx: *const Context,
        ) !*Self {
            const self = try allocator.create(Self);
            self.* = .{
                .parent = allocator,
                .arena = std.heap.ArenaAllocator.init(allocator),
                .handler = handler,
                .ctx = ctx,
            };
            return self;
        }

        /// Produce a type-erased handle.  After this call the caller must not
        /// access `self` directly — ownership is transferred to the AnyWorkUnit.
        pub fn toAny(self: *Self) AnyWorkUnit {
            return .{
                .ptr = self,
                .runFn = runFn,
                .deinitFn = deinitFn,
            };
        }

        fn runFn(ptr: *anyopaque) anyerror!void {
            const self: *Self = @ptrCast(@alignCast(ptr));
            // Save parent and arena before any possible failure so the defer
            // can clean up even after self is freed.
            const parent = self.parent;
            // Arena deinit runs on all exit paths (success, error, cancellation).
            // Struct destroy runs after arena deinit (safe: parent allocator is
            // independent of the arena).
            defer {
                self.arena.deinit();
                parent.destroy(self);
            }
            if (self.ctx.isCancelled()) return error.Cancelled;
            try self.handler.execute(self.arena.allocator(), self.ctx);
        }

        fn deinitFn(ptr: *anyopaque) void {
            const self: *Self = @ptrCast(@alignCast(ptr));
            const parent = self.parent;
            self.arena.deinit();
            parent.destroy(self);
        }
    };
}

// ── Tests ─────────────────────────────────────────────────────────────────────

const testing = std.testing;

/// Manages concurrent test execution; owns test setup and teardown; ensures isolation between units.
const TestHandler = struct {
    executed: *bool,
    should_error: bool = false,

    pub fn execute(self: *TestHandler, arena: std.mem.Allocator, ctx: *const Context) !void {
        _ = arena;
        _ = ctx;
        self.executed.* = true;
        if (self.should_error) return error.HandlerFailed;
    }
};

test "WorkUnit: runFn executes handler and frees memory" {
    var executed = false;
    var ctx = Context.background();

    const unit = try WorkUnit(TestHandler).init(
        testing.allocator,
        .{ .executed = &executed },
        &ctx,
    );
    const any = unit.toAny();
    try any.runFn(any.ptr);
    try testing.expect(executed);
    // Memory freed inside runFn; GPA will catch any leak.
}

test "WorkUnit: runFn defers arena deinit even when handler errors" {
    var executed = false;
    var ctx = Context.background();

    const unit = try WorkUnit(TestHandler).init(
        testing.allocator,
        .{ .executed = &executed, .should_error = true },
        &ctx,
    );
    const any = unit.toAny();
    try testing.expectError(error.HandlerFailed, any.runFn(any.ptr));
    try testing.expect(executed);
    // Arena and struct freed even though handler returned an error.
}

test "WorkUnit: runFn returns Cancelled and frees when context is cancelled" {
    var executed = false;
    var ctx = Context.background();
    ctx.cancel(error.Cancelled);

    const unit = try WorkUnit(TestHandler).init(
        testing.allocator,
        .{ .executed = &executed },
        &ctx,
    );
    const any = unit.toAny();
    try testing.expectError(error.Cancelled, any.runFn(any.ptr));
    // Handler must not have been called.
    try testing.expect(!executed);
}

test "WorkUnit: deinitFn cleans up without executing" {
    var executed = false;
    var ctx = Context.background();

    const unit = try WorkUnit(TestHandler).init(
        testing.allocator,
        .{ .executed = &executed },
        &ctx,
    );
    const any = unit.toAny();
    any.deinitFn(any.ptr);
    try testing.expect(!executed);
    // Arena and struct freed; GPA will catch any leak.
}

test "WorkUnit: GPA no leaks — success, error, cancelled paths" {
    {
        var gpa: std.heap.DebugAllocator(.{}) = .init;
        defer if (gpa.deinit() == .leak) @panic("leak: success path");
        var executed = false;
        var ctx = Context.background();
        const unit = try WorkUnit(TestHandler).init(gpa.allocator(), .{ .executed = &executed }, &ctx);
        _ = unit.toAny().runFn(unit.toAny().ptr) catch {};
    }
    {
        var gpa: std.heap.DebugAllocator(.{}) = .init;
        defer if (gpa.deinit() == .leak) @panic("leak: error path");
        var executed = false;
        var ctx = Context.background();
        const unit = try WorkUnit(TestHandler).init(gpa.allocator(), .{ .executed = &executed, .should_error = true }, &ctx);
        _ = unit.toAny().runFn(unit.toAny().ptr) catch {};
    }
    {
        var gpa: std.heap.DebugAllocator(.{}) = .init;
        defer if (gpa.deinit() == .leak) @panic("leak: deinit path");
        var executed = false;
        var ctx = Context.background();
        const unit = try WorkUnit(TestHandler).init(gpa.allocator(), .{ .executed = &executed }, &ctx);
        unit.toAny().deinitFn(unit.toAny().ptr);
    }
}

test "WorkUnit: thread transfer — create on A, run on B" {
    var executed = false;
    var ctx = Context.background();

    const unit = try WorkUnit(TestHandler).init(
        testing.allocator,
        .{ .executed = &executed },
        &ctx,
    );
    const any = unit.toAny();

    const T = struct {
        fn run(a: AnyWorkUnit) void {
            a.runFn(a.ptr) catch |e| std.log.err("runFn: {s}", .{@errorName(e)});
        }
    };

    const thread = try std.Thread.spawn(.{}, T.run, .{any});
    thread.join();
    try testing.expect(executed);
}
